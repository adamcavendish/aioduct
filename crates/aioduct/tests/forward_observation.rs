#![cfg(all(not(target_arch = "wasm32"), feature = "tokio"))]

use std::convert::Infallible;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use aioduct::runtime::tokio_rt::{TcpConnector, TokioIo, TokioRuntime};
use aioduct::{
    HttpEngineSend, Interest, NoopConnectionObservation, Observer, RequestFinish,
    RequestFinishReason, RequestInfo, RequestObservation, ResponseHeaders,
};
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response};
use tokio::net::TcpListener;

#[derive(Default)]
struct Records {
    events: Mutex<Vec<&'static str>>,
    request_bodies: Mutex<Vec<(u64, Option<u64>)>>,
    request_body_failures: Mutex<Vec<u64>>,
}

struct Recorder {
    records: Arc<Records>,
    interest: Interest,
}

struct Observation(Arc<Records>);

impl Observer for Recorder {
    type Context = ();
    type Observation = Observation;
    type ConnectionObservation = NoopConnectionObservation;

    fn begin_connection(&self) -> (Interest, Self::ConnectionObservation) {
        (Interest::EMPTY, NoopConnectionObservation)
    }

    fn begin(&self, _: RequestInfo<'_>, _: Option<()>) -> (Interest, Self::Observation) {
        (self.interest, Observation(self.records.clone()))
    }
}

impl RequestObservation for Observation {
    fn on_request_body(&self, event: &aioduct::RequestBodyEvent<'_>) {
        if let aioduct::RequestBodyEvent::Ended { attempt, bytes, .. } = event {
            self.0
                .request_bodies
                .lock()
                .unwrap()
                .push((*attempt, *bytes));
        } else if let aioduct::RequestBodyEvent::Failed { attempt, error, .. } = event
            && matches!(error, aioduct::Error::WriteTimeout)
        {
            self.0.request_body_failures.lock().unwrap().push(*attempt);
        }
        self.0.events.lock().unwrap().push("request-body");
    }

    fn on_response_headers(&self, _: &ResponseHeaders) {
        self.0.events.lock().unwrap().push("headers");
    }

    fn on_request_finish(&self, event: &RequestFinish<'_>) {
        let event_name = match event.reason {
            RequestFinishReason::Ended => "ended",
            RequestFinishReason::Failed { .. } => "failed",
            _ => panic!("unexpected forward finish reason: {:?}", event.reason),
        };
        self.0.events.lock().unwrap().push(event_name);
    }
}

async fn upstream(body: &'static [u8]) -> http::Uri {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        http1::Builder::new()
            .serve_connection(
                TokioIo::new(stream),
                service_fn(move |_: Request<hyper::body::Incoming>| async move {
                    Ok::<_, Infallible>(Response::new(Full::new(Bytes::from_static(body))))
                }),
            )
            .await
            .unwrap();
    });
    format!("http://{address}/").parse().unwrap()
}

async fn upstream_with_request_body() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        http1::Builder::new()
            .serve_connection(
                TokioIo::new(stream),
                service_fn(|request: Request<hyper::body::Incoming>| async move {
                    let _ = request.into_body().collect().await.unwrap();
                    Ok::<_, Infallible>(Response::new(Full::new(Bytes::from_static(b"ok"))))
                }),
            )
            .await
            .unwrap();
    });
    format!("http://{address}/")
}

async fn upstream_with_request_body_retry(counter: Arc<AtomicUsize>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        http1::Builder::new()
            .serve_connection(
                TokioIo::new(stream),
                service_fn(move |request: Request<hyper::body::Incoming>| {
                    let counter = counter.clone();
                    async move {
                        let _ = request.into_body().collect().await.unwrap();
                        let status = if counter.fetch_add(1, Ordering::Relaxed) == 0 {
                            http::StatusCode::INTERNAL_SERVER_ERROR
                        } else {
                            http::StatusCode::OK
                        };
                        Ok::<_, Infallible>(
                            Response::builder()
                                .status(status)
                                .body(Full::new(Bytes::from_static(b"ok")))
                                .unwrap(),
                        )
                    }
                }),
            )
            .await
            .unwrap();
    });
    format!("http://{address}/")
}

fn incoming_request() -> http::Request<Full<Bytes>> {
    http::Request::builder()
        .method("GET")
        .uri("/forwarded")
        .header(http::header::HOST, "downstream.test")
        .body(Full::new(Bytes::new()))
        .unwrap()
}

fn incoming_request_with_body() -> http::Request<Full<Bytes>> {
    http::Request::builder()
        .method("POST")
        .uri("/forwarded")
        .header(http::header::HOST, "downstream.test")
        .body(Full::new(Bytes::from_static(b"payload")))
        .unwrap()
}

