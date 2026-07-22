use bytes::{Buf, Bytes};
use http::{Request, Uri};
use http_body_util::BodyExt as _;
use std::future::Future as _;
use std::pin::Pin;
use std::task::Poll;
use std::time::Duration;

use crate::body::RequestBodySend;
use crate::error::{Error, UnsupportedCapability};
use crate::response::Response;
use crate::runtime::RuntimePoll;

type H3SendStream = h3::client::RequestStream<super::quinn_adapter::SendStream<Bytes>, Bytes>;
type H3RecvStream = h3::client::RequestStream<super::quinn_adapter::RecvStream, Bytes>;

#[derive(Debug)]
enum UploadFrameError {
    UnsupportedFrame,
}

impl std::fmt::Display for UploadFrameError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::UnsupportedFrame => "HTTP/3 request body emitted an unsupported frame",
        })
    }
}

impl std::error::Error for UploadFrameError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum H3ReplayEvidence {
    ProvenUnprocessed,
    VersionFallback,
    Ambiguous,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum H3Operation {
    OpenRequest,
    SendData,
    FinishUpload,
    ReceiveResponse,
    ReceiveData,
    ReceiveTrailers,
}

impl std::fmt::Display for H3Operation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::OpenRequest => "open request stream",
            Self::SendData => "send request data",
            Self::FinishUpload => "finish request upload",
            Self::ReceiveResponse => "receive response headers",
            Self::ReceiveData => "receive response data",
            Self::ReceiveTrailers => "receive response trailers",
        })
    }
}

#[derive(Debug)]
struct H3DispatchError {
    operation: H3Operation,
    source: h3::error::StreamError,
}

impl H3DispatchError {
    fn replay_evidence(&self) -> H3ReplayEvidence {
        if is_version_fallback(&self.source) {
            return H3ReplayEvidence::VersionFallback;
        }
        match &self.source {
            h3::error::StreamError::RemoteTerminate { code, .. }
                if *code == h3::error::Code::H3_REQUEST_REJECTED =>
            {
                H3ReplayEvidence::ProvenUnprocessed
            }
            // TODO(http3-goaway-replay): upstream h3 does not expose the
            // validated GOAWAY stream-id boundary. Treat RemoteClosing as
            // ambiguous rather than risking a duplicate request.
            _ => H3ReplayEvidence::Ambiguous,
        }
    }

    fn is_endpoint_failure(&self) -> bool {
        match &self.source {
            h3::error::StreamError::ConnectionError(error) => !error.is_h3_no_error(),
            h3::error::StreamError::Undefined(source) => {
                matches!(
                    source.downcast_ref::<quinn::WriteError>(),
                    Some(quinn::WriteError::ConnectionLost(_))
                ) || matches!(
                    source.downcast_ref::<quinn::ReadError>(),
                    Some(quinn::ReadError::ConnectionLost(_))
                )
            }
            h3::error::StreamError::StreamError { .. }
            | h3::error::StreamError::RemoteTerminate { .. }
            | h3::error::StreamError::HeaderTooBig { .. }
            | h3::error::StreamError::RemoteClosing => false,
            _ => false,
        }
    }

    fn connection_is_unusable(&self) -> bool {
        match &self.source {
            h3::error::StreamError::ConnectionError(_) | h3::error::StreamError::RemoteClosing => {
                true
            }
            h3::error::StreamError::Undefined(source) => {
                matches!(
                    source.downcast_ref::<quinn::WriteError>(),
                    Some(quinn::WriteError::ConnectionLost(_))
                ) || matches!(
                    source.downcast_ref::<quinn::ReadError>(),
                    Some(quinn::ReadError::ConnectionLost(_))
                )
            }
            _ => false,
        }
    }
}

impl std::fmt::Display for H3DispatchError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "HTTP/3 {} failed: {}",
            self.operation, self.source
        )
    }
}

impl std::error::Error for H3DispatchError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum UploadControl {
    Cancel,
    Detach,
}

struct UploadControlGuard {
    sender: Option<futures_channel::oneshot::Sender<UploadControl>>,
}

impl UploadControlGuard {
    fn new(sender: futures_channel::oneshot::Sender<UploadControl>) -> Self {
        Self {
            sender: Some(sender),
        }
    }

    fn detach(&mut self) {
        if let Some(sender) = self.sender.take() {
            let _ = sender.send(UploadControl::Detach);
        }
    }
}

