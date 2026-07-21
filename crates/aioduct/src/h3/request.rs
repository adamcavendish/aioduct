use bytes::{Buf, Bytes};
use http::{Request, Uri};
use http_body_util::BodyExt as _;

use crate::body::RequestBodySend;
use crate::error::{Error, UnsupportedCapability};
use crate::response::Response;

type H3SendStream = h3::client::RequestStream<h3_quinn::SendStream<Bytes>, Bytes>;
type H3RecvStream = h3::client::RequestStream<h3_quinn::RecvStream, Bytes>;

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

pub(crate) async fn send_on_h3<B>(
    send_request: &mut h3::client::SendRequest<h3_quinn::OpenStreams, Bytes>,
    request: Request<B>,
    url: Uri,
) -> Result<Response, Error>
where
    B: http_body::Body<Data = Bytes, Error = Error>,
{
    let (parts, body) = request.into_parts();
    let request = Request::from_parts(parts, ());
    let stream = send_request.send_request(request).await.map_err(h3_error)?;
    let (mut send, mut recv) = stream.split();

    upload_body(&mut send, body).await?;
    let response = recv.recv_response().await.map_err(h3_error)?;

    response_from_stream(response, recv, url)
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
    }

    if let Err(error) = stream.finish().await
        && !is_h3_no_error_stop_sending(&error)
    {
        return Err(h3_error(error));
    }
    Ok(())
}

fn response_from_stream(
    response: http::Response<()>,
    stream: H3RecvStream,
    url: Uri,
) -> Result<Response, Error> {
    let (parts, ()) = response.into_parts();
    let body_stream =
        futures_util::stream::unfold((stream, false), |(mut stream, data_done)| async move {
            if data_done {
                return None;
            }
            match stream.recv_data().await {
                Ok(Some(mut buf)) => {
                    let remaining = buf.remaining();
                    let bytes = buf.copy_to_bytes(remaining);
                    Some((
                        Ok::<_, Error>(hyper::body::Frame::data(bytes)),
                        (stream, false),
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
                        (stream, true),
                    )),
                    Ok(None) => None,
                    Err(error) => Some((Err(h3_error(error)), (stream, true))),
                },
                Err(error) => Some((Err(h3_error(error)), (stream, true))),
            }
        });

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