#[tokio::test]
async fn forward_reports_headers_before_successful_body_completion() {
    let records = Arc::new(Records::default());
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .observer(Recorder {
            records: records.clone(),
            interest: Interest::RESPONSE_HEADERS | Interest::REQUEST_FINISH,
        })
        .build()
        .unwrap();
    let response = client
        .forward(incoming_request())
        .upstream(upstream(b"forwarded").await)
        .send()
        .await
        .unwrap();

    assert_eq!(response.text().await.unwrap(), "forwarded");
    assert_eq!(*records.events.lock().unwrap(), vec!["headers", "ended"]);
}

#[tokio::test]
async fn forward_processing_failure_keeps_headers_and_finishes_failed() {
    let records = Arc::new(Records::default());
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .observer(Recorder {
            records: records.clone(),
            interest: Interest::RESPONSE_HEADERS | Interest::REQUEST_FINISH,
        })
        .build()
        .unwrap();

    let error = client
        .forward(incoming_request())
        .upstream(upstream(b"forwarded").await)
        .response_content_digest(1)
        .send()
        .await
        .unwrap_err();
    assert!(matches!(error, aioduct::Error::Unsupported(_)));
    assert_eq!(*records.events.lock().unwrap(), vec!["headers", "failed"]);
}

#[tokio::test]
async fn forward_reports_request_body_source_completion_without_frame_callbacks() {
    let records = Arc::new(Records::default());
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .observer(Recorder {
            records: records.clone(),
            interest: Interest::REQUEST_BODY | Interest::REQUEST_FINISH,
        })
        .build()
        .unwrap();

    let response = client
        .forward(incoming_request_with_body())
        .upstream(upstream(b"ok").await)
        .send()
        .await
        .unwrap();
    assert_eq!(response.text().await.unwrap(), "ok");

    let events = records.events.lock().unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|event| **event == "request-body")
            .count(),
        1
    );
    assert_eq!(events.iter().filter(|event| **event == "ended").count(), 1);
}

#[tokio::test]
async fn direct_buffered_request_reports_source_completion_with_attempt() {
    let records = Arc::new(Records::default());
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .observer(Recorder {
            records: records.clone(),
            interest: Interest::REQUEST_BODY | Interest::REQUEST_FINISH,
        })
        .build()
        .unwrap();

    let response = client
        .post(&upstream_with_request_body().await)
        .unwrap()
        .body(Bytes::from_static(b"payload"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.text().await.unwrap(), "ok");
    assert_eq!(*records.request_bodies.lock().unwrap(), vec![(1, Some(7))]);
}

#[tokio::test]
async fn direct_streaming_request_reports_source_completion_once() {
    let records = Arc::new(Records::default());
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .observer(Recorder {
            records: records.clone(),
            interest: Interest::REQUEST_BODY | Interest::REQUEST_FINISH,
        })
        .build()
        .unwrap();

    let body: aioduct::body::RequestBodySend = Full::new(Bytes::from_static(b"streamed"))
        .map_err(|never| match never {})
        .boxed_unsync();
    let response = client
        .post(&upstream_with_request_body().await)
        .unwrap()
        .body_stream(body)
        .send()
        .await
        .unwrap();
    assert_eq!(response.text().await.unwrap(), "ok");
    assert_eq!(*records.request_bodies.lock().unwrap(), vec![(1, Some(8))]);
}

#[tokio::test]
async fn direct_retried_request_attributes_source_completion_to_each_attempt() {
    let records = Arc::new(Records::default());
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .observer(Recorder {
            records: records.clone(),
            interest: Interest::REQUEST_BODY | Interest::REQUEST_FINISH,
        })
        .retry(
            aioduct::RetryConfig::default()
                .max_retries(1)
                .initial_backoff(Duration::ZERO),
        )
        .build()
        .unwrap();

    let response = client
        .put(&upstream_with_request_body_retry(Arc::new(AtomicUsize::new(0))).await)
        .unwrap()
        .body(Bytes::from_static(b"payload"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.text().await.unwrap(), "ok");
    assert_eq!(
        *records.request_bodies.lock().unwrap(),
        vec![(1, Some(7)), (2, Some(7))]
    );
}

#[tokio::test]
async fn direct_request_write_timeout_reports_source_failure_with_attempt() {
    struct PendingBody;
    impl http_body::Body for PendingBody {
        type Data = Bytes;
        type Error = aioduct::Error;

        fn poll_frame(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Option<Result<http_body::Frame<Self::Data>, Self::Error>>> {
            std::task::Poll::Pending
        }

        fn is_end_stream(&self) -> bool {
            false
        }
    }

    let records = Arc::new(Records::default());
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .observer(Recorder {
            records: records.clone(),
            interest: Interest::REQUEST_BODY | Interest::REQUEST_FINISH,
        })
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let body: aioduct::body::RequestBodySend = PendingBody.boxed_unsync();

    let error = client
        .post(&upstream_with_request_body().await)
        .unwrap()
        .body_stream(body)
        .write_timeout(Duration::from_millis(1))
        .send()
        .await
        .unwrap_err();
    assert!(matches!(error.error(), aioduct::Error::Hyper(_)));
    assert_eq!(*records.request_body_failures.lock().unwrap(), vec![1]);
}
