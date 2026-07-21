use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::{Context, Poll};
use std::time::Duration;

use bytes::{Buf, Bytes};
use http_body_util::BodyExt as _;

use aioduct::HttpEngineSend;
use aioduct::runtime::TokioRuntime;
use aioduct::runtime::tokio_rt::TcpConnector;

struct PendingBody {
    dropped: Arc<AtomicBool>,
}

impl http_body::Body for PendingBody {
    type Data = Bytes;
    type Error = aioduct::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        _context: &mut Context<'_>,
    ) -> Poll<Option<Result<hyper::body::Frame<Self::Data>, Self::Error>>> {
        Poll::Pending
    }

    fn is_end_stream(&self) -> bool {
        false
    }
}

impl Drop for PendingBody {
    fn drop(&mut self) {
        self.dropped.store(true, Ordering::SeqCst);
    }
}

fn client(write_timeout: Option<Duration>) -> HttpEngineSend<TokioRuntime, TcpConnector> {
    let mut builder = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .tls(aioduct::tls::RustlsConnector::danger_accept_invalid_certs())
        .http3(true)
        .unwrap()
        .timeout(Duration::from_secs(5));
    if let Some(write_timeout) = write_timeout {
        builder = builder.write_timeout(write_timeout);
    }
    builder.build().unwrap()
}

async fn request_reset_code<S>(stream: &mut h3::server::RequestStream<S, Bytes>) -> h3::error::Code
where
    S: h3::quic::RecvStream,
{
    loop {
        match stream.recv_data().await {
            Ok(Some(mut chunk)) => chunk.advance(chunk.remaining()),
            Ok(None) => panic!("HTTP/3 request direction finished without a reset"),
            Err(h3::error::StreamError::RemoteTerminate { code, .. }) => return code,
            Err(error) => panic!("unexpected H3 request-stream error: {error}"),
        }
    }
}

#[tokio::test]
async fn h3_returns_early_response_and_continues_upload() {
    let (release_tx, release_rx) = tokio::sync::oneshot::channel::<Bytes>();
    let (received_tx, received_rx) = tokio::sync::oneshot::channel::<Vec<u8>>();
    let received_tx = Arc::new(std::sync::Mutex::new(Some(received_tx)));
    let (addr, _, _) = aioduct_test_server::h3::h3_server_streaming(move |_request, mut stream| {
        let received_tx = received_tx.clone();
        async move {
            stream
                .send_response(http::Response::builder().status(200).body(()).unwrap())
                .await
                .unwrap();
            stream
                .send_data(Bytes::from_static(b"accepted"))
                .await
                .unwrap();
            stream.finish().await.unwrap();

            let mut received = Vec::new();
            while let Some(mut chunk) = stream.recv_data().await.unwrap() {
                received.extend_from_slice(chunk.chunk());
                chunk.advance(chunk.remaining());
            }
            if let Some(sender) = received_tx.lock().unwrap().take() {
                let _ = sender.send(received);
            }
        }
    })
    .await;
    let body = http_body_util::StreamBody::new(futures_util::stream::once(async move {
        let data = release_rx.await.expect("upload release sender dropped");
        Ok::<_, aioduct::Error>(hyper::body::Frame::data(data))
    }))
    .boxed_unsync();

    let response = tokio::time::timeout(
        Duration::from_secs(1),
        client(None)
            .post(&format!("https://127.0.0.1:{}/accepted", addr.port()))
            .unwrap()
            .body_stream(body)
            .send(),
    )
    .await
    .expect("early response waited for the unfinished upload")
    .unwrap();

    let upload = Bytes::from_static(b"body sent after final response");
    release_tx.send(upload.clone()).unwrap();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), received_rx)
            .await
            .expect("detached upload did not finish")
            .unwrap(),
        upload
    );
    assert_eq!(response.text().await.unwrap(), "accepted");
}

#[tokio::test]
async fn h3_completed_upload_keeps_pending_response_read_alive() {
    let (headers_tx, headers_rx) = tokio::sync::oneshot::channel();
    let headers_tx = Arc::new(std::sync::Mutex::new(Some(headers_tx)));
    let (release_tx, release_rx) = tokio::sync::oneshot::channel::<Bytes>();
    let (addr, _, _) = aioduct_test_server::h3::h3_server_streaming(move |_request, mut stream| {
        let headers_tx = headers_tx.clone();
        async move {
            if let Some(sender) = headers_tx.lock().unwrap().take() {
                let _ = sender.send(());
            }
            while let Some(mut chunk) = stream.recv_data().await.unwrap() {
                chunk.advance(chunk.remaining());
            }
            stream
                .send_response(http::Response::builder().status(200).body(()).unwrap())
                .await
                .unwrap();
            stream
                .send_data(Bytes::from_static(b"complete"))
                .await
                .unwrap();
            stream.finish().await.unwrap();
        }
    })
    .await;
    let body = http_body_util::StreamBody::new(futures_util::stream::once(async move {
        let data = release_rx.await.expect("upload release sender dropped");
        Ok::<_, aioduct::Error>(hyper::body::Frame::data(data))
    }))
    .boxed_unsync();
    let request = tokio::spawn(async move {
        client(None)
            .post(&format!("https://127.0.0.1:{}/complete", addr.port()))
            .unwrap()
            .body_stream(body)
            .send()
            .await
            .unwrap()
    });

    headers_rx.await.unwrap();
    release_tx
        .send(Bytes::from_static(b"complete upload"))
        .unwrap();
    let response = tokio::time::timeout(Duration::from_secs(1), request)
        .await
        .expect("response did not follow the completed upload")
        .unwrap();
    assert_eq!(response.text().await.unwrap(), "complete");
}

