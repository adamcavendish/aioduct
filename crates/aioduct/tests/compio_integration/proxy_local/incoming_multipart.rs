use std::convert::Infallible;
use std::io::{Read as _, Write as _};
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;

use bytes::Bytes;
use http_body_util::Full;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response};

use super::*;

#[path = "../../support/proxy_incoming_multipart.rs"]
mod support;

const FOLLOW_UP_PATH: &str = "/__aioduct_proxy_pool_follow_up";

struct CompioProxyForwardingBroker {
    addr: SocketAddr,
    warm_identity: String,
    shutdown: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl CompioProxyForwardingBroker {
    fn start(
        upstream: http::Uri,
        proxy: aioduct::ProxyConfig,
        tls_config: Arc<rustls::ClientConfig>,
    ) -> Self {
        let shutdown = Arc::new(AtomicBool::new(false));
        let server_shutdown = shutdown.clone();
        let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel(1);
        let thread = std::thread::spawn(move || {
            compio_runtime::Runtime::new()
                .unwrap()
                .block_on(async move {
                    let connector = aioduct::tls::RustlsConnector::new(tls_config);
                    let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
                        .tls(connector)
                        .proxy(proxy)
                        .pool_idle_timeout(Duration::from_secs(60))
                        .timeout(support::TEST_TIMEOUT)
                        .build_local()
                        .unwrap();

                    let warm = client
                        .get_local(&support::upstream_url(&upstream, "/warm"))
                        .unwrap()
                        .send()
                        .await
                        .unwrap();
                    let warm_identity = warm.text().await.unwrap();

                    let listener = compio_net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                    ready_tx
                        .send((listener.local_addr().unwrap(), warm_identity))
                        .unwrap();

                    loop {
                        let (stream, _) = listener.accept().await.unwrap();
                        if server_shutdown.load(Ordering::SeqCst) {
                            return;
                        }
                        let io = Box::pin(aioduct::runtime::compio_rt::CompioIo::new(
                            compio_io::compat::AsyncStream::new(stream),
                        ));
                        let request_client = client.clone();
                        let request_upstream = upstream.clone();
                        let service = service_fn(move |request: Request<hyper::body::Incoming>| {
                            let client = request_client.clone();
                            let upstream = request_upstream.clone();
                            async move {
                                let result = if request.method() == http::Method::GET
                                    && request.uri().path() == FOLLOW_UP_PATH
                                {
                                    drop(request);
                                    match client
                                        .get_local(&support::upstream_url(&upstream, "/follow-up"))
                                    {
                                        Ok(request) => request.send().await,
                                        Err(error) => Err(error),
                                    }
                                } else {
                                    client
                                        .forward_local(super::super::valid_forward_request(request))
                                        .upstream(upstream)
                                        .send()
                                        .await
                                };
                                let response = match result {
                                    Ok(response) => {
                                        let status = response.status();
                                        match response.bytes().await {
                                            Ok(body) => Response::builder()
                                                .status(status)
                                                .body(Full::new(body))
                                                .unwrap(),
                                            Err(error) => bad_gateway(error.to_string()),
                                        }
                                    }
                                    Err(error) => bad_gateway(error.to_string()),
                                };
                                Ok::<_, Infallible>(response)
                            }
                        });
                        let connection = http1::Builder::new().serve_connection(io, service);
                        let _ =
                            compio_runtime::time::timeout(support::TEST_TIMEOUT, connection).await;
                    }
                });
        });
        let (addr, warm_identity) = ready_rx
            .recv_timeout(support::TEST_TIMEOUT)
            .expect("Compio proxy forwarding broker did not start");
        Self {
            addr,
            warm_identity,
            shutdown,
            thread: Some(thread),
        }
    }
}

impl Drop for CompioProxyForwardingBroker {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::SeqCst);
        let _ = std::net::TcpStream::connect_timeout(&self.addr, support::TEST_TIMEOUT);
        if let Some(thread) = self.thread.take() {
            let result = thread.join();
            if !std::thread::panicking() {
                result.expect("Compio proxy forwarding broker thread panicked");
            }
        }
    }
}

fn bad_gateway(error: String) -> Response<Full<Bytes>> {
    Response::builder()
        .status(http::StatusCode::BAD_GATEWAY)
        .body(Full::new(Bytes::from(error)))
        .unwrap()
}

fn raw_request(addr: SocketAddr, head: &[u8], body: &[u8]) -> (u16, Bytes) {
    let mut stream = std::net::TcpStream::connect_timeout(&addr, support::TEST_TIMEOUT).unwrap();
    stream
        .set_read_timeout(Some(support::TEST_TIMEOUT))
        .unwrap();
    stream
        .set_write_timeout(Some(support::TEST_TIMEOUT))
        .unwrap();
    stream.write_all(head).unwrap();
    stream.write_all(body).unwrap();
    stream.flush().unwrap();

    let mut response = Vec::new();
    stream.read_to_end(&mut response).unwrap();
    let header_end = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .expect("broker response should contain complete headers");
    let headers = String::from_utf8_lossy(&response[..header_end]);
    let status = headers
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|status| status.parse::<u16>().ok())
        .expect("broker response should contain a status code");
    (status, Bytes::copy_from_slice(&response[header_end + 4..]))
}

