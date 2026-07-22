use std::convert::Infallible;
use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use bytes::Bytes;
use http::header::ALT_SVC;
use http_body_util::{BodyExt as _, Full};

use aioduct::runtime::TokioRuntime;
use aioduct::runtime::tokio_rt::TcpConnector;
use aioduct::{HttpEngineSend, Resolve};

struct H3AttemptResolver {
    h3_port: u16,
    attempts: Arc<AtomicUsize>,
}

impl Resolve for H3AttemptResolver {
    fn resolve(
        &self,
        host: &str,
        port: u16,
    ) -> Pin<Box<dyn Future<Output = std::io::Result<SocketAddr>> + Send>> {
        assert_eq!(host, "localhost");
        if port == self.h3_port {
            self.attempts.fetch_add(1, Ordering::SeqCst);
        }
        Box::pin(async move { Ok(SocketAddr::from(([127, 0, 0, 1], port))) })
    }
}

async fn alt_svc_tcp_server(
    h3_port: u16,
) -> (
    SocketAddr,
    aioduct_test_server::ConnectionCounter,
    Arc<AtomicUsize>,
) {
    let uploads = Arc::new(AtomicUsize::new(0));
    let server_uploads = uploads.clone();
    let advertisement = format!("h3=\":{h3_port}\"; ma=3600");
    let (addr, _, counter) =
        aioduct_test_server::tls::tls_server_with(&[b"http/1.1"], move |request| {
            let uploads = server_uploads.clone();
            let advertisement = advertisement.clone();
            async move {
                let body = request.into_body().collect().await.unwrap().to_bytes();
                if !body.is_empty() {
                    assert_eq!(body, Bytes::from_static(b"one-shot upload"));
                    uploads.fetch_add(1, Ordering::SeqCst);
                }
                Ok::<_, Infallible>(
                    http::Response::builder()
                        .header(ALT_SVC, advertisement)
                        .body(Full::new(Bytes::from_static(b"tcp")))
                        .unwrap(),
                )
            }
        })
        .await;
    (addr, counter, uploads)
}

fn alt_svc_client() -> HttpEngineSend<TokioRuntime, TcpConnector> {
    HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .tls(aioduct::tls::RustlsConnector::danger_accept_invalid_certs())
        .alt_svc_h3(true)
        .unwrap()
        .connect_timeout(Duration::from_millis(150))
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap()
}

async fn version_fallback_h3_server(
    succeed_first: bool,
) -> (
    SocketAddr,
    aioduct_test_server::ConnectionCounter,
    Arc<AtomicUsize>,
) {
    let requests = Arc::new(AtomicUsize::new(0));
    let server_requests = requests.clone();
    let (addr, _, counter) = aioduct_test_server::h3::h3_server_streaming_with_transport(
        Arc::new(quinn::TransportConfig::default()),
        move |_request, mut stream, connection| {
            let request_index = server_requests.fetch_add(1, Ordering::SeqCst);
            async move {
                if succeed_first && request_index == 0 {
                    stream
                        .send_response(http::Response::builder().status(200).body(()).unwrap())
                        .await
                        .unwrap();
                    stream.finish().await.unwrap();
                    return;
                }
                connection.close(
                    quinn::VarInt::from_u64(h3::error::Code::H3_VERSION_FALLBACK.value()).unwrap(),
                    b"retry over an earlier HTTP version",
                );
            }
        },
    )
    .await;
    (addr, counter, requests)
}

async fn failing_h3_server() -> (SocketAddr, aioduct_test_server::ConnectionCounter) {
    let (addr, _, counter) = aioduct_test_server::h3::h3_server_streaming_with_transport(
        Arc::new(quinn::TransportConfig::default()),
        |_request, _stream, connection| async move {
            connection.close(
                quinn::VarInt::from_u64(h3::error::Code::H3_INTERNAL_ERROR.value()).unwrap(),
                b"ambiguous connection failure",
            );
        },
    )
    .await;
    (addr, counter)
}

async fn seed_alt_svc(client: &HttpEngineSend<TokioRuntime, TcpConnector>, url: &str) {
    let response = client.get(url).unwrap().send().await.unwrap();
    assert_eq!(response.version(), http::Version::HTTP_11);
    assert_eq!(response.text().await.unwrap(), "tcp");
}

