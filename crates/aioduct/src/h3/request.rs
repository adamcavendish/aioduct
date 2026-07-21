use bytes::{Buf, Bytes};
use http::{Request, Uri};
use http_body_util::BodyExt as _;
use std::future::Future as _;
use std::pin::Pin;
use std::task::Poll;

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
}

pub(crate) async fn send_on_h3<R>(
    send_request: &mut super::H3SendRequest,
    request: Request<RequestBodySend>,
    url: Uri,
) -> Result<Response, Error>
where
    R: RuntimePoll,
{
    let (parts, body) = request.into_parts();
    let request = Request::from_parts(parts, ());
    let stream = send_request.send_request(request).await.map_err(h3_error)?;
    let (send, mut recv) = stream.split();

    let (control_sender, control_receiver) = futures_channel::oneshot::channel();
    let (result_sender, mut result_receiver) = futures_channel::oneshot::channel();
    R::spawn_send(supervise_upload(
        send,
        body,
        control_receiver,
        result_sender,
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
                response.map_err(h3_error)?,
                recv,
                url,
                Some(UploadControlGuard::new(control_sender)),
            )
        }
        RequestProgress::Upload(Ok(())) => {
            drop(control_sender);
            // Keep polling the same response future. The upstream h3-quinn
            // receive stream is not cancellation-safe while a read is pending.
            let response = receive_response.as_mut().await.map_err(h3_error)?;
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

async fn supervise_upload(
    mut stream: H3SendStream,
    body: RequestBodySend,
    mut control: futures_channel::oneshot::Receiver<UploadControl>,
    result: futures_channel::oneshot::Sender<Result<(), Error>>,
) {
    let mut upload = Box::pin(upload_body(&mut stream, body));
    let progress = futures_util::future::poll_fn(|context| {
        if let Poll::Ready(result) = upload.as_mut().poll(context) {
            return Poll::Ready(UploadProgress::Complete(result));
        }
        match Pin::new(&mut control).poll(context) {
            Poll::Ready(Ok(control)) => Poll::Ready(UploadProgress::Control(control)),
            Poll::Ready(Err(_)) => Poll::Ready(UploadProgress::Control(UploadControl::Cancel)),
            Poll::Pending => Poll::Pending,
        }
    })
    .await;

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
            let upload_result = upload.await;
            if upload_result.is_err() {
                stream.stop_stream(h3::error::Code::H3_REQUEST_CANCELLED);
            }
            let _ = result.send(upload_result);
        }
    }
}

async fn upload_body<B>(stream: &mut H3SendStream, body: B) -> Result<(), Error>
where
    B: http_body::Body<Data = Bytes, Error = Error>,
{
    let mut body = std::pin::pin!(body);
    // TODO(http3-trailers): validate trailer fields, ordering, timeout, and
    // cancellation end to end before sending trailers through upstream h3.
    while let Some(frame) = body.as_mut().frame().await {
        let frame = frame?;
        match frame.into_data() {
            Ok(data) => {
                if !data.is_empty()
                    && let Err(error) = stream.send_data(data).await
                {
                    if is_h3_no_error_stop_sending(&error) {
                        return Ok(());
                    }
                    return Err(h3_error(error));
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

    if let Err(error) = stream.finish().await
        && !is_h3_no_error_stop_sending(&error)
    {
        return Err(h3_error(error));
    }
    Ok(())
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
                    Err(error) => Some((Err(h3_error(error)), (stream, true, upload_control))),
                },
                Err(error) => Some((Err(h3_error(error)), (stream, true, upload_control))),
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

fn h3_error(error: h3::error::StreamError) -> Error {
    Error::Other(Box::new(error))
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
        let error = h3_error(h3::error::StreamError::RemoteClosing);
        let Error::Other(source) = error else {
            panic!("h3 errors must retain their concrete source");
        };
        assert!(source.downcast_ref::<h3::error::StreamError>().is_some());

        let error = upload_frame_error(UploadFrameError::UnsupportedFrame);
        let Error::Other(source) = error else {
            panic!("unsupported frames must retain their concrete source");
        };
        assert_eq!(
            source.to_string(),
            "HTTP/3 request body emitted an unsupported frame"
        );
    }
}