impl Drop for UploadControlGuard {
    fn drop(&mut self) {
        if let Some(sender) = self.sender.take() {
            let _ = sender.send(UploadControl::Cancel);
        }
    }
}

enum RequestProgress {
    Response(Result<http::Response<()>, h3::error::StreamError>),
    Upload(Result<(), Error>),
}

enum UploadProgress {
    Complete(Result<(), Error>),
    Control(UploadControl),
    PeerStopped(Result<Option<quinn::VarInt>, quinn::StoppedError>),
}

pub(crate) async fn send_on_h3<R>(
    connection: &mut super::H3Connection,
    request: Request<RequestBodySend>,
    url: Uri,
    write_timeout: Option<Duration>,
) -> Result<Response, Error>
where
    R: RuntimePoll,
{
    let (parts, body) = request.into_parts();
    let request = Request::from_parts(parts, ());
    let connection_owner = connection.send_request().clone();
    let mut stream = connection
        .send_request()
        .send_request(request)
        .await
        .map_err(|error| h3_error(H3Operation::OpenRequest, error))?;
    let stream_id = stream.id();
    let Some(request_stream) = connection.take_request_stream(stream_id) else {
        stream.stop_stream(h3::error::Code::H3_REQUEST_CANCELLED);
        stream.stop_sending(h3::error::Code::H3_REQUEST_CANCELLED);
        return Err(Error::Other(
            "HTTP/3 request stream transport state was unavailable".into(),
        ));
    };
    let (send, mut recv) = stream.split();

    let (control_sender, control_receiver) = futures_channel::oneshot::channel();
    let (result_sender, mut result_receiver) = futures_channel::oneshot::channel();
    R::spawn_send(supervise_upload::<R>(
        send,
        body,
        request_stream,
        write_timeout,
        control_receiver,
        result_sender,
        connection_owner,
    ));

    let mut receive_response = Box::pin(recv.recv_response());
    let progress = futures_util::future::poll_fn(|context| {
        match Pin::new(&mut result_receiver).poll(context) {
            Poll::Ready(Ok(result)) => return Poll::Ready(RequestProgress::Upload(result)),
            Poll::Ready(Err(_)) => {
                return Poll::Ready(RequestProgress::Upload(Err(Error::Other(Box::new(
                    std::io::Error::other("HTTP/3 upload supervision ended without a result"),
                )))));
            }
            Poll::Pending => {}
        }
        if let Poll::Ready(response) = receive_response.as_mut().poll(context) {
            return Poll::Ready(RequestProgress::Response(response));
        }
        Poll::Pending
    })
    .await;
    match progress {
        RequestProgress::Response(response) => {
            drop(receive_response);
            response_from_stream(
                response.map_err(|error| h3_error(H3Operation::ReceiveResponse, error))?,
                recv,
                url,
                Some(UploadControlGuard::new(control_sender)),
            )
        }
        RequestProgress::Upload(Ok(())) => {
            drop(control_sender);
            // Keep polling the same response future. Restarting a pending read
            // can lose the wakeup that delivers the response headers.
            let response = receive_response
                .as_mut()
                .await
                .map_err(|error| h3_error(H3Operation::ReceiveResponse, error))?;
            drop(receive_response);
            response_from_stream(response, recv, url, None)
        }
        RequestProgress::Upload(Err(error)) => {
            drop(control_sender);
            drop(receive_response);
            Err(error)
        }
    }
}

