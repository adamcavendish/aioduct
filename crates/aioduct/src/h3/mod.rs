use std::sync::Arc;

use bytes::{Buf, Bytes};
use http::{Request, Uri};
use http_body_util::BodyExt;

use crate::error::{AioductBody, Error};
use crate::pool::PooledConnection;
use crate::response::Response;
use crate::runtime::Runtime;

pub(crate) async fn connect_h3<R: Runtime>(
    quinn_conn: quinn::Connection,
) -> Result<PooledConnection<R>, Error> {
    let h3_conn = h3_quinn::Connection::new(quinn_conn);
    let (mut driver, send_request) = h3::client::new(h3_conn)
        .await
        .map_err(|e| Error::Other(Box::new(e)))?;

    R::spawn(async move {
        let _ = futures_util::future::poll_fn(|cx| driver.poll_close(cx)).await;
    });

    Ok(PooledConnection::new_h3(send_request))
}

pub(crate) async fn send_on_h3(
    send_request: &mut h3::client::SendRequest<h3_quinn::OpenStreams, Bytes>,
    request: Request<AioductBody>,
    url: Uri,
) -> Result<Response, Error> {
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

    let hyper_body: AioductBody = http_body_util::StreamBody::new(body_stream).boxed();
    let http_resp = http::Response::from_parts(resp_parts, hyper_body);

    Ok(Response::from_boxed(http_resp, url))
}

fn ensure_h3_alpn(config: Arc<rustls::ClientConfig>) -> Arc<rustls::ClientConfig> {
    if config.alpn_protocols.iter().any(|p| p == b"h3") {
        return config;
    }
    let mut config = (*config).clone();
    config.alpn_protocols.insert(0, b"h3".to_vec());
    Arc::new(config)
}

pub(crate) fn build_quinn_endpoint(
    tls_config: Arc<rustls::ClientConfig>,
    local_address: Option<std::net::IpAddr>,
) -> Result<quinn::Endpoint, Error> {
    let mut transport_config = quinn::TransportConfig::default();
    transport_config.keep_alive_interval(Some(std::time::Duration::from_secs(15)));

    let tls_config = ensure_h3_alpn(tls_config);
    let quic_config = quinn::crypto::rustls::QuicClientConfig::try_from(tls_config)
        .map_err(|e| Error::Tls(Box::new(e)))?;

    let mut client_config = quinn::ClientConfig::new(Arc::new(quic_config));
    client_config.transport_config(Arc::new(transport_config));

    let bind_addr: std::net::SocketAddr = match local_address {
        Some(ip) => std::net::SocketAddr::new(ip, 0),
        None => "0.0.0.0:0".parse().unwrap(),
    };
    let mut endpoint = quinn::Endpoint::client(bind_addr).map_err(Error::Io)?;
    endpoint.set_default_client_config(client_config);

    Ok(endpoint)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn make_rustls_config(alpn: &[&[u8]]) -> Arc<rustls::ClientConfig> {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let mut config = rustls::ClientConfig::builder()
            .with_root_certificates(rustls::RootCertStore::from_iter(
                webpki_roots::TLS_SERVER_ROOTS.iter().cloned(),
            ))
            .with_no_client_auth();
        config.alpn_protocols = alpn.iter().map(|p| p.to_vec()).collect();
        Arc::new(config)
    }

    #[test]
    fn ensure_h3_alpn_adds_h3_when_missing() {
        let config = make_rustls_config(&[b"h2", b"http/1.1"]);
        let result = ensure_h3_alpn(config);
        assert_eq!(result.alpn_protocols[0], b"h3");
        assert_eq!(result.alpn_protocols[1], b"h2");
        assert_eq!(result.alpn_protocols[2], b"http/1.1");
    }

    #[test]
    fn ensure_h3_alpn_preserves_existing_h3() {
        let config = make_rustls_config(&[b"h3", b"h2"]);
        let original_ptr = Arc::as_ptr(&config);
        let result = ensure_h3_alpn(config);
        assert_eq!(Arc::as_ptr(&result), original_ptr);
    }

    #[test]
    fn ensure_h3_alpn_adds_h3_to_empty_list() {
        let config = make_rustls_config(&[]);
        let result = ensure_h3_alpn(config);
        assert_eq!(result.alpn_protocols, vec![b"h3".to_vec()]);
    }

    #[test]
    fn ensure_h3_alpn_does_not_duplicate() {
        let config = make_rustls_config(&[b"h2", b"h3", b"http/1.1"]);
        let result = ensure_h3_alpn(config);
        assert_eq!(result.alpn_protocols.len(), 3);
        assert!(result.alpn_protocols.contains(&b"h3".to_vec()));
    }

    #[test]
    fn h3_alpn_is_first_in_list() {
        let config = make_rustls_config(&[b"h2", b"http/1.1"]);
        let result = ensure_h3_alpn(config);
        assert_eq!(result.alpn_protocols[0], b"h3");
    }
}
