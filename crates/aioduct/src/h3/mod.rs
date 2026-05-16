use std::sync::Arc;

use bytes::{Buf, Bytes};
use http::{Request, Uri};
use http_body_util::BodyExt;

use crate::body::RequestBoxBody;
use crate::error::Error;
use crate::pool::PooledConnection;
use crate::response::Response;
use crate::runtime::RuntimePoll;

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::pin::Pin;
use std::task::{Context, Poll};

use crate::happy_eyeballs::{HAPPY_EYEBALLS_DELAY, interleave_addrs};

pub(crate) async fn connect_h3<R: RuntimePoll>(
    quinn_conn: quinn::Connection,
) -> Result<PooledConnection<RequestBoxBody>, Error> {
    let h3_conn = h3_quinn::Connection::new(quinn_conn);
    let (mut driver, send_request) = h3::client::new(h3_conn)
        .await
        .map_err(|e| Error::Other(Box::new(e)))?;

    R::spawn_send(async move {
        let _ = futures_util::future::poll_fn(|cx| driver.poll_close(cx)).await;
    });

    Ok(PooledConnection::new_h3(send_request))
}

pub(crate) async fn connect_h3_addrs<R: RuntimePoll>(
    endpoint: &quinn::Endpoint,
    addrs: &[SocketAddr],
    server_name: &str,
    local_address: Option<IpAddr>,
) -> Result<(PooledConnection<RequestBoxBody>, SocketAddr), Error> {
    let addrs = h3_candidate_addrs(endpoint, addrs, local_address)?;

    let (quinn_conn, addr) = race_quic_connect::<R>(endpoint, &addrs, server_name).await?;
    let pooled = connect_h3::<R>(quinn_conn).await?;
    Ok((pooled, addr))
}

pub(crate) async fn connect_h3_addrs_0rtt<R: RuntimePoll>(
    endpoint: &quinn::Endpoint,
    addrs: &[SocketAddr],
    server_name: &str,
    local_address: Option<IpAddr>,
) -> Result<(PooledConnection<RequestBoxBody>, SocketAddr, bool), Error> {
    let addrs = h3_candidate_addrs(endpoint, addrs, local_address)?;

    let (quinn_conn, addr, used_0rtt) =
        race_quic_connect_0rtt::<R>(endpoint, &addrs, server_name).await?;
    let pooled = connect_h3::<R>(quinn_conn).await?;
    Ok((pooled, addr, used_0rtt))
}

fn h3_candidate_addrs(
    endpoint: &quinn::Endpoint,
    addrs: &[SocketAddr],
    local_address: Option<IpAddr>,
) -> Result<Vec<SocketAddr>, Error> {
    let endpoint_addr = endpoint.local_addr().map_err(Error::Io)?;
    let filtered = filter_h3_addrs(addrs, local_address, endpoint_addr.ip());
    if filtered.is_empty() {
        return Err(Error::InvalidUrl(
            "no compatible HTTP/3 addresses found".into(),
        ));
    }
    Ok(filtered)
}

fn filter_h3_addrs(
    addrs: &[SocketAddr],
    local_address: Option<IpAddr>,
    endpoint_ip: IpAddr,
) -> Vec<SocketAddr> {
    if let Some(local_ip) = local_address {
        addrs
            .iter()
            .copied()
            .filter(|a| a.is_ipv4() == local_ip.is_ipv4())
            .collect()
    } else if endpoint_ip.is_ipv6() {
        interleave_addrs(addrs)
    } else {
        addrs.iter().copied().filter(|a| a.is_ipv4()).collect()
    }
}

// ── Happy Eyeballs for QUIC ────────────────────────────────────────────────

enum H3ConnectResult {
    Connected(quinn::Connection, SocketAddr),
    Failed(Error),
    DeadlineReached,
}