fn post_raw_multipart(addr: SocketAddr, multipart_body: fn() -> (String, Bytes)) -> (u16, Bytes) {
    let (content_type, body) = multipart_body();
    let head = format!(
        "POST {path} HTTP/1.1\r\n\
Host: {addr}\r\n\
Content-Type: {content_type}\r\n\
Content-Length: {length}\r\n\
Connection: close\r\n\r\n",
        path = support::MULTIPART_PATH,
        length = body.len(),
    );
    raw_request(addr, head.as_bytes(), &body)
}

fn get_follow_up(addr: SocketAddr) -> (u16, Bytes) {
    let head = format!(
        "GET {FOLLOW_UP_PATH} HTTP/1.1\r\n\
Host: {addr}\r\n\
Connection: close\r\n\r\n"
    );
    raw_request(addr, head.as_bytes(), &[])
}

fn assert_compio_forward_local_real_incoming_multipart_reuses_fresh_tunnel<ProxyConnections>(
    origin: support::HttpsMultipartOrigin,
    proxy: aioduct::ProxyConfig,
    proxy_certificate: Option<rustls::pki_types::CertificateDer<'static>>,
    multipart_body: fn() -> (String, Bytes),
    proxy_connections: ProxyConnections,
) where
    ProxyConnections: Fn() -> usize,
{
    let mut certificates = vec![origin.certificate.clone()];
    certificates.extend(proxy_certificate);
    let broker = CompioProxyForwardingBroker::start(
        origin.upstream(),
        proxy,
        support::client_config_trusting(&certificates),
    );

    assert_eq!(broker.warm_identity, "warm:1");
    assert_eq!(origin.observations.connections(), 1);
    assert_eq!(proxy_connections(), 1);

    let (status, body) = post_raw_multipart(broker.addr, multipart_body);

    assert_eq!(status, 200, "broker returned body: {body:?}");
    assert_eq!(body, Bytes::from_static(b"upload:2"));
    assert_eq!(origin.observations.connections(), 2);
    assert_eq!(proxy_connections(), 2);
    assert_eq!(origin.observations.uploads(), 1);
    assert_eq!(origin.observations.exact_uploads(), 1);
    assert_eq!(origin.observations.file_occurrences(), 1);

    origin.close_first_and_wait_blocking();
    let (status, body) = get_follow_up(broker.addr);

    assert_eq!(status, 200, "broker returned body: {body:?}");
    assert_eq!(body, Bytes::from_static(b"follow-up:2"));
    assert_eq!(origin.observations.connections(), 2);
    assert_eq!(
        proxy_connections(),
        2,
        "the ordinary follow-up must reuse the upload's fresh CONNECT tunnel"
    );
}

fn assert_compio_forward_local_real_incoming_multipart_through_https_proxy(
    origin: support::HttpsMultipartOrigin,
) {
    let proxy = support::HttpsConnectProxy::start();
    let observations = proxy.observations.clone();
    let upload_bytes = support::backpressured_multipart_body().1.len();
    assert_compio_forward_local_real_incoming_multipart_reuses_fresh_tunnel(
        origin,
        aioduct::ProxyConfig::https(&format!("https://localhost:{}", proxy.addr.port())).unwrap(),
        Some(proxy.certificate.clone()),
        support::backpressured_multipart_body,
        || observations.connections(),
    );

    assert_eq!(observations.http1_alpn_connections(), 2);
    assert_eq!(observations.connect_requests(), 2);
    assert!(
        observations.max_tunneled_client_bytes() >= upload_bytes,
        "the throttled outer TLS tunnel did not carry the complete multipart upload"
    );
    assert!(
        observations.throttled_client_reads() > 16,
        "the HTTPS proxy did not apply sustained read backpressure"
    );
}

#[test]
fn compio_forward_local_real_incoming_multipart_through_connect_proxy_reuses_fresh_tunnel() {
    let (proxy_addr, proxy_connections) = start_counting_http_proxy_tokio();
    assert_compio_forward_local_real_incoming_multipart_reuses_fresh_tunnel(
        support::HttpsH1MultipartOrigin::start(),
        aioduct::ProxyConfig::http(&format!("http://{proxy_addr}")).unwrap(),
        None,
        support::multipart_body,
        || proxy_connections.load(std::sync::atomic::Ordering::SeqCst),
    );
}

#[test]
fn compio_forward_local_real_incoming_multipart_through_connect_proxy_reuses_fresh_https_h2_tunnel()
{
    let (proxy_addr, proxy_connections) = start_counting_http_proxy_tokio();
    assert_compio_forward_local_real_incoming_multipart_reuses_fresh_tunnel(
        support::HttpsH2MultipartOrigin::start(),
        aioduct::ProxyConfig::http(&format!("http://{proxy_addr}")).unwrap(),
        None,
        support::multipart_body,
        || proxy_connections.load(std::sync::atomic::Ordering::SeqCst),
    );
}

#[test]
fn compio_forward_local_real_incoming_multipart_through_https_proxy_reuses_fresh_tunnel() {
    assert_compio_forward_local_real_incoming_multipart_through_https_proxy(
        support::HttpsH1MultipartOrigin::start_backpressured(),
    );
}

#[test]
fn compio_forward_local_real_incoming_multipart_through_https_proxy_reuses_fresh_https_h2_tunnel() {
    assert_compio_forward_local_real_incoming_multipart_through_https_proxy(
        support::HttpsH2MultipartOrigin::start_backpressured(),
    );
}
