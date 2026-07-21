use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::{Buf, Bytes};
use http_body_util::BodyExt as _;

use aioduct::HttpEngineSend;
use aioduct::runtime::TokioRuntime;
use aioduct::runtime::tokio_rt::TcpConnector;
use aioduct_test_server::h3::H3RequestStream;

fn client() -> HttpEngineSend<TokioRuntime, TcpConnector> {
    HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .tls(aioduct::tls::RustlsConnector::danger_accept_invalid_certs())
        .http3(true)
        .unwrap()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap()
}

fn constrained_transport() -> Arc<quinn::TransportConfig> {
    let mut transport = quinn::TransportConfig::default();
    transport.stream_receive_window((16_u32 * 1024).into());
    Arc::new(transport)
}

async fn read_request_body(stream: &mut H3RequestStream) -> Vec<u8> {
    let mut body = Vec::new();
    while let Some(mut chunk) = stream.recv_data().await.unwrap() {
        body.extend_from_slice(chunk.chunk());
        chunk.advance(chunk.remaining());
    }
    body
}

async fn respond_ok(stream: &mut H3RequestStream) {
    let response = http::Response::builder().status(200).body(()).unwrap();
    stream.send_response(response).await.unwrap();
    stream.finish().await.unwrap();
}

async fn warm(client: &HttpEngineSend<TokioRuntime, TcpConnector>, base: &str) {
    let response = client
        .get(&format!("{base}/warm"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert!(response.bytes().await.unwrap().is_empty());
}

#[tokio::test]
async fn fresh_h3_request_rejected_retries_replayable_post_once() {
    let rejected = Arc::new(AtomicBool::new(false));
    let received = Arc::new(Mutex::new(Vec::new()));
    let server_rejected = rejected.clone();
    let server_received = received.clone();
    let (addr, _, counter) = aioduct_test_server::h3::h3_server_streaming_with_transport(
        constrained_transport(),
        move |_request, mut stream, _connection| {
            let rejected = server_rejected.clone();
            let received = server_received.clone();
            async move {
                if !rejected.swap(true, Ordering::SeqCst) {
                    stream.stop_sending(h3::error::Code::H3_REQUEST_REJECTED);
                    stream.stop_stream(h3::error::Code::H3_REQUEST_REJECTED);
                    return;
                }

                let body = read_request_body(&mut stream).await;
                received.lock().unwrap().push(body);
                respond_ok(&mut stream).await;
            }
        },
    )
    .await;
    let body = vec![b'x'; 128 * 1024];

    let response = client()
        .post(&format!("https://127.0.0.1:{}/upload", addr.port()))
        .unwrap()
        .body(body.clone())
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), http::StatusCode::OK);
    assert_eq!(counter.connections(), 2);
    assert_eq!(counter.requests(), 2);
    assert_eq!(received.lock().unwrap().as_slice(), [body]);
}

#[tokio::test]
async fn pooled_h3_request_rejected_retries_replayable_post_once() {
    let rejected = Arc::new(AtomicBool::new(false));
    let received = Arc::new(Mutex::new(Vec::new()));
    let server_rejected = rejected.clone();
    let server_received = received.clone();
    let (addr, _, counter) =
        aioduct_test_server::h3::h3_server_streaming(move |request, mut stream| {
            let rejected = server_rejected.clone();
            let received = server_received.clone();
            async move {
                if request.uri().path() == "/upload" && !rejected.swap(true, Ordering::SeqCst) {
                    stream.stop_sending(h3::error::Code::H3_REQUEST_REJECTED);
                    stream.stop_stream(h3::error::Code::H3_REQUEST_REJECTED);
                    return;
                }
                let body = read_request_body(&mut stream).await;
                if request.uri().path() == "/upload" {
                    received.lock().unwrap().push(body);
                }
                respond_ok(&mut stream).await;
            }
        })
        .await;
    let client = client();
    let base = format!("https://127.0.0.1:{}", addr.port());
    warm(&client, &base).await;

    let response = client
        .post(&format!("{base}/upload"))
        .unwrap()
        .body("replayable H3 upload")
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), http::StatusCode::OK);
    assert_eq!(counter.connections(), 2);
    assert_eq!(counter.requests(), 3);
    assert_eq!(
        received.lock().unwrap().as_slice(),
        [b"replayable H3 upload"]
    );
}