async fn race_quic_connect<R: RuntimePoll>(
    endpoint: &quinn::Endpoint,
    addrs: &[SocketAddr],
    server_name: &str,
) -> Result<(quinn::Connection, SocketAddr), Error> {
    if addrs.len() == 1 {
        return quic_connect_one(endpoint, addrs[0], server_name).await;
    }

    let mut last_err = Error::Other("failed to establish HTTP/3 connection".into());
    for (i, &addr) in addrs.iter().enumerate() {
        let is_last = i == addrs.len() - 1;
        if is_last {
            match quic_connect_one(endpoint, addr, server_name).await {
                Ok(result) => return Ok(result),
                Err(e) => last_err = e,
            }
        } else {
            match quic_connect_with_deadline::<R>(endpoint, addr, server_name).await {
                H3ConnectResult::Connected(conn, addr) => return Ok((conn, addr)),
                H3ConnectResult::Failed(e) => last_err = e,
                H3ConnectResult::DeadlineReached => {}
            }
        }
    }
    Err(last_err)
}

async fn quic_connect_one(
    endpoint: &quinn::Endpoint,
    addr: SocketAddr,
    server_name: &str,
) -> Result<(quinn::Connection, SocketAddr), Error> {
    let connecting = endpoint
        .connect(addr, server_name)
        .map_err(|e| Error::Other(Box::new(e)))?;
    let conn = connecting.await.map_err(|e| Error::Other(Box::new(e)))?;
    Ok((conn, addr))
}

async fn quic_connect_with_deadline<R: RuntimePoll>(
    endpoint: &quinn::Endpoint,
    addr: SocketAddr,
    server_name: &str,
) -> H3ConnectResult {
    let connecting = match endpoint.connect(addr, server_name) {
        Ok(c) => c,
        Err(e) => return H3ConnectResult::Failed(Error::Other(Box::new(e))),
    };
    SelectQuicConnect {
        connect: Box::pin(async move { connecting.await.map_err(|e| Error::Other(Box::new(e))) }),
        sleep: Box::pin(R::sleep(HAPPY_EYEBALLS_DELAY)),
        addr,
        done: false,
    }
    .await
}

struct SelectQuicConnect {
    connect: Pin<Box<dyn std::future::Future<Output = Result<quinn::Connection, Error>> + Send>>,
    sleep: Pin<Box<dyn std::future::Future<Output = ()> + Send>>,
    addr: SocketAddr,
    done: bool,
}

impl std::future::Future for SelectQuicConnect {
    type Output = H3ConnectResult;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = unsafe { self.get_unchecked_mut() };

        if this.done {
            return Poll::Pending;
        }

        if let Poll::Ready(result) = this.connect.as_mut().poll(cx) {
            this.done = true;
            return Poll::Ready(match result {
                Ok(conn) => H3ConnectResult::Connected(conn, this.addr),
                Err(e) => H3ConnectResult::Failed(e),
            });
        }

        if let Poll::Ready(()) = this.sleep.as_mut().poll(cx) {
            this.done = true;
            return Poll::Ready(H3ConnectResult::DeadlineReached);
        }

        Poll::Pending
    }
}

// ── Happy Eyeballs for QUIC 0-RTT ─────────────────────────────────────────

enum H3ConnectResult0rtt {
    Connected(quinn::Connection, SocketAddr, bool),
    Failed(Error),
    DeadlineReached,
}

async fn race_quic_connect_0rtt<R: RuntimePoll>(
    endpoint: &quinn::Endpoint,
    addrs: &[SocketAddr],
    server_name: &str,
) -> Result<(quinn::Connection, SocketAddr, bool), Error> {
    if addrs.len() == 1 {
        return quic_connect_one_0rtt(endpoint, addrs[0], server_name).await;
    }

    let mut last_err = Error::Other("failed to establish HTTP/3 connection".into());
    for (i, &addr) in addrs.iter().enumerate() {
        let is_last = i == addrs.len() - 1;
        if is_last {
            match quic_connect_one_0rtt(endpoint, addr, server_name).await {
                Ok(result) => return Ok(result),
                Err(e) => last_err = e,
            }
        } else {
            match quic_connect_0rtt_with_deadline::<R>(endpoint, addr, server_name).await {
                H3ConnectResult0rtt::Connected(conn, addr, used_0rtt) => {
                    return Ok((conn, addr, used_0rtt));
                }
                H3ConnectResult0rtt::Failed(e) => last_err = e,
                H3ConnectResult0rtt::DeadlineReached => {}
            }
        }
    }
    Err(last_err)
}

