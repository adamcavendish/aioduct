use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::{Buf, Bytes};
use http_body_util::BodyExt as _;

use aioduct::HttpEngineSend;
use aioduct::runtime::TokioRuntime;
use aioduct::runtime::tokio_rt::TcpConnector;

#[tokio::test]
async fn h3_sends_headers_before_waiting_for_streaming_body() {
    let (headers_tx, headers_rx) = tokio::sync::oneshot::channel();
    let headers_tx = Arc::new(Mutex::new(Some(headers_tx)));
    let server_headers_tx = headers_tx.clone();
    let (addr, _cert, _counter) =
        aioduct_test_server::h3::h3_server_streaming(move |request, mut stream| {
            let headers_tx = server_headers_tx.clone();
            async move {
                if let Some(sender) = headers_tx.lock().unwrap().take() {
                    let _ =
                        sender.send((request.method().clone(), request.uri().path().to_owned()));
                }

                let mut body = Vec::new();
                while let Some(mut chunk) = stream.recv_data().await.unwrap() {
                    body.extend_from_slice(chunk.chunk());
                    chunk.advance(chunk.remaining());
                }
                assert_eq!(body, b"gated upload");

                let response = http::Response::builder().status(200).body(()).unwrap();
                stream.send_response(response).await.unwrap();
                stream.finish().await.unwrap();
            }
        })
        .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .tls(aioduct::tls::RustlsConnector::danger_accept_invalid_certs())
        .http3(true)
        .unwrap()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();

    let (body_tx, body_rx) = tokio::sync::oneshot::channel();
    let body_stream = futures_util::stream::once(async move {
        body_rx.await.unwrap();
        Ok::<_, aioduct::Error>(hyper::body::Frame::data(Bytes::from_static(
            b"gated upload",
        )))
    });
    let body = http_body_util::StreamBody::new(body_stream).boxed_unsync();
    let url = format!("https://127.0.0.1:{}/upload", addr.port());
    let request =
        tokio::spawn(async move { client.post(&url).unwrap().body_stream(body).send().await });

    let (method, path) = tokio::time::timeout(Duration::from_secs(1), headers_rx)
        .await
        .expect("HTTP/3 headers were blocked behind the body producer")
        .unwrap();
    assert_eq!(method, http::Method::POST);
    assert_eq!(path, "/upload");

    body_tx.send(()).unwrap();
    let response = request.await.unwrap().unwrap();
    assert_eq!(response.status(), http::StatusCode::OK);
    assert_eq!(response.version(), http::Version::HTTP_3);
}

#[tokio::test]
async fn h3_streams_ordered_chunks_without_draining_the_producer_first() {
    const CHUNKS: u32 = 64;

    let (ack_tx, ack_rx) = tokio::sync::mpsc::channel::<()>(1);
    let ack_tx = Arc::new(tokio::sync::Mutex::new(ack_tx));
    let server_ack_tx = ack_tx.clone();
    let (addr, _cert, _counter) =
        aioduct_test_server::h3::h3_server_streaming(move |_request, mut stream| {
            let ack_tx = server_ack_tx.clone();
            async move {
                let mut expected = 0_u32;
                while let Some(mut chunk) = stream.recv_data().await.unwrap() {
                    assert_eq!(chunk.remaining(), std::mem::size_of::<u32>());
                    assert_eq!(chunk.get_u32(), expected);
                    expected += 1;
                    ack_tx.lock().await.send(()).await.unwrap();
                }
                assert_eq!(expected, CHUNKS);

                let response = http::Response::builder().status(200).body(()).unwrap();
                stream.send_response(response).await.unwrap();
                stream.finish().await.unwrap();
            }
        })
        .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .tls(aioduct::tls::RustlsConnector::danger_accept_invalid_certs())
        .http3(true)
        .unwrap()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();

    let body_stream = futures_util::stream::unfold(
        (0_u32, ack_rx),
        |(index, mut acknowledgements)| async move {
            if index > 0 {
                acknowledgements.recv().await.unwrap();
            }
            if index == CHUNKS {
                return None;
            }
            Some((
                Ok::<_, aioduct::Error>(hyper::body::Frame::data(Bytes::copy_from_slice(
                    &index.to_be_bytes(),
                ))),
                (index + 1, acknowledgements),
            ))
        },
    );
    let body = http_body_util::StreamBody::new(body_stream).boxed_unsync();
    let response = client
        .post(&format!("https://127.0.0.1:{}/chunks", addr.port()))
        .unwrap()
        .body_stream(body)
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), http::StatusCode::OK);
}