#[tokio::test]
async fn changed_alt_svc_endpoint_does_not_reuse_previous_h3_connection() {
    let (second_addr, _, second_counter) =
        aioduct_test_server::h3::h3_server_streaming_with_transport(
            Arc::new(quinn::TransportConfig::default()),
            |_request, mut stream, _connection| async move {
                stream
                    .send_response(http::Response::builder().status(200).body(()).unwrap())
                    .await
                    .unwrap();
                stream
                    .send_data(Bytes::from_static(b"second"))
                    .await
                    .unwrap();
                stream.finish().await.unwrap();
            },
        )
        .await;
    let advertisement = format!("h3=\":{}\"; ma=3600", second_addr.port());
    let (first_addr, _, first_counter) =
        aioduct_test_server::h3::h3_server_streaming_with_transport(
            Arc::new(quinn::TransportConfig::default()),
            move |_request, mut stream, _connection| {
                let advertisement = advertisement.clone();
                async move {
                    stream
                        .send_response(
                            http::Response::builder()
                                .status(200)
                                .header(ALT_SVC, advertisement)
                                .body(())
                                .unwrap(),
                        )
                        .await
                        .unwrap();
                    stream
                        .send_data(Bytes::from_static(b"first"))
                        .await
                        .unwrap();
                    stream.finish().await.unwrap();
                }
            },
        )
        .await;
    let (tcp_addr, tcp_counter, _) = alt_svc_tcp_server(first_addr.port()).await;
    let client = alt_svc_client();
    let url = format!("https://127.0.0.1:{}/endpoint-change", tcp_addr.port());

    seed_alt_svc(&client, &url).await;
    let first = client.get(&url).unwrap().send().await.unwrap();
    assert_eq!(first.version(), http::Version::HTTP_3);
    assert_eq!(first.text().await.unwrap(), "first");

    let second = client.get(&url).unwrap().send().await.unwrap();
    assert_eq!(second.version(), http::Version::HTTP_3);
    assert_eq!(second.text().await.unwrap(), "second");

    assert_eq!(tcp_counter.requests(), 1);
    assert_eq!(first_counter.requests(), 1);
    assert_eq!(second_counter.requests(), 1);
}

#[tokio::test]
async fn failed_alt_svc_connect_falls_back_without_consuming_one_shot_body() {
    let held_udp = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let h3_port = held_udp.local_addr().unwrap().port();
    let (tcp_addr, tcp_counter, uploads) = alt_svc_tcp_server(h3_port).await;
    let h3_attempts = Arc::new(AtomicUsize::new(0));
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .tls(aioduct::tls::RustlsConnector::danger_accept_invalid_certs())
        .resolver(H3AttemptResolver {
            h3_port,
            attempts: h3_attempts.clone(),
        })
        .alt_svc_h3(true)
        .unwrap()
        .connect_timeout(Duration::from_millis(150))
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let url = format!("https://localhost:{}/fallback", tcp_addr.port());
    seed_alt_svc(&client, &url).await;
    let body = Full::new(Bytes::from_static(b"one-shot upload"))
        .map_err(|never| match never {})
        .boxed_unsync();

    let response = client
        .post(&url)
        .unwrap()
        .body_stream(body)
        .send()
        .await
        .unwrap();
    assert_eq!(response.version(), http::Version::HTTP_11);
    assert_eq!(response.text().await.unwrap(), "tcp");

    let suppressed = client.get(&url).unwrap().send().await.unwrap();
    assert_eq!(suppressed.version(), http::Version::HTTP_11);
    suppressed.bytes().await.unwrap();
    assert_eq!(h3_attempts.load(Ordering::SeqCst), 1);
    assert_eq!(tcp_counter.requests(), 3);
    assert_eq!(uploads.load(Ordering::SeqCst), 1);
    drop(held_udp);
}

#[tokio::test]
async fn fresh_h3_version_fallback_replays_safe_get_over_tcp() {
    let (h3_addr, h3_counter, _) = version_fallback_h3_server(false).await;
    let (tcp_addr, tcp_counter, _) = alt_svc_tcp_server(h3_addr.port()).await;
    let client = alt_svc_client();
    let url = format!("https://127.0.0.1:{}/version-fallback", tcp_addr.port());
    seed_alt_svc(&client, &url).await;

    let response = client.get(&url).unwrap().send().await.unwrap();
    assert_eq!(response.version(), http::Version::HTTP_11);
    assert_eq!(response.text().await.unwrap(), "tcp");
    let suppressed = client.get(&url).unwrap().send().await.unwrap();
    assert_eq!(suppressed.version(), http::Version::HTTP_11);

    assert_eq!(h3_counter.requests(), 1);
    assert_eq!(tcp_counter.requests(), 3);
}

