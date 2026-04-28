use std::sync::Arc;

use bytes::{Buf, Bytes};
use http::{Request, Uri};
use http_body_util::BodyExt;

use crate::error::{AioductBody, Error};
use crate::pool::PooledConnection;
use crate::response::Response;
use crate::runtime::Runtime;

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

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

pub(crate) async fn connect_h3_addrs<R: Runtime>(
    endpoint: &quinn::Endpoint,
    addrs: &[SocketAddr],
    server_name: &str,
    local_address: Option<IpAddr>,
) -> Result<(PooledConnection<R>, SocketAddr), Error> {
    let endpoint_addr = endpoint.local_addr().map_err(Error::Io)?;
    let addrs = ordered_h3_addrs(addrs, local_address, endpoint_addr.ip());
    if addrs.is_empty() {
        return Err(Error::InvalidUrl(
            "no compatible HTTP/3 addresses found".into(),
        ));
    }

    let mut last_err = None;
    for addr in addrs {
        match endpoint.connect(addr, server_name) {
            Ok(connecting) => match connecting.await {
                Ok(quinn_conn) => match connect_h3::<R>(quinn_conn).await {
                    Ok(pooled) => return Ok((pooled, addr)),
                    Err(err) => last_err = Some(err),
                },
                Err(err) => last_err = Some(Error::Other(Box::new(err))),
            },
            Err(err) => last_err = Some(Error::Other(Box::new(err))),
        }
    }

    Err(last_err.unwrap_or_else(|| Error::Other("failed to establish HTTP/3 connection".into())))
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

    let mut request_body_stopped = false;
    if !body_bytes.is_empty()
        && let Err(err) = stream.send_data(body_bytes).await
    {
        if is_h3_no_error_stop_sending(&err) {
            request_body_stopped = true;
        } else {
            return Err(Error::Other(Box::new(err)));
        }
    }

    if !request_body_stopped
        && let Err(err) = stream.finish().await
        && !is_h3_no_error_stop_sending(&err)
    {
        return Err(Error::Other(Box::new(err)));
    }

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

fn is_h3_no_error_stop_sending(error: &h3::error::StreamError) -> bool {
    matches!(
        error,
        h3::error::StreamError::RemoteTerminate {
            code: h3::error::Code::H3_NO_ERROR,
            ..
        }
    )
}

fn h3_bind_addr(local_address: Option<IpAddr>) -> SocketAddr {
    SocketAddr::new(
        local_address.unwrap_or(IpAddr::V6(Ipv6Addr::UNSPECIFIED)),
        0,
    )
}

fn h3_ipv4_bind_addr() -> SocketAddr {
    SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0)
}

fn ordered_h3_addrs(
    addrs: &[SocketAddr],
    local_address: Option<IpAddr>,
    endpoint_ip: IpAddr,
) -> Vec<SocketAddr> {
    if let Some(local_ip) = local_address {
        return addrs
            .iter()
            .copied()
            .filter(|addr| addr.is_ipv4() == local_ip.is_ipv4())
            .collect();
    }

    let (ipv6_addrs, ipv4_addrs): (Vec<_>, Vec<_>) =
        addrs.iter().copied().partition(|addr| addr.is_ipv6());

    if endpoint_ip.is_ipv6() {
        ipv6_addrs.into_iter().chain(ipv4_addrs).collect()
    } else {
        ipv4_addrs
    }
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

    let bind_addr = h3_bind_addr(local_address);
    let mut endpoint = match quinn::Endpoint::client(bind_addr) {
        Ok(endpoint) => endpoint,
        Err(err) if local_address.is_none() && bind_addr.is_ipv6() => {
            #[cfg(feature = "tracing")]
            tracing::debug!(error = %err, "h3.endpoint.ipv6_bind_fallback");
            #[cfg(not(feature = "tracing"))]
            let _ = err;
            quinn::Endpoint::client(h3_ipv4_bind_addr()).map_err(Error::Io)?
        }
        Err(err) => return Err(Error::Io(err)),
    };
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

    #[test]
    fn h3_bind_addr_defaults_to_ipv6_unspecified() {
        assert_eq!(
            h3_bind_addr(None),
            SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), 0)
        );
    }

    #[test]
    fn h3_bind_addr_preserves_explicit_local_address() {
        let local = IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1));

        assert_eq!(h3_bind_addr(Some(local)), SocketAddr::new(local, 0));
    }

    #[test]
    fn ordered_h3_addrs_prefers_ipv6_on_dual_stack_endpoint() {
        let ipv4 = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1)), 443);
        let ipv6 = SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), 443);

        let addrs = ordered_h3_addrs(&[ipv4, ipv6], None, IpAddr::V6(Ipv6Addr::UNSPECIFIED));

        assert_eq!(addrs, vec![ipv6, ipv4]);
    }

    #[test]
    fn ordered_h3_addrs_filters_ipv6_for_ipv4_endpoint() {
        let ipv4 = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1)), 443);
        let ipv6 = SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), 443);

        let addrs = ordered_h3_addrs(&[ipv6, ipv4], None, IpAddr::V4(Ipv4Addr::UNSPECIFIED));

        assert_eq!(addrs, vec![ipv4]);
    }

    #[test]
    fn ordered_h3_addrs_honors_explicit_ipv4_local_address() {
        let ipv4 = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1)), 443);
        let ipv6 = SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), 443);

        let addrs = ordered_h3_addrs(
            &[ipv6, ipv4],
            Some(IpAddr::V4(Ipv4Addr::LOCALHOST)),
            IpAddr::V6(Ipv6Addr::UNSPECIFIED),
        );

        assert_eq!(addrs, vec![ipv4]);
    }

    #[test]
    fn ordered_h3_addrs_honors_explicit_ipv6_local_address() {
        let ipv4 = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1)), 443);
        let ipv6 = SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), 443);

        let addrs = ordered_h3_addrs(
            &[ipv4, ipv6],
            Some(IpAddr::V6(Ipv6Addr::LOCALHOST)),
            IpAddr::V6(Ipv6Addr::UNSPECIFIED),
        );

        assert_eq!(addrs, vec![ipv6]);
    }
}