async fn supervise_upload<R: RuntimePoll>(
    mut stream: H3SendStream,
    body: RequestBodySend,
    request_stream: super::quinn_adapter::RequestStreamState,
    write_timeout: Option<Duration>,
    mut control: futures_channel::oneshot::Receiver<UploadControl>,
    result: futures_channel::oneshot::Sender<Result<(), Error>>,
    connection_owner: super::H3SendRequest,
) {
    let write_progress = request_stream.write_progress();
    let mut peer_stopped = Box::pin(request_stream);
    let mut upload = Box::pin(upload_body::<R, _>(
        &mut stream,
        body,
        &write_progress,
        write_timeout,
    ));
    let mut detached = false;
    let progress = loop {
        let progress = futures_util::future::poll_fn(|context| {
            if let Poll::Ready(result) = upload.as_mut().poll(context) {
                return Poll::Ready(UploadProgress::Complete(result));
            }
            if let Poll::Ready(stopped) = peer_stopped.as_mut().poll(context) {
                return Poll::Ready(UploadProgress::PeerStopped(stopped));
            }
            if detached {
                return Poll::Pending;
            }
            match Pin::new(&mut control).poll(context) {
                Poll::Ready(Ok(control)) => Poll::Ready(UploadProgress::Control(control)),
                Poll::Ready(Err(_)) => Poll::Ready(UploadProgress::Control(UploadControl::Cancel)),
                Poll::Pending => Poll::Pending,
            }
        })
        .await;
        if matches!(progress, UploadProgress::Control(UploadControl::Detach)) {
            detached = true;
            continue;
        }
        break progress;
    };

    match progress {
        UploadProgress::Complete(upload_result) => {
            drop(upload);
            if upload_result.is_err() {
                stream.stop_stream(h3::error::Code::H3_REQUEST_CANCELLED);
            }
            let _ = result.send(upload_result);
        }
        UploadProgress::Control(UploadControl::Cancel) => {
            drop(upload);
            stream.stop_stream(h3::error::Code::H3_REQUEST_CANCELLED);
        }
        UploadProgress::Control(UploadControl::Detach) => {
            unreachable!("detach control is consumed by the supervision loop")
        }
        UploadProgress::PeerStopped(stopped) => {
            drop(upload);
            let stopped = match stopped {
                Ok(Some(code)) => {
                    let code = h3::error::Code::from(code.into_inner());
                    stream.stop_stream(code);
                    if code == h3::error::Code::H3_NO_ERROR {
                        Ok(())
                    } else {
                        Err(h3_error(
                            H3Operation::SendData,
                            h3::error::StreamError::RemoteTerminate { code },
                        ))
                    }
                }
                Ok(None) => Ok(()),
                Err(error) => {
                    stream.stop_stream(h3::error::Code::H3_REQUEST_CANCELLED);
                    Err(h3_stopped_error(error))
                }
            };
            let _ = result.send(stopped);
        }
    }
    // Upstream h3 closes the connection when its final SendRequest owner is
    // dropped. Keep this clone alive until the detached stream has quiesced.
    drop(connection_owner);
}

async fn upload_body<R, B>(
    stream: &mut H3SendStream,
    body: B,
    write_progress: &super::quinn_adapter::WriteProgress,
    write_timeout: Option<Duration>,
) -> Result<(), Error>
where
    R: RuntimePoll,
    B: http_body::Body<Data = Bytes, Error = Error>,
{
    let mut body = std::pin::pin!(body);
    // TODO(http3-trailers): validate trailer fields, ordering, timeout, and
    // cancellation end to end before sending trailers through upstream h3.
    while let Some(frame) = body.as_mut().frame().await {
        let frame = frame?;
        match frame.into_data() {
            Ok(data) => {
                if !data.is_empty() {
                    transport_write::<R, _>(
                        H3Operation::SendData,
                        write_progress,
                        write_timeout,
                        stream.send_data(data),
                    )
                    .await?;
                }
            }
            Err(frame) => {
                let _trailers = frame
                    .into_trailers()
                    .map_err(|_| upload_frame_error(UploadFrameError::UnsupportedFrame))?;
                return Err(UnsupportedCapability::Http3RequestTrailers.into_error());
            }
        }
        yield_once().await;
    }

    transport_write::<R, _>(
        H3Operation::FinishUpload,
        write_progress,
        write_timeout,
        stream.finish(),
    )
    .await?;
    Ok(())
}

async fn transport_write<R, F>(
    operation: H3Operation,
    write_progress: &super::quinn_adapter::WriteProgress,
    write_timeout: Option<Duration>,
    future: F,
) -> Result<(), Error>
where
    R: RuntimePoll,
    F: std::future::Future<Output = Result<(), h3::error::StreamError>>,
{
    let mut future = std::pin::pin!(future);
    let result = if let Some(duration) = write_timeout {
        let mut observed_progress = write_progress.load();
        let mut sleep = Box::pin(R::sleep(duration));
        match futures_util::future::poll_fn(|context| {
            if let Poll::Ready(result) = future.as_mut().poll(context) {
                return Poll::Ready(Ok(result));
            }

            let current_progress = write_progress.load();
            if current_progress != observed_progress {
                observed_progress = current_progress;
                sleep = Box::pin(R::sleep(duration));
            }
            if sleep.as_mut().poll(context).is_ready() {
                return Poll::Ready(Err(Error::WriteTimeout));
            }
            Poll::Pending
        })
        .await
        {
            Ok(result) => result,
            Err(error) => return Err(error),
        }
    } else {
        future.await
    };

    match result {
        Ok(()) => Ok(()),
        Err(error) if is_h3_no_error_stop_sending(&error) => Ok(()),
        Err(error) => Err(h3_error(operation, error)),
    }
}

