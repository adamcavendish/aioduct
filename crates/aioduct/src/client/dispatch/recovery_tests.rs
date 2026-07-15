use super::*;
use crate::body::RequestBodySend;
use crate::client::{BodyReplayability, HttpEngineSend};
use crate::observer::{PoolOutcome, RequestEvent, RequestObserver, RequestPhase};
use crate::pool::{PoolKey, PooledConnection, ProtocolHint, ProxyRoute};
use crate::runtime::executor::poll_executor;
use crate::runtime::tokio_rt::{TcpConnector, TokioIo, TokioRuntime};
use http_body_util::BodyExt as _;
use std::future::Future as _;
use std::sync::{Arc, Mutex};
use std::task::Poll;

type Core = HttpEngineCore<RequestBodySend>;

fn test_request_body(bytes: &'static [u8]) -> RequestBodySend {
    http_body_util::Full::new(bytes::Bytes::from_static(bytes))
        .map_err(|never| match never {})
        .boxed_unsync()
}

async fn assert_recovered_request(
    result: Result<Response, PooledSendError<RequestBodySend>>,
    expected_uri: &str,
) {
    let recovered = match result {
        Err(PooledSendError::Recovered { request, .. }) => *request,
        Ok(_) => panic!("closed pooled dispatcher unexpectedly returned a response"),
        Err(PooledSendError::Failed(error)) => {
            panic!("closed pooled dispatcher did not return the exact request: {error}")
        }
    };

    assert_eq!(recovered.method(), http::Method::POST);
    assert_eq!(recovered.uri(), expected_uri);
    assert_eq!(
        recovered.into_body().collect().await.unwrap().to_bytes(),
        bytes::Bytes::from_static(b"one-shot-upload")
    );
}

#[tokio::test]
async fn pooled_h1_rejection_recovers_exact_unsent_request() {
    let (client_io, server_io) = tokio::io::duplex(8192);
    let (sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(client_io))
        .await
        .expect("h1 handshake");
    drop(connection);
    drop(server_io);

    let mut pooled = PooledConnection::new_h1(sender);
    let request = http::Request::post("/upload")
        .body(test_request_body(b"one-shot-upload"))
        .unwrap();
    let uri = "http://example.com/upload".parse().unwrap();

    let result = Core::try_send_on_pooled_connection(&mut pooled, request, uri).await;
    assert_recovered_request(result, "/upload").await;
    assert_eq!(pooled.requests_served(), 0);
}

#[tokio::test]
async fn pooled_h2_rejection_recovers_exact_unsent_request() {
    let (client_io, server_io) = tokio::io::duplex(65536);
    let server = tokio::spawn(async move {
        let _ = hyper::server::conn::http2::Builder::new(poll_executor::<TokioRuntime>())
            .serve_connection(
                TokioIo::new(server_io),
                hyper::service::service_fn(|_request| async {
                    Ok::<_, std::convert::Infallible>(hyper::Response::new(
                        http_body_util::Empty::<bytes::Bytes>::new(),
                    ))
                }),
            )
            .await;
    });

    let (sender, connection) = hyper::client::conn::http2::handshake(
        poll_executor::<TokioRuntime>(),
        TokioIo::new(client_io),
    )
    .await
    .expect("h2 handshake");
    drop(connection);

    let mut pooled = PooledConnection::new_h2(sender);
    let request = http::Request::post("/upload")
        .body(test_request_body(b"one-shot-upload"))
        .unwrap();
    let uri = "https://example.com/upload".parse().unwrap();

    let result = Core::try_send_on_pooled_connection(&mut pooled, request, uri).await;
    assert_recovered_request(result, "https://example.com/upload").await;
    assert_eq!(pooled.requests_served(), 0);
    server.abort();
}

#[tokio::test]
async fn pooled_h1_failure_after_serialization_does_not_recover_request() {
    use tokio::io::AsyncReadExt as _;

    let (client_io, mut server_io) = tokio::io::duplex(8192);
    let server = tokio::spawn(async move {
        let mut received = Vec::new();
        let mut buf = [0_u8; 1024];
        while !received
            .windows(b"one-shot-upload".len())
            .any(|window| window == b"one-shot-upload")
        {
            let read = server_io.read(&mut buf).await.unwrap();
            assert_ne!(read, 0, "client closed before serializing the request body");
            received.extend_from_slice(&buf[..read]);
        }
    });

    let (sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(client_io))
        .await
        .expect("h1 handshake");
    let driver = tokio::spawn(async move {
        let _ = connection.await;
    });

    let mut pooled = PooledConnection::new_h1(sender);
    let request = http::Request::post("/upload")
        .header(http::header::HOST, "example.com")
        .header(http::header::CONTENT_LENGTH, "15")
        .body(test_request_body(b"one-shot-upload"))
        .unwrap();
    let uri = "http://example.com/upload".parse().unwrap();

    let result = Core::try_send_on_pooled_connection(&mut pooled, request, uri).await;
    match result {
        Err(PooledSendError::Failed(_)) => {}
        Err(PooledSendError::Recovered { .. }) => {
            panic!("request was recovered after its body reached the transport")
        }
        Ok(_) => panic!("server closed without sending a response"),
    }
    assert_eq!(pooled.requests_served(), 1);
    server.await.unwrap();
    driver.await.unwrap();
}

