use bytes::{Buf, Bytes};
use http::{Request, Uri};
use http_body_util::BodyExt as _;

use crate::body::RequestBodySend;
use crate::error::Error;
use crate::response::Response;

type H3SendStream = h3::client::RequestStream<h3_quinn::SendStream<Bytes>, Bytes>;
type H3RecvStream = h3::client::RequestStream<h3_quinn::RecvStream, Bytes>;

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
    while let Some(frame) = body.as_mut().frame().await {
        let frame = frame?;
        if let Ok(data) = frame.into_data()
            && !data.is_empty()
            && let Err(error) = stream.send_data(data).await
        {
            if is_h3_no_error_stop_sending(&error) {
                return Ok(());
            }
            return Err(h3_error(error));
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
                    Ok(Some(trailers)) => {
                        Some((Ok(hyper::body::Frame::trailers(trailers)), (stream, true)))
                    }
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