async fn yield_once() {
    let mut yielded = false;
    futures_util::future::poll_fn(move |context| {
        if yielded {
            Poll::Ready(())
        } else {
            yielded = true;
            context.waker().wake_by_ref();
            Poll::Pending
        }
    })
    .await;
}

fn response_from_stream(
    response: http::Response<()>,
    stream: H3RecvStream,
    url: Uri,
    upload_control: Option<UploadControlGuard>,
) -> Result<Response, Error> {
    let (parts, ()) = response.into_parts();
    let body_stream = futures_util::stream::unfold(
        (stream, false, upload_control),
        |(mut stream, data_done, mut upload_control)| async move {
            if data_done {
                return None;
            }
            match stream.recv_data().await {
                Ok(Some(mut buf)) => {
                    let remaining = buf.remaining();
                    let bytes = buf.copy_to_bytes(remaining);
                    Some((
                        Ok::<_, Error>(hyper::body::Frame::data(bytes)),
                        (stream, false, upload_control),
                    ))
                }
                Ok(None) => match stream.recv_trailers().await {
                    // TODO(http3-trailers): expose trailers only after aioduct
                    // validates trailer fields, ordering, timeout, and
                    // cancellation end to end. Until then, fail closed.
                    Ok(Some(_)) => Some((
                        Err(Error::Unsupported(
                            "HTTP/3 response trailers are not supported by aioduct".to_owned(),
                        )),
                        (stream, true, upload_control),
                    )),
                    Ok(None) => {
                        if let Some(control) = upload_control.as_mut() {
                            control.detach();
                        }
                        None
                    }
                    Err(error) => Some((
                        Err(h3_error(H3Operation::ReceiveTrailers, error)),
                        (stream, true, upload_control),
                    )),
                },
                Err(error) => Some((
                    Err(h3_error(H3Operation::ReceiveData, error)),
                    (stream, true, upload_control),
                )),
            }
        },
    );

    let body: RequestBodySend = http_body_util::StreamBody::new(body_stream).boxed_unsync();
    Ok(Response::from_boxed(
        http::Response::from_parts(parts, body),
        url,
    ))
}

fn is_h3_no_error_stop_sending(error: &h3::error::StreamError) -> bool {
    matches!(
        error,
        h3::error::StreamError::RemoteTerminate {
            code: h3::error::Code::H3_NO_ERROR,
            ..
        }
    )
}

fn is_version_fallback(error: &h3::error::StreamError) -> bool {
    matches!(
        error,
        h3::error::StreamError::ConnectionError(h3::error::ConnectionError::Remote(
            h3::quic::ConnectionErrorIncoming::ApplicationClose { error_code }
        )) if *error_code == h3::error::Code::H3_VERSION_FALLBACK.value()
    )
}

