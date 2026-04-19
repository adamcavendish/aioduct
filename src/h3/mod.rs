use std::sync::Arc;

use bytes::{Buf, Bytes};
use http::Request;
use http_body_util::BodyExt;

use crate::error::{Error, HyperBody, Result};
use crate::response::Response;
use crate::runtime::Runtime;

pub(crate) async fn send_h3_request<R: Runtime>(
    quinn_conn: quinn::Connection,
    request: Request<HyperBody>,
) -> Result<Response> {
    let h3_conn = h3_quinn::Connection::new(quinn_conn);
    let (mut driver, mut send_request) = h3::client::new(h3_conn)
        .await
        .map_err(|e| Error::Other(Box::new(e)))?;

    R::spawn(async move {
        let _ = futures_util::future::poll_fn(|cx| driver.poll_close(cx)).await;
    });

    let (parts, body) = request.into_parts();
    let head_req = Request::from_parts(parts, ());

    let mut stream = send_request
        .send_request(head_req)
        .await
        .map_err(|e| Error::Other(Box::new(e)))?;

    let body_bytes = body
        .collect()
        .await
        .map_err(|e| Error::Other(Box::new(e)))?
        .to_bytes();

    if !body_bytes.is_empty() {
        stream
            .send_data(body_bytes)
            .await
            .map_err(|e| Error::Other(Box::new(e)))?;
    }

    stream
        .finish()
        .await
        .map_err(|e| Error::Other(Box::new(e)))?;

    let resp = stream
        .recv_response()
        .await
        .map_err(|e| Error::Other(Box::new(e)))?;

    let (resp_parts, _) = resp.into_parts();

    let body_stream = futures_util::stream::unfold(stream, |mut s| async move {
        match s.recv_data().await {
            Ok(Some(buf)) => {
                let bytes = Bytes::copy_from_slice(buf.chunk());
                Some((Ok::<_, Error>(hyper::body::Frame::data(bytes)), s))
            }
            Ok(None) => None,
            Err(e) => Some((Err(Error::Other(Box::new(e))), s)),
        }
    });

    let hyper_body: HyperBody = http_body_util::StreamBody::new(body_stream).boxed();
    let http_resp = http::Response::from_parts(resp_parts, hyper_body);

    Ok(Response::new(http_resp))
}

pub(crate) fn build_quinn_endpoint(
    tls_config: Arc<rustls::ClientConfig>,
) -> Result<quinn::Endpoint> {
    let mut transport_config = quinn::TransportConfig::default();
    transport_config.keep_alive_interval(Some(std::time::Duration::from_secs(15)));

    let quic_config = quinn::crypto::rustls::QuicClientConfig::try_from(tls_config)
        .map_err(|e| Error::Tls(Box::new(e)))?;

    let mut client_config = quinn::ClientConfig::new(Arc::new(quic_config));
    client_config.transport_config(Arc::new(transport_config));

    let mut endpoint = quinn::Endpoint::client("0.0.0.0:0".parse().unwrap()).map_err(Error::Io)?;
    endpoint.set_default_client_config(client_config);

    Ok(endpoint)
}