#[tokio::test]
async fn h3_dropping_early_response_cancels_pending_upload() {
    let (reset_tx, reset_rx) = tokio::sync::oneshot::channel();
    let reset_tx = Arc::new(std::sync::Mutex::new(Some(reset_tx)));
    let (addr, _, _) = aioduct_test_server::h3::h3_server_streaming(move |_request, mut stream| {
        let reset_tx = reset_tx.clone();
        async move {
            stream
                .send_response(http::Response::builder().status(413).body(()).unwrap())
                .await
                .unwrap();
            let code = request_reset_code(&mut stream).await;
            if let Some(sender) = reset_tx.lock().unwrap().take() {
                let _ = sender.send(code);
            }
        }
    })
    .await;
    let dropped = Arc::new(AtomicBool::new(false));
    let body = PendingBody {
        dropped: dropped.clone(),
    }
    .boxed_unsync();

    let response = client(None)
        .post(&format!("https://127.0.0.1:{}/cancel", addr.port()))
        .unwrap()
        .body_stream(body)
        .send()
        .await
        .unwrap();
    drop(response);

    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), reset_rx)
            .await
            .expect("dropping the response did not cancel the upload")
            .unwrap(),
        h3::error::Code::H3_REQUEST_CANCELLED
    );
    assert!(dropped.load(Ordering::SeqCst));
}

#[tokio::test]
async fn h3_request_timeout_cancels_pending_upload() {
    let (reset_tx, reset_rx) = tokio::sync::oneshot::channel();
    let reset_tx = Arc::new(std::sync::Mutex::new(Some(reset_tx)));
    let (addr, _, _) = aioduct_test_server::h3::h3_server_streaming(move |_request, mut stream| {
        let reset_tx = reset_tx.clone();
        async move {
            let code = request_reset_code(&mut stream).await;
            if let Some(sender) = reset_tx.lock().unwrap().take() {
                let _ = sender.send(code);
            }
        }
    })
    .await;
    let dropped = Arc::new(AtomicBool::new(false));
    let body = PendingBody {
        dropped: dropped.clone(),
    }
    .boxed_unsync();
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .tls(aioduct::tls::RustlsConnector::danger_accept_invalid_certs())
        .http3(true)
        .unwrap()
        .timeout(Duration::from_millis(100))
        .build()
        .unwrap();

    let error = client
        .post(&format!("https://127.0.0.1:{}/timeout", addr.port()))
        .unwrap()
        .body_stream(body)
        .send()
        .await
        .unwrap_err();

    assert!(
        error.is_timeout(),
        "expected request timeout, got {error:?}"
    );
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), reset_rx)
            .await
            .expect("request timeout did not cancel the upload")
            .unwrap(),
        h3::error::Code::H3_REQUEST_CANCELLED
    );
    assert!(dropped.load(Ordering::SeqCst));
}

#[tokio::test]
async fn h3_no_error_stop_sending_preserves_early_response() {
    let (release_tx, release_rx) = tokio::sync::oneshot::channel::<Bytes>();
    let (addr, _, _) =
        aioduct_test_server::h3::h3_server_streaming(|_request, mut stream| async move {
            stream
                .send_response(http::Response::builder().status(413).body(()).unwrap())
                .await
                .unwrap();
            stream
                .send_data(Bytes::from_static(b"upload rejected"))
                .await
                .unwrap();
            stream.finish().await.unwrap();
            stream.stop_sending(h3::error::Code::H3_NO_ERROR);
        })
        .await;
    let body = http_body_util::StreamBody::new(futures_util::stream::once(async move {
        let data = release_rx.await.expect("upload release sender dropped");
        Ok::<_, aioduct::Error>(hyper::body::Frame::data(data))
    }))
    .boxed_unsync();

    let response = client(None)
        .post(&format!("https://127.0.0.1:{}/reject", addr.port()))
        .unwrap()
        .body_stream(body)
        .send()
        .await
        .unwrap();
    assert_eq!(
        release_tx
            .send(Bytes::from_static(b"ignored after peer stop"))
            .unwrap_err(),
        Bytes::from_static(b"ignored after peer stop")
    );

    assert_eq!(response.status(), http::StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(response.text().await.unwrap(), "upload rejected");
}

#[tokio::test]
async fn h3_detached_producer_timeout_resets_upload_without_replacing_response() {
    let (reset_tx, reset_rx) = tokio::sync::oneshot::channel();
    let reset_tx = Arc::new(std::sync::Mutex::new(Some(reset_tx)));
    let (addr, _, _) = aioduct_test_server::h3::h3_server_streaming(move |_request, mut stream| {
        let reset_tx = reset_tx.clone();
        async move {
            stream
                .send_response(http::Response::builder().status(202).body(()).unwrap())
                .await
                .unwrap();
            stream
                .send_data(Bytes::from_static(b"queued"))
                .await
                .unwrap();
            stream.finish().await.unwrap();
            let code = request_reset_code(&mut stream).await;
            if let Some(sender) = reset_tx.lock().unwrap().take() {
                let _ = sender.send(code);
            }
        }
    })
    .await;
    let dropped = Arc::new(AtomicBool::new(false));
    let body = PendingBody {
        dropped: dropped.clone(),
    }
    .boxed_unsync();

    let response = client(Some(Duration::from_millis(100)))
        .post(&format!("https://127.0.0.1:{}/queued", addr.port()))
        .unwrap()
        .body_stream(body)
        .send()
        .await
        .unwrap();

    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), reset_rx)
            .await
            .expect("detached producer ignored its write timeout")
            .unwrap(),
        h3::error::Code::H3_REQUEST_CANCELLED
    );
    assert!(dropped.load(Ordering::SeqCst));
    assert_eq!(response.text().await.unwrap(), "queued");
}