pub(crate) fn replay_evidence(error: &Error) -> Option<H3ReplayEvidence> {
    let mut source: Option<&(dyn std::error::Error + 'static)> = Some(error);
    while let Some(error) = source {
        if let Some(error) = error.downcast_ref::<H3DispatchError>() {
            return Some(error.replay_evidence());
        }
        source = error.source();
    }
    None
}

pub(crate) fn is_endpoint_failure(error: &Error) -> bool {
    let mut source: Option<&(dyn std::error::Error + 'static)> = Some(error);
    while let Some(error) = source {
        if let Some(error) = error.downcast_ref::<H3DispatchError>() {
            return error.is_endpoint_failure();
        }
        source = error.source();
    }
    false
}

pub(crate) fn connection_is_unusable(error: &Error) -> bool {
    let mut source: Option<&(dyn std::error::Error + 'static)> = Some(error);
    while let Some(error) = source {
        if let Some(error) = error.downcast_ref::<H3DispatchError>() {
            return error.connection_is_unusable();
        }
        source = error.source();
    }
    false
}

fn h3_error(operation: H3Operation, error: h3::error::StreamError) -> Error {
    Error::Other(Box::new(H3DispatchError {
        operation,
        source: error,
    }))
}

fn h3_stopped_error(error: quinn::StoppedError) -> Error {
    h3_error(
        H3Operation::SendData,
        h3::error::StreamError::Undefined(Box::new(quinn::WriteError::from(error))),
    )
}

fn upload_frame_error(error: UploadFrameError) -> Error {
    Error::Other(Box::new(error))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stop_sending_classifier_only_accepts_h3_no_error() {
        let no_error = h3::error::StreamError::RemoteTerminate {
            code: h3::error::Code::H3_NO_ERROR,
        };
        let cancelled = h3::error::StreamError::RemoteTerminate {
            code: h3::error::Code::H3_REQUEST_CANCELLED,
        };

        assert!(is_h3_no_error_stop_sending(&no_error));
        assert!(!is_h3_no_error_stop_sending(&cancelled));
    }

    #[test]
    fn helper_errors_preserve_their_sources() {
        let error = h3_error(
            H3Operation::ReceiveResponse,
            h3::error::StreamError::RemoteClosing,
        );
        let Error::Other(source) = error else {
            panic!("h3 errors must retain their concrete source");
        };
        let dispatch = source
            .downcast_ref::<H3DispatchError>()
            .expect("h3 errors must retain dispatch context");
        assert_eq!(dispatch.operation, H3Operation::ReceiveResponse);
        assert!(matches!(
            &dispatch.source,
            h3::error::StreamError::RemoteClosing
        ));

        let error = upload_frame_error(UploadFrameError::UnsupportedFrame);
        let Error::Other(source) = error else {
            panic!("unsupported frames must retain their concrete source");
        };
        assert_eq!(
            source.to_string(),
            "HTTP/3 request body emitted an unsupported frame"
        );
    }

    #[test]
    fn goaway_is_ambiguous_without_a_validated_stream_boundary() {
        for operation in [H3Operation::OpenRequest, H3Operation::ReceiveResponse] {
            let error = H3DispatchError {
                operation,
                source: h3::error::StreamError::RemoteClosing,
            };
            assert_eq!(error.replay_evidence(), H3ReplayEvidence::Ambiguous);
            assert!(error.connection_is_unusable());
            assert!(!error.is_endpoint_failure());
        }
    }

    #[test]
    fn request_rejected_is_proven_unprocessed() {
        let error = H3DispatchError {
            operation: H3Operation::ReceiveResponse,
            source: h3::error::StreamError::RemoteTerminate {
                code: h3::error::Code::H3_REQUEST_REJECTED,
            },
        };
        assert_eq!(error.replay_evidence(), H3ReplayEvidence::ProvenUnprocessed);
    }

    #[test]
    fn version_fallback_is_distinct_replay_evidence() {
        let error = H3DispatchError {
            operation: H3Operation::ReceiveResponse,
            source: h3::error::StreamError::ConnectionError(h3::error::ConnectionError::Remote(
                h3::quic::ConnectionErrorIncoming::ApplicationClose {
                    error_code: h3::error::Code::H3_VERSION_FALLBACK.value(),
                },
            )),
        };
        assert_eq!(error.replay_evidence(), H3ReplayEvidence::VersionFallback);
        assert!(error.is_endpoint_failure());
    }

    #[test]
    fn request_scoped_failures_do_not_poison_the_endpoint() {
        for source in [
            h3::error::StreamError::RemoteClosing,
            h3::error::StreamError::RemoteTerminate {
                code: h3::error::Code::H3_REQUEST_REJECTED,
            },
        ] {
            let error = H3DispatchError {
                operation: H3Operation::ReceiveResponse,
                source,
            };
            assert!(!error.is_endpoint_failure());
            assert_eq!(
                error.connection_is_unusable(),
                matches!(error.source, h3::error::StreamError::RemoteClosing)
            );
        }
    }

    #[test]
    fn connection_scoped_failure_poisons_the_endpoint() {
        let error = H3DispatchError {
            operation: H3Operation::ReceiveResponse,
            source: h3::error::StreamError::ConnectionError(h3::error::ConnectionError::Timeout),
        };
        assert!(error.is_endpoint_failure());
    }
}