#[tokio::test]
async fn pooled_and_fresh_h3_rejections_share_one_replay_budget() {
    let (addr, _, counter) =
        aioduct_test_server::h3::h3_server_streaming(|request, mut stream| async move {
            if request.uri().path() == "/upload" {
                stream.stop_sending(h3::error::Code::H3_REQUEST_REJECTED);
                stream.stop_stream(h3::error::Code::H3_REQUEST_REJECTED);
            } else {
                let _ = read_request_body(&mut stream).await;
                respond_ok(&mut stream).await;
            }
        })
        .await;
    let client = client();
    let base = format!("https://127.0.0.1:{}", addr.port());
    warm(&client, &base).await;

    let result = client
        .post(&format!("{base}/upload"))
        .unwrap()
        .body("replay exactly once")
        .send()
        .await;

    assert!(result.is_err());
    assert_eq!(counter.connections(), 2);
    assert_eq!(counter.requests(), 3);
}

#[tokio::test]
async fn h3_request_rejected_does_not_replay_a_one_shot_body() {
    let (addr, _, counter) =
        aioduct_test_server::h3::h3_server_streaming(|_request, mut stream| async move {
            stream.stop_sending(h3::error::Code::H3_REQUEST_REJECTED);
            stream.stop_stream(h3::error::Code::H3_REQUEST_REJECTED);
        })
        .await;
    let frames = futures_util::stream::once(async {
        Ok::<_, aioduct::Error>(hyper::body::Frame::data(Bytes::from_static(b"one shot")))
    });
    let body = http_body_util::StreamBody::new(frames).boxed_unsync();

    let result = client()
        .post(&format!("https://127.0.0.1:{}/upload", addr.port()))
        .unwrap()
        .body_stream(body)
        .send()
        .await;

    assert!(result.is_err());
    assert_eq!(counter.connections(), 1);
    assert_eq!(counter.requests(), 1);
}

#[tokio::test]
async fn h3_processed_reset_is_not_replayed() {
    let receipts = Arc::new(AtomicUsize::new(0));
    let server_receipts = receipts.clone();
    let (addr, _, counter) =
        aioduct_test_server::h3::h3_server_streaming(move |request, mut stream| {
            let receipts = server_receipts.clone();
            async move {
                let body = read_request_body(&mut stream).await;
                if request.uri().path() == "/reset" {
                    assert_eq!(body, b"processed once");
                    receipts.fetch_add(1, Ordering::SeqCst);
                    stream.stop_stream(h3::error::Code::H3_REQUEST_CANCELLED);
                    return;
                }
                respond_ok(&mut stream).await;
            }
        })
        .await;
    let client = client();
    let base = format!("https://127.0.0.1:{}", addr.port());
    warm(&client, &base).await;

    let result = client
        .post(&format!("{base}/reset"))
        .unwrap()
        .body("processed once")
        .send()
        .await;

    assert!(result.is_err());
    assert_eq!(receipts.load(Ordering::SeqCst), 1);
    assert_eq!(counter.connections(), 1);
    assert_eq!(counter.requests(), 2);
}

#[tokio::test]
async fn h3_connection_loss_after_processed_post_is_not_replayed() {
    let receipts = Arc::new(AtomicUsize::new(0));
    let server_receipts = receipts.clone();
    let (addr, _, counter) = aioduct_test_server::h3::h3_server_streaming_with_transport(
        Arc::new(quinn::TransportConfig::default()),
        move |request, mut stream, connection| {
            let receipts = server_receipts.clone();
            async move {
                let body = read_request_body(&mut stream).await;
                if request.uri().path() == "/close" {
                    assert_eq!(body, b"processed before connection loss");
                    receipts.fetch_add(1, Ordering::SeqCst);
                    connection.close(
                        quinn::VarInt::from_u32(h3::error::Code::H3_INTERNAL_ERROR.value() as u32),
                        b"test connection loss",
                    );
                    return;
                }
                respond_ok(&mut stream).await;
            }
        },
    )
    .await;
    let client = client();
    let base = format!("https://127.0.0.1:{}", addr.port());
    warm(&client, &base).await;

    let result = client
        .post(&format!("{base}/close"))
        .unwrap()
        .body("processed before connection loss")
        .send()
        .await;

    assert!(result.is_err());
    assert_eq!(receipts.load(Ordering::SeqCst), 1);
    assert_eq!(counter.connections(), 1);
    assert_eq!(counter.requests(), 2);
}
