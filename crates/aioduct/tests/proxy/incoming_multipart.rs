use std::convert::Infallible;

use bytes::Bytes;
use http_body_util::Full;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

use super::*;

#[path = "../support/proxy_incoming_multipart.rs"]
mod support;

async fn start_forwarding_broker(
    client: HttpEngineSend<TokioRuntime, TcpConnector>,
    upstream: http::Uri,
) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let io = aioduct::runtime::tokio_rt::TokioIo::new(stream);
            let client = client.clone();
            let upstream = upstream.clone();
            tokio::spawn(async move {
                let service = service_fn(move |request: Request<hyper::body::Incoming>| {
                    let client = client.clone();
                    let upstream = upstream.clone();
                    async move {
                        let response = match client.forward(request).upstream(upstream).send().await
                        {
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
                let _ = http1::Builder::new().serve_connection(io, service).await;
            });
        }
    });

    addr
}

fn bad_gateway(error: String) -> Response<Full<Bytes>> {
    Response::builder()
        .status(http::StatusCode::BAD_GATEWAY)
        .body(Full::new(Bytes::from(error)))
        .unwrap()
}

async fn post_raw_multipart(
    addr: SocketAddr,
    multipart_body: fn() -> (String, Bytes),
) -> (u16, Bytes) {
    let (content_type, body) = multipart_body();
    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let request = format!(
        "POST {path} HTTP/1.1\r\n\
Host: {addr}\r\n\
Content-Type: {content_type}\r\n\
Content-Length: {length}\r\n\
Connection: close\r\n\r\n",
        path = support::MULTIPART_PATH,
        length = body.len(),
    );
    stream.write_all(request.as_bytes()).await.unwrap();
    stream.write_all(&body).await.unwrap();
    stream.flush().await.unwrap();

    let mut response = Vec::new();
    stream.read_to_end(&mut response).await.unwrap();
    parse_raw_response(&response)
}

fn parse_raw_response(response: &[u8]) -> (u16, Bytes) {
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

async fn assert_forward_real_incoming_multipart_reuses_fresh_tunnel<ProxyConnections>(
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
    let connector =
        aioduct::tls::RustlsConnector::new(support::client_config_trusting(&certificates));
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .tls(connector)
        .proxy(proxy)
        .pool_idle_timeout(Duration::from_secs(60))
        .timeout(support::TEST_TIMEOUT)
        .build()
        .unwrap();
    let upstream = origin.upstream();

    let warm = client
        .get(&support::upstream_url(&upstream, "/warm"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(warm.text().await.unwrap(), "warm:1");
    assert_eq!(origin.observations.connections(), 1);
    assert_eq!(proxy_connections(), 1);

    let broker_addr = start_forwarding_broker(client.clone(), upstream.clone()).await;
    let (status, body) = post_raw_multipart(broker_addr, multipart_body).await;

    assert_eq!(status, 200, "broker returned body: {body:?}");
    assert_eq!(body, Bytes::from_static(b"upload:2"));
    assert_eq!(origin.observations.connections(), 2);
    assert_eq!(proxy_connections(), 2);
    assert_eq!(origin.observations.uploads(), 1);
    assert_eq!(origin.observations.exact_uploads(), 1);
    assert_eq!(origin.observations.file_occurrences(), 1);

    origin.close_first_and_wait_blocking();

    let follow_up = client
        .get(&support::upstream_url(&upstream, "/follow-up"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(follow_up.text().await.unwrap(), "follow-up:2");
    assert_eq!(origin.observations.connections(), 2);
    assert_eq!(
        proxy_connections(),
        2,
        "the ordinary follow-up must reuse the upload's fresh CONNECT tunnel"
    );
}

async fn assert_forward_real_incoming_multipart_through_https_proxy(
    origin: support::HttpsMultipartOrigin,
) {
    let proxy = support::HttpsConnectProxy::start();
    let observations = proxy.observations.clone();
    let upload_bytes = support::backpressured_multipart_body().1.len();
    assert_forward_real_incoming_multipart_reuses_fresh_tunnel(
        origin,
        aioduct::ProxyConfig::https(&format!("https://localhost:{}", proxy.addr.port())).unwrap(),
        Some(proxy.certificate.clone()),
        support::backpressured_multipart_body,
        || observations.connections(),
    )
    .await;

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

#[tokio::test]
async fn forward_real_incoming_multipart_through_connect_proxy_reuses_fresh_tunnel_send() {
    let (proxy_addr, proxy_connections) = connect_proxy().await;
    assert_forward_real_incoming_multipart_reuses_fresh_tunnel(
        support::HttpsH1MultipartOrigin::start(),
        aioduct::ProxyConfig::http(&format!("http://{proxy_addr}")).unwrap(),
        None,
        support::multipart_body,
        || proxy_connections.load(AtomicOrdering::SeqCst),
    )
    .await;
}

#[tokio::test]
async fn forward_real_incoming_multipart_through_connect_proxy_reuses_fresh_https_h2_tunnel_send() {
    let (proxy_addr, proxy_connections) = connect_proxy().await;
    assert_forward_real_incoming_multipart_reuses_fresh_tunnel(
        support::HttpsH2MultipartOrigin::start(),
        aioduct::ProxyConfig::http(&format!("http://{proxy_addr}")).unwrap(),
        None,
        support::multipart_body,
        || proxy_connections.load(AtomicOrdering::SeqCst),
    )
    .await;
}

#[tokio::test]
async fn forward_real_incoming_multipart_through_https_proxy_reuses_fresh_tunnel_send() {
    assert_forward_real_incoming_multipart_through_https_proxy(
        support::HttpsH1MultipartOrigin::start_backpressured(),
    )
    .await;
}

#[tokio::test]
async fn forward_real_incoming_multipart_through_https_proxy_reuses_fresh_https_h2_tunnel_send() {
    assert_forward_real_incoming_multipart_through_https_proxy(
        support::HttpsH2MultipartOrigin::start_backpressured(),
    )
    .await;
}
