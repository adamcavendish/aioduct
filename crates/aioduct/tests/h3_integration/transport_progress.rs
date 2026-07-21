use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use bytes::{Buf, Bytes};
use http_body_util::BodyExt as _;

use aioduct::HttpEngineSend;
use aioduct::runtime::TokioRuntime;
use aioduct::runtime::tokio_rt::TcpConnector;

fn client(write_timeout: Duration) -> HttpEngineSend<TokioRuntime, TcpConnector> {
    HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .tls(aioduct::tls::RustlsConnector::danger_accept_invalid_certs())
        .http3(true)
        .unwrap()
        .write_timeout(write_timeout)
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap()
}

fn constrained_transport(stream_window: u32) -> Arc<quinn::TransportConfig> {
    let mut transport = quinn::TransportConfig::default();
    transport.stream_receive_window(stream_window.into());
    transport.receive_window((8_u32 * 1024 * 1024).into());
    Arc::new(transport)
}

async fn respond_ok(stream: &mut aioduct_test_server::h3::H3RequestStream) {
    let response = http::Response::builder().status(200).body(()).unwrap();
    stream.send_response(response).await.unwrap();
    stream.finish().await.unwrap();
}

#[tokio::test]
async fn h3_transport_write_times_out_when_flow_control_stalls() {
    let (addr, _, _) = aioduct_test_server::h3::h3_server_streaming_with_transport(
        constrained_transport(16 * 1024),
        |_request, stream, _connection| async move {
            let _stream = stream;
            tokio::time::sleep(Duration::from_secs(5)).await;
        },
    )
    .await;

    let error = client(Duration::from_millis(100))
        .post(&format!("https://127.0.0.1:{}/upload", addr.port()))
        .unwrap()
        .body(vec![0_u8; 4 * 1024 * 1024])
        .send()
        .await
        .unwrap_err();

    assert!(
        matches!(error.error(), aioduct::Error::WriteTimeout),
        "expected H3 transport write timeout, got {error:?}"
    );
}

#[tokio::test]
async fn h3_slow_transport_progress_refreshes_the_write_timeout() {
    const STREAM_WINDOW: u32 = 4 * 1024;
    const BODY_SIZE: usize = 128 * 1024;

    let received = Arc::new(AtomicUsize::new(0));
    let server_received = received.clone();
    let (addr, _, _) = aioduct_test_server::h3::h3_server_streaming_with_transport(
        constrained_transport(STREAM_WINDOW),
        move |_request, mut stream, _connection| {
            let received = server_received.clone();
            async move {
                let mut total = 0;
                while let Some(mut chunk) = stream.recv_data().await.unwrap() {
                    total += chunk.remaining();
                    chunk.advance(chunk.remaining());
                    received.store(total, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                respond_ok(&mut stream).await;
            }
        },
    )
    .await;

    let started = Instant::now();
    let response = client(Duration::from_millis(100))
        .post(&format!("https://127.0.0.1:{}/upload", addr.port()))
        .unwrap()
        .body(vec![0_u8; BODY_SIZE])
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), http::StatusCode::OK);
    assert!(started.elapsed() > Duration::from_millis(300));
    assert_eq!(received.load(Ordering::SeqCst), BODY_SIZE);
}

#[tokio::test]
async fn h3_detached_upload_uses_transport_progress_timeout() {
    const STREAM_WINDOW: u32 = 16 * 1024;
    const BODY_SIZE: usize = 1024 * 1024;

    let received = Arc::new(AtomicUsize::new(0));
    let server_received = received.clone();
    let (complete_tx, complete_rx) = tokio::sync::oneshot::channel();
    let complete_tx = Arc::new(std::sync::Mutex::new(Some(complete_tx)));
    let server_complete_tx = complete_tx.clone();
    let (addr, _, _) = aioduct_test_server::h3::h3_server_streaming_with_transport(
        constrained_transport(STREAM_WINDOW),
        move |_request, mut stream, _connection| {
            let received = server_received.clone();
            let complete_tx = server_complete_tx.clone();
            async move {
                respond_ok(&mut stream).await;
                let mut total = 0;
                let mut next_pause = 32 * 1024;
                let result = loop {
                    match stream.recv_data().await {
                        Ok(Some(mut chunk)) => {
                            total += chunk.remaining();
                            chunk.advance(chunk.remaining());
                            received.store(total, Ordering::SeqCst);
                            if total >= next_pause {
                                next_pause += 32 * 1024;
                                tokio::time::sleep(Duration::from_millis(20)).await;
                            }
                        }
                        Ok(None) => break Ok(total),
                        Err(error) => break Err(error.to_string()),
                    }
                };
                if let Some(sender) = complete_tx.lock().unwrap().take() {
                    let _ = sender.send(result);
                }
            }
        },
    )
    .await;

    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    let body = http_body_util::StreamBody::new(futures_util::stream::once(async move {
        let data = release_rx.await.expect("upload release sender dropped");
        Ok::<_, aioduct::Error>(hyper::body::Frame::data(data))
    }))
    .boxed_unsync();
    let response = tokio::time::timeout(
        Duration::from_secs(1),
        client(Duration::from_millis(300))
            .post(&format!("https://127.0.0.1:{}/upload", addr.port()))
            .unwrap()
            .body_stream(body)
            .send(),
    )
    .await
    .expect("HTTP/3 response waited for the gated upload")
    .unwrap();

    release_tx.send(Bytes::from(vec![0_u8; BODY_SIZE])).unwrap();
    assert!(response.bytes().await.unwrap().is_empty());
    let started = Instant::now();
    let drained = tokio::time::timeout(Duration::from_secs(3), complete_rx).await;
    assert!(
        drained.is_ok(),
        "server received only {} of {BODY_SIZE} detached upload bytes",
        received.load(Ordering::SeqCst)
    );
    assert_eq!(drained.unwrap().unwrap(), Ok(BODY_SIZE));

    assert!(started.elapsed() > Duration::from_millis(300));
    assert_eq!(received.load(Ordering::SeqCst), BODY_SIZE);
}