async fn quic_connect_one_0rtt(
    endpoint: &quinn::Endpoint,
    addr: SocketAddr,
    server_name: &str,
) -> Result<(quinn::Connection, SocketAddr, bool), Error> {
    let connecting = endpoint
        .connect(addr, server_name)
        .map_err(|e| Error::Other(Box::new(e)))?;
    match connecting.into_0rtt() {
        Ok((conn, _zero_rtt_accepted)) => Ok((conn, addr, true)),
        Err(connecting) => {
            let conn = connecting.await.map_err(|e| Error::Other(Box::new(e)))?;
            Ok((conn, addr, false))
        }
    }
}

async fn quic_connect_0rtt_with_deadline<R: RuntimePoll>(
    endpoint: &quinn::Endpoint,
    addr: SocketAddr,
    server_name: &str,
) -> H3ConnectResult0rtt {
    let connecting = match endpoint.connect(addr, server_name) {
        Ok(c) => c,
        Err(e) => return H3ConnectResult0rtt::Failed(Error::Other(Box::new(e))),
    };
    SelectQuicConnect0rtt {
        connect: Box::pin(async move {
            match connecting.into_0rtt() {
                Ok((conn, _zero_rtt_accepted)) => Ok((conn, true)),
                Err(connecting) => {
                    let conn = connecting.await.map_err(|e| Error::Other(Box::new(e)))?;
                    Ok((conn, false))
                }
            }
        }),
        sleep: Box::pin(R::sleep(HAPPY_EYEBALLS_DELAY)),
        addr,
        done: false,
    }
    .await
}

#[allow(clippy::type_complexity)]
struct SelectQuicConnect0rtt {
    connect:
        Pin<Box<dyn std::future::Future<Output = Result<(quinn::Connection, bool), Error>> + Send>>,
    sleep: Pin<Box<dyn std::future::Future<Output = ()> + Send>>,
    addr: SocketAddr,
    done: bool,
}

impl std::future::Future for SelectQuicConnect0rtt {
    type Output = H3ConnectResult0rtt;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = unsafe { self.get_unchecked_mut() };

        if this.done {
            return Poll::Pending;
        }

        if let Poll::Ready(result) = this.connect.as_mut().poll(cx) {
            this.done = true;
            return Poll::Ready(match result {
                Ok((conn, used_0rtt)) => H3ConnectResult0rtt::Connected(conn, this.addr, used_0rtt),
                Err(e) => H3ConnectResult0rtt::Failed(e),
            });
        }

        if let Poll::Ready(()) = this.sleep.as_mut().poll(cx) {
            this.done = true;
            return Poll::Ready(H3ConnectResult0rtt::DeadlineReached);
        }

        Poll::Pending
    }
}