#[tokio::test]
async fn pooled_h3_version_fallback_replays_safe_get_over_tcp() {
    let (h3_addr, h3_counter, _) = version_fallback_h3_server(true).await;
    let (tcp_addr, tcp_counter, _) = alt_svc_tcp_server(h3_addr.port()).await;
    let client = alt_svc_client();
    let url = format!("https://127.0.0.1:{}/pooled-fallback", tcp_addr.port());
    seed_alt_svc(&client, &url).await;

    let pooled = client.get(&url).unwrap().send().await.unwrap();
    assert_eq!(pooled.version(), http::Version::HTTP_3);
    pooled.bytes().await.unwrap();
    let response = client.get(&url).unwrap().send().await.unwrap();
    assert_eq!(response.version(), http::Version::HTTP_11);
    assert_eq!(response.text().await.unwrap(), "tcp");

    assert_eq!(h3_counter.connections(), 1);
    assert_eq!(h3_counter.requests(), 2);
    assert_eq!(tcp_counter.requests(), 2);
}

#[tokio::test]
async fn h3_version_fallback_does_not_replay_buffered_post() {
    let (h3_addr, h3_counter, _) = version_fallback_h3_server(false).await;
    let (tcp_addr, tcp_counter, uploads) = alt_svc_tcp_server(h3_addr.port()).await;
    let client = alt_svc_client();
    let url = format!("https://127.0.0.1:{}/buffered-post", tcp_addr.port());
    seed_alt_svc(&client, &url).await;

    let error = client
        .post(&url)
        .unwrap()
        .body("one-shot upload")
        .send()
        .await
        .unwrap_err();

    assert!(error.to_string().contains("HTTP/3"), "{error:?}");
    assert_eq!(h3_counter.requests(), 1);
    assert_eq!(tcp_counter.requests(), 1);
    assert_eq!(uploads.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn h3_version_fallback_does_not_replay_one_shot_body() {
    let (h3_addr, h3_counter, _) = version_fallback_h3_server(false).await;
    let (tcp_addr, tcp_counter, uploads) = alt_svc_tcp_server(h3_addr.port()).await;
    let client = alt_svc_client();
    let url = format!("https://127.0.0.1:{}/one-shot", tcp_addr.port());
    seed_alt_svc(&client, &url).await;
    let body = Full::new(Bytes::from_static(b"one-shot upload"))
        .map_err(|never| match never {})
        .boxed_unsync();

    let error = client
        .post(&url)
        .unwrap()
        .body_stream(body)
        .send()
        .await
        .unwrap_err();

    assert!(error.to_string().contains("HTTP/3"), "{error:?}");
    assert_eq!(h3_counter.requests(), 1);
    assert_eq!(tcp_counter.requests(), 1);
    assert_eq!(uploads.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn ambiguous_h3_connection_failure_is_terminal_and_suppresses_alt_svc() {
    let (h3_addr, h3_counter) = failing_h3_server().await;
    let (tcp_addr, tcp_counter, _) = alt_svc_tcp_server(h3_addr.port()).await;
    let client = alt_svc_client();
    let url = format!("https://127.0.0.1:{}/connection-loss", tcp_addr.port());
    seed_alt_svc(&client, &url).await;

    let error = client.get(&url).unwrap().send().await.unwrap_err();
    assert!(error.to_string().contains("HTTP/3"), "{error:?}");

    let suppressed = client.get(&url).unwrap().send().await.unwrap();
    assert_eq!(suppressed.version(), http::Version::HTTP_11);
    suppressed.bytes().await.unwrap();
    assert_eq!(h3_counter.requests(), 1);
    assert_eq!(tcp_counter.requests(), 2);
}

#[tokio::test]
async fn always_h3_connect_failure_does_not_fall_back_to_tcp() {
    let (tcp_addr, _, tcp_counter) =
        aioduct_test_server::tls::tls_server_with(&[b"http/1.1"], |_request| async {
            Ok::<_, Infallible>(http::Response::new(Full::new(Bytes::from_static(b"tcp"))))
        })
        .await;
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .tls(aioduct::tls::RustlsConnector::danger_accept_invalid_certs())
        .http3(true)
        .unwrap()
        .connect_timeout(Duration::from_millis(100))
        .timeout(Duration::from_secs(2))
        .build()
        .unwrap();

    let result = client
        .get(&format!("https://127.0.0.1:{}/strict", tcp_addr.port()))
        .unwrap()
        .send()
        .await;

    assert!(result.is_err());
    assert_eq!(tcp_counter.requests(), 0);
}