struct DropDispatchOnHit {
    dispatch: Mutex<Option<Box<dyn std::any::Any + Send>>>,
}

impl RequestObserver for DropDispatchOnHit {
    fn on_event(&self, event: &RequestEvent) {
        if matches!(
            event.phase,
            RequestPhase::PoolCheckoutComplete {
                outcome: PoolOutcome::Hit,
                ..
            }
        ) {
            drop(self.dispatch.lock().unwrap().take());
        }
    }

    fn on_connection_event(&self, _event: &crate::observer::ConnectionEvent) {}
}

#[tokio::test]
async fn exact_h1_recovery_dispatches_one_shot_body_fresh_once() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (received_tx, received_rx) = tokio::sync::oneshot::channel();
    let received_tx = Arc::new(Mutex::new(Some(received_tx)));
    let origin = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let received_tx = received_tx.clone();
        hyper::server::conn::http1::Builder::new()
            .serve_connection(
                TokioIo::new(stream),
                hyper::service::service_fn(move |request: http::Request<hyper::body::Incoming>| {
                    let received_tx = received_tx.clone();
                    async move {
                        let body = request.into_body().collect().await.unwrap().to_bytes();
                        if let Some(sender) = received_tx.lock().unwrap().take() {
                            let _ = sender.send(body);
                        }
                        Ok::<_, std::convert::Infallible>(
                            hyper::Response::builder()
                                .header(http::header::CONNECTION, "close")
                                .body(http_body_util::Full::new(bytes::Bytes::from_static(
                                    b"fresh",
                                )))
                                .unwrap(),
                        )
                    }
                }),
            )
            .await
            .unwrap();
    });

    // Make the sender ready at checkout, then drop its connection driver from
    // the checkout observer to force the readiness-to-dispatch race.
    let (client_io, server_io) = tokio::io::duplex(8192);
    let (sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(client_io))
        .await
        .expect("h1 handshake");
    let mut connection = Box::pin(connection);
    std::future::poll_fn(|cx| {
        assert!(connection.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;

    let observer = DropDispatchOnHit {
        dispatch: Mutex::new(Some(Box::new((connection, server_io)))),
    };
    let engine = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .request_observer(observer)
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .unwrap();

    let uri: http::Uri = format!("http://{addr}/upload").parse().unwrap();
    let pool_key = PoolKey::with_hint_and_route(
        http::uri::Scheme::HTTP,
        uri.authority().unwrap().clone(),
        ProtocolHint::Auto,
        ProxyRoute::DIRECT,
    );
    engine
        .core
        .pool
        .checkin(pool_key, PooledConnection::new_h1(sender));

    let stream = futures_util::stream::iter([Ok::<_, crate::Error>(hyper::body::Frame::data(
        bytes::Bytes::from_static(b"one-shot-upload"),
    ))]);
    let body: RequestBodySend = http_body_util::StreamBody::new(stream).boxed_unsync();
    let request = http::Request::post("/upload")
        .header(http::header::HOST, addr.to_string())
        .header(http::header::CONTENT_LENGTH, "15")
        .body(body)
        .unwrap();

    let response = engine
        .execute_single_with_hint_send(
            request,
            &uri,
            ProtocolHint::Auto,
            None,
            Some(std::time::Duration::from_secs(5)),
            None,
            Some(std::time::Duration::from_secs(5)),
            None,
            false,
            BodyReplayability::OneShot,
        )
        .await
        .unwrap();

    assert_eq!(response.text().await.unwrap(), "fresh");
    assert_eq!(
        received_rx.await.unwrap(),
        bytes::Bytes::from_static(b"one-shot-upload")
    );
    assert_eq!(engine.pool_stats().stale_reuse_retries, 1);
    origin.await.unwrap();
}