pub(crate) async fn send_on_h3(
    send_request: &mut h3::client::SendRequest<h3_quinn::OpenStreams, Bytes>,
    request: Request<RequestBoxBody>,
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

    let hyper_body: RequestBoxBody = http_body_util::StreamBody::new(body_stream).boxed_unsync();
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

pub(crate) fn build_quinn_endpoint(
    tls_config: Arc<rustls::ClientConfig>,
    local_address: Option<std::net::IpAddr>,
    enable_0rtt: bool,
) -> Result<quinn::Endpoint, Error> {
    let mut transport_config = quinn::TransportConfig::default();
    transport_config.keep_alive_interval(Some(std::time::Duration::from_secs(15)));

    let tls_config = ensure_h3_alpn(tls_config);
    let tls_config = if enable_0rtt {
        let mut config = (*tls_config).clone();
        config.enable_early_data = true;
        Arc::new(config)
    } else {
        tls_config
    };
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
        let mut config = rustls::ClientConfig::builder_with_provider(crate::tls::crypto_provider())
            .with_safe_default_protocol_versions()
            .expect("configured rustls provider does not support the default TLS versions")
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
    fn filter_h3_addrs_interleaves_on_ipv6_endpoint() {
        let ipv4 = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1)), 443);
        let ipv6 = SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), 443);

        let addrs = filter_h3_addrs(&[ipv4, ipv6], None, IpAddr::V6(Ipv6Addr::UNSPECIFIED));

        assert_eq!(addrs, vec![ipv6, ipv4]);
    }

    #[test]
    fn filter_h3_addrs_filters_ipv6_for_ipv4_endpoint() {
        let ipv4 = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1)), 443);
        let ipv6 = SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), 443);

        let addrs = filter_h3_addrs(&[ipv6, ipv4], None, IpAddr::V4(Ipv4Addr::UNSPECIFIED));

        assert_eq!(addrs, vec![ipv4]);
    }

    #[test]
    fn filter_h3_addrs_honors_explicit_ipv4_local_address() {
        let ipv4 = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1)), 443);
        let ipv6 = SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), 443);

        let addrs = filter_h3_addrs(
            &[ipv6, ipv4],
            Some(IpAddr::V4(Ipv4Addr::LOCALHOST)),
            IpAddr::V6(Ipv6Addr::UNSPECIFIED),
        );

        assert_eq!(addrs, vec![ipv4]);
    }

    #[test]
    fn filter_h3_addrs_honors_explicit_ipv6_local_address() {
        let ipv4 = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1)), 443);
        let ipv6 = SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), 443);

        let addrs = filter_h3_addrs(
            &[ipv4, ipv6],
            Some(IpAddr::V6(Ipv6Addr::LOCALHOST)),
            IpAddr::V6(Ipv6Addr::UNSPECIFIED),
        );

        assert_eq!(addrs, vec![ipv6]);
    }

    #[test]
    fn filter_h3_addrs_empty_input() {
        let addrs = filter_h3_addrs(&[], None, IpAddr::V6(Ipv6Addr::UNSPECIFIED));
        assert!(addrs.is_empty());
    }

    #[test]
    fn filter_h3_addrs_all_same_family() {
        let a1 = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)), 443);
        let a2 = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 2)), 443);
        let addrs = filter_h3_addrs(&[a1, a2], None, IpAddr::V4(Ipv4Addr::UNSPECIFIED));
        assert_eq!(addrs, vec![a1, a2]);
    }

    #[test]
    fn h3_ipv4_bind_addr_returns_unspecified() {
        let addr = h3_ipv4_bind_addr();
        assert_eq!(addr.ip(), IpAddr::V4(Ipv4Addr::UNSPECIFIED));
        assert_eq!(addr.port(), 0);
    }

    // ── is_h3_no_error_stop_sending tests ────────────────────────────────
    // NOTE: h3::error::StreamError variants are #[non_exhaustive] and cannot be
    // constructed from outside the crate. The function is tested transitively via
    // integration tests that exercise actual H3 connections.

    // ── build_quinn_endpoint tests ───────────────────────────────────────

    #[cfg(feature = "tokio")]
    #[tokio::test]
    async fn build_quinn_endpoint_succeeds_with_defaults() {
        let config = make_rustls_config(&[b"h3"]);
        let result = build_quinn_endpoint(config, None, false);
        assert!(
            result.is_ok(),
            "build_quinn_endpoint failed: {:?}",
            result.err()
        );
    }

    #[cfg(feature = "tokio")]
    #[tokio::test]
    async fn build_quinn_endpoint_with_ipv4_local_address() {
        let config = make_rustls_config(&[b"h3"]);
        let local = IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1));
        let result = build_quinn_endpoint(config, Some(local), false);
        assert!(
            result.is_ok(),
            "build_quinn_endpoint failed: {:?}",
            result.err()
        );
        let ep = result.unwrap();
        assert!(ep.local_addr().unwrap().is_ipv4());
    }

    #[cfg(feature = "tokio")]
    #[tokio::test]
    async fn build_quinn_endpoint_with_0rtt_enabled() {
        let config = make_rustls_config(&[b"h3"]);
        let result = build_quinn_endpoint(config, None, true);
        assert!(
            result.is_ok(),
            "build_quinn_endpoint failed: {:?}",
            result.err()
        );
    }

    #[cfg(feature = "tokio")]
    #[tokio::test]
    async fn build_quinn_endpoint_adds_h3_alpn_if_missing() {
        // Pass a config without h3 ALPN; build_quinn_endpoint should add it
        let config = make_rustls_config(&[b"h2"]);
        let result = build_quinn_endpoint(config, None, false);
        assert!(
            result.is_ok(),
            "build_quinn_endpoint failed: {:?}",
            result.err()
        );
    }

    // ── filter_h3_addrs additional coverage ──────────────────────────────

    #[test]
    fn filter_h3_addrs_multiple_ipv4_with_ipv4_endpoint() {
        let a1 = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)), 443);
        let a2 = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 2)), 443);
        let a3 = SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), 443);
        let addrs = filter_h3_addrs(&[a1, a2, a3], None, IpAddr::V4(Ipv4Addr::UNSPECIFIED));
        assert_eq!(addrs, vec![a1, a2]);
    }

    #[test]
    fn filter_h3_addrs_multiple_ipv6_with_ipv6_endpoint() {
        let a1 = SocketAddr::new(IpAddr::V6(Ipv6Addr::new(0, 0, 0, 0, 0, 0, 0, 1)), 443);
        let a2 = SocketAddr::new(IpAddr::V6(Ipv6Addr::new(0, 0, 0, 0, 0, 0, 0, 2)), 443);
        let a3 = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)), 443);
        // With IPv6 endpoint and no local_address, interleave_addrs is called
        let addrs = filter_h3_addrs(&[a1, a2, a3], None, IpAddr::V6(Ipv6Addr::UNSPECIFIED));
        // interleave_addrs interleaves IPv6 and IPv4
        assert_eq!(addrs.len(), 3);
        // First should be IPv6 (interleaving puts IPv6 first)
        assert!(addrs[0].is_ipv6());
    }

    #[test]
    fn filter_h3_addrs_local_address_filters_all() {
        // All addresses are IPv6, but local_address is IPv4 -> empty
        let a1 = SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), 443);
        let a2 = SocketAddr::new(IpAddr::V6(Ipv6Addr::new(0, 0, 0, 0, 0, 0, 0, 2)), 443);
        let addrs = filter_h3_addrs(
            &[a1, a2],
            Some(IpAddr::V4(Ipv4Addr::LOCALHOST)),
            IpAddr::V6(Ipv6Addr::UNSPECIFIED),
        );
        assert!(addrs.is_empty());
    }

    // ── SelectQuicConnect done=true returns Pending ───────────────────────

    #[test]
    fn select_quic_connect_done_returns_pending() {
        use std::task::{Context, Poll, RawWaker, RawWakerVTable, Waker};

        fn noop(_: *const ()) {}
        fn clone_fn(p: *const ()) -> RawWaker {
            RawWaker::new(p, &VTABLE)
        }
        static VTABLE: RawWakerVTable = RawWakerVTable::new(clone_fn, noop, noop, noop);
        let raw = RawWaker::new(std::ptr::null(), &VTABLE);
        let waker = unsafe { Waker::from_raw(raw) };
        let mut cx = Context::from_waker(&waker);

        let mut select = SelectQuicConnect {
            connect: Box::pin(async { Err(Error::Timeout) }),
            sleep: Box::pin(async {}),
            addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 443),
            done: true,
        };

        let pin = unsafe { Pin::new_unchecked(&mut select) };
        assert!(matches!(pin.poll(&mut cx), Poll::Pending));
    }

    #[test]
    fn select_quic_connect_0rtt_done_returns_pending() {
        use std::task::{Context, Poll, RawWaker, RawWakerVTable, Waker};

        fn noop(_: *const ()) {}
        fn clone_fn(p: *const ()) -> RawWaker {
            RawWaker::new(p, &VTABLE)
        }
        static VTABLE: RawWakerVTable = RawWakerVTable::new(clone_fn, noop, noop, noop);
        let raw = RawWaker::new(std::ptr::null(), &VTABLE);
        let waker = unsafe { Waker::from_raw(raw) };
        let mut cx = Context::from_waker(&waker);

        let mut select = SelectQuicConnect0rtt {
            connect: Box::pin(async { Err(Error::Timeout) }),
            sleep: Box::pin(async {}),
            addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 443),
            done: true,
        };

        let pin = unsafe { Pin::new_unchecked(&mut select) };
        assert!(matches!(pin.poll(&mut cx), Poll::Pending));
    }
}
