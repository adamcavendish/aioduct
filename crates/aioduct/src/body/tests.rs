use super::*;
use crate::response::ResponseBodySend;

use bytes::Bytes;
use http_body::Body;
use http_body_util::BodyExt;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

fn buffered(data: &[u8]) -> RequestBody {
    RequestBody::Buffered(Bytes::from(data.to_vec()))
}

fn streaming() -> RequestBody {
    let body: RequestBodySend = http_body_util::Empty::new()
        .map_err(|never| match never {})
        .boxed_unsync();
    RequestBody::Streaming(body)
}

#[derive(Clone, Default)]
struct UploadRecorder(Arc<Mutex<Vec<(u64, u64)>>>);

impl crate::observer::Observer for UploadRecorder {
    type Context = ();
    type Observation = Self;
    type ConnectionObservation = crate::observer::NoopConnectionObservation;
    fn begin(
        &self,
        _: crate::observer::RequestInfo<'_>,
        _: Option<()>,
    ) -> (crate::observer::Interest, Self) {
        (crate::observer::Interest::REQUEST_BODY, self.clone())
    }

    fn begin_connection(&self) -> (crate::observer::Interest, Self::ConnectionObservation) {
        (
            crate::observer::Interest::EMPTY,
            crate::observer::NoopConnectionObservation,
        )
    }
}

impl crate::observer::RequestObservation for UploadRecorder {
    fn on_request_body(&self, event: &crate::observer::RequestBodyEvent<'_>) {
        if let crate::observer::RequestBodyEvent::Ended {
            attempt,
            bytes: Some(bytes),
            ..
        } = event
        {
            self.0.lock().unwrap().push((*attempt, *bytes));
        }
    }
}

#[test]
fn delayed_send_upload_completion_keeps_its_original_attempt() {
    let recorder = UploadRecorder::default();
    let uri = "http://localhost/".parse().unwrap();
    let handle = crate::observer::SendRequestObservationHandle::new(
        &recorder,
        crate::observer::RequestInfo {
            method: &http::Method::POST,
            initial_uri: &uri,
        },
        None,
    )
    .unwrap();
    let counter = handle.attempt_counter().unwrap();
    counter.started();
    let body = http_body_util::Full::new(Bytes::from_static(b"source"))
        .map_err(|never| match never {})
        .boxed_unsync();
    let mut source = ObservedRequestBodySend::new(body, handle.attempt_observer().unwrap());
    counter.started(); // A later attempt can begin before this source finishes.
    let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
    assert!(matches!(
        std::pin::Pin::new(&mut source).poll_frame(&mut cx),
        std::task::Poll::Ready(Some(Ok(_)))
    ));
    assert_eq!(*recorder.0.lock().unwrap(), vec![(1, 6)]);
}

#[test]
fn delayed_local_upload_completion_keeps_its_original_attempt() {
    let recorder = UploadRecorder::default();
    let uri = "http://localhost/".parse().unwrap();
    let handle = crate::observer::LocalRequestObservationHandle::new(
        &recorder,
        crate::observer::RequestInfo {
            method: &http::Method::POST,
            initial_uri: &uri,
        },
        None,
    )
    .unwrap();
    let counter = handle.attempt_counter().unwrap();
    counter.started();
    let body: RequestBodyLocal = Box::pin(
        http_body_util::Full::new(Bytes::from_static(b"source")).map_err(|never| match never {}),
    );
    let mut source = ObservedRequestBodyLocal::new(body, handle.attempt_observer().unwrap());
    counter.started();
    let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
    assert!(matches!(
        std::pin::Pin::new(&mut source).poll_frame(&mut cx),
        std::task::Poll::Ready(Some(Ok(_)))
    ));
    assert_eq!(*recorder.0.lock().unwrap(), vec![(1, 6)]);
}

struct LocalEndRecorder(Rc<RefCell<Vec<LocalBodyEnd>>>);

impl LocalBodyObserver for LocalEndRecorder {
    fn on_body_end(&self, end: LocalBodyEnd) {
        self.0.borrow_mut().push(end);
    }
}

struct SendEndRecorder(Arc<Mutex<Vec<BodyEnd>>>);

impl SendBodyObserver for SendEndRecorder {
    fn on_body_end(&self, end: BodyEnd) {
        self.0.lock().unwrap().push(end);
    }
}

fn observed_send_body(body: RequestBodySend, ends: Arc<Mutex<Vec<BodyEnd>>>) -> ResponseBodySend {
    ResponseBodySend::with_owner(
        body,
        Some(SendObservationHandle::new(Arc::new(SendEndRecorder(ends)))),
    )
}

#[test]
fn send_observation_finishes_once_on_eof() {
    let ends = Arc::new(Mutex::new(Vec::new()));
    let body = http_body_util::Full::new(Bytes::from_static(b"send"))
        .map_err(|never| match never {})
        .boxed_unsync();
    let mut body = observed_send_body(body, ends.clone());
    let waker = std::task::Waker::noop();
    let mut cx = std::task::Context::from_waker(waker);

    assert!(matches!(
        std::pin::Pin::new(&mut body).poll_frame(&mut cx),
        std::task::Poll::Ready(Some(Ok(_)))
    ));
    assert!(matches!(
        std::pin::Pin::new(&mut body).poll_frame(&mut cx),
        std::task::Poll::Ready(None)
    ));
    assert!(matches!(
        std::pin::Pin::new(&mut body).poll_frame(&mut cx),
        std::task::Poll::Ready(None)
    ));
    drop(body);

    assert_eq!(&*ends.lock().unwrap(), &[BodyEnd::Complete]);
}

#[test]
fn send_observation_is_abandoned_when_carrier_is_dropped() {
    let ends = Arc::new(Mutex::new(Vec::new()));
    let body = http_body_util::Empty::<Bytes>::new()
        .map_err(|never| match never {})
        .boxed_unsync();
    drop(observed_send_body(body, ends.clone()));

    assert_eq!(&*ends.lock().unwrap(), &[BodyEnd::Abandoned]);
}

#[test]
fn send_observation_handoff_is_terminal() {
    let ends = Arc::new(Mutex::new(Vec::new()));
    let body = http_body_util::Empty::<Bytes>::new()
        .map_err(|never| match never {})
        .boxed_unsync();
    let mut body = observed_send_body(body, ends.clone());
    body.finish_handoff();
    drop(body);

    assert_eq!(&*ends.lock().unwrap(), &[BodyEnd::HandedOff]);
}

fn observed_local_body(
    body: impl http_body::Body<Data = Bytes, Error = crate::error::Error> + 'static,
    ends: Rc<RefCell<Vec<LocalBodyEnd>>>,
) -> ResponseBodyLocal {
    ResponseBodyLocal::with_owner(
        Box::pin(body),
        Some(LocalObservationHandle::new(Rc::new(LocalEndRecorder(ends)))),
    )
}

#[test]
fn local_observation_finishes_once_on_eof() {
    let ends = Rc::new(RefCell::new(Vec::new()));
    let mut body = observed_local_body(
        http_body_util::Full::new(Bytes::from_static(b"local")).map_err(|never| match never {}),
        ends.clone(),
    );
    let waker = std::task::Waker::noop();
    let mut cx = std::task::Context::from_waker(waker);

    assert!(matches!(
        std::pin::Pin::new(&mut body).poll_frame(&mut cx),
        std::task::Poll::Ready(Some(Ok(_)))
    ));
    assert!(matches!(
        std::pin::Pin::new(&mut body).poll_frame(&mut cx),
        std::task::Poll::Ready(None)
    ));
    assert!(matches!(
        std::pin::Pin::new(&mut body).poll_frame(&mut cx),
        std::task::Poll::Ready(None)
    ));
    drop(body);

    assert_eq!(&*ends.borrow(), &[LocalBodyEnd::Complete]);
}

struct LocalErrorBody;

impl http_body::Body for LocalErrorBody {
    type Data = Bytes;
    type Error = crate::error::Error;

    fn poll_frame(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<http_body::Frame<Self::Data>, Self::Error>>> {
        std::task::Poll::Ready(Some(Err(crate::error::Error::Other(
            "local body error".into(),
        ))))
    }
}

#[test]
fn local_observation_finishes_once_on_error() {
    let ends = Rc::new(RefCell::new(Vec::new()));
    let mut body = observed_local_body(LocalErrorBody, ends.clone());
    let waker = std::task::Waker::noop();
    let mut cx = std::task::Context::from_waker(waker);

    assert!(matches!(
        std::pin::Pin::new(&mut body).poll_frame(&mut cx),
        std::task::Poll::Ready(Some(Err(_)))
    ));
    drop(body);

    assert_eq!(&*ends.borrow(), &[LocalBodyEnd::Error]);
}

#[test]
fn local_observation_reports_abandoned_when_body_is_dropped() {
    let ends = Rc::new(RefCell::new(Vec::new()));
    let body = observed_local_body(
        http_body_util::Empty::<Bytes>::new().map_err(|never| match never {}),
        ends.clone(),
    );
    let (_inner, owner) = body.into_parts();
    drop(owner);

    assert_eq!(&*ends.borrow(), &[LocalBodyEnd::Abandoned]);
}

#[test]
fn local_observation_reports_handoff_once() {
    let ends = Rc::new(RefCell::new(Vec::new()));
    let mut body = observed_local_body(
        http_body_util::Empty::<Bytes>::new().map_err(|never| match never {}),
        ends.clone(),
    );
    body.finish_handoff();
    body.finish_handoff();
    drop(body);

    assert_eq!(&*ends.borrow(), &[LocalBodyEnd::HandedOff]);
}

#[test]
fn try_clone_buffered_returns_some() {
    let body = buffered(b"hello");
    let cloned = body.try_clone();
    assert!(cloned.is_some());
    match cloned.unwrap() {
        RequestBody::Buffered(b) => assert_eq!(&b[..], b"hello"),
        _ => panic!("expected Buffered"),
    }
}

#[test]
fn try_clone_streaming_returns_none() {
    let body = streaming();
    assert!(body.try_clone().is_none());
}

#[test]
fn from_bytes() {
    let body: RequestBody = Bytes::from_static(b"data").into();
    match body {
        RequestBody::Buffered(b) => assert_eq!(&b[..], b"data"),
        _ => panic!("expected Buffered"),
    }
}

#[test]
fn from_vec() {
    let body: RequestBody = vec![1u8, 2, 3].into();
    match body {
        RequestBody::Buffered(b) => assert_eq!(&b[..], &[1, 2, 3]),
        _ => panic!("expected Buffered"),
    }
}

#[test]
fn from_string() {
    let body: RequestBody = String::from("text").into();
    match body {
        RequestBody::Buffered(b) => assert_eq!(&b[..], b"text"),
        _ => panic!("expected Buffered"),
    }
}

#[test]
fn from_static_str() {
    let body: RequestBody = "static".into();
    match body {
        RequestBody::Buffered(b) => assert_eq!(&b[..], b"static"),
        _ => panic!("expected Buffered"),
    }
}

#[test]
fn from_static_bytes() {
    let body: RequestBody = (b"bytes" as &'static [u8]).into();
    match body {
        RequestBody::Buffered(b) => assert_eq!(&b[..], b"bytes"),
        _ => panic!("expected Buffered"),
    }
}

#[test]
fn from_hyper_body_is_streaming() {
    let hyper_body: RequestBodySend = http_body_util::Empty::new()
        .map_err(|never| match never {})
        .boxed_unsync();
    let body: RequestBody = hyper_body.into();
    assert!(body.try_clone().is_none());
}

#[test]
fn debug_buffered() {
    let body = buffered(b"data");
    let dbg = format!("{body:?}");
    assert!(dbg.contains("Buffered"));
}

#[test]
fn debug_streaming() {
    let body = streaming();
    let dbg = format!("{body:?}");
    assert!(dbg.contains("Streaming"));
}

#[test]
fn body_stream_debug() {
    let hyper_body: RequestBodySend = http_body_util::Empty::new()
        .map_err(|never| match never {})
        .boxed_unsync();
    let stream = BodyStreamSend::new(hyper_body);
    let dbg = format!("{stream:?}");
    assert!(dbg.contains("BodyStreamSend"));
}

#[tokio::test]
async fn body_stream_empty_returns_none() {
    let hyper_body: RequestBodySend = http_body_util::Empty::new()
        .map_err(|never| match never {})
        .boxed_unsync();
    let mut stream = BodyStreamSend::new(hyper_body);
    assert!(stream.next().await.is_none());
}

#[tokio::test]
async fn body_stream_with_data() {
    let hyper_body: RequestBodySend = http_body_util::Full::new(Bytes::from("hello"))
        .map_err(|never| match never {})
        .boxed_unsync();
    let mut stream = BodyStreamSend::new(hyper_body);
    let chunk = stream.next().await.unwrap().unwrap();
    assert_eq!(&chunk[..], b"hello");
    assert!(stream.next().await.is_none());
}

#[tokio::test]
async fn body_stream_done_stays_none() {
    let hyper_body: RequestBodySend = http_body_util::Empty::new()
        .map_err(|never| match never {})
        .boxed_unsync();
    let mut stream = BodyStreamSend::new(hyper_body);
    assert!(stream.next().await.is_none());
    assert!(stream.next().await.is_none());
}

#[test]
fn into_local_body_buffered() {
    let body = buffered(b"local_test");
    let local = body.into_local_body();
    // Verify it's a valid body by checking size_hint
    use http_body::Body;
    let hint = local.size_hint();
    assert_eq!(hint.exact(), Some(10));
}

#[test]
fn into_local_body_streaming() {
    let body = streaming();
    let local = body.into_local_body();
    // Streaming empty body has size 0
    use http_body::Body;
    let hint = local.size_hint();
    assert_eq!(hint.exact(), Some(0));
}

#[tokio::test]
async fn body_stream_error_propagates_and_marks_done() {
    use std::pin::Pin;
    use std::task::{Context, Poll};

    struct ErrorAfterFirst {
        sent: bool,
    }

    impl http_body::Body for ErrorAfterFirst {
        type Data = Bytes;
        type Error = Error;
        fn poll_frame(
            mut self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<Option<Result<http_body::Frame<Self::Data>, Self::Error>>> {
            if !self.sent {
                self.sent = true;
                Poll::Ready(Some(Err(Error::Other("deliberate error".into()))))
            } else {
                Poll::Ready(None)
            }
        }
    }

    let hyper_body: RequestBodySend = ErrorAfterFirst { sent: false }.boxed_unsync();
    let mut stream = BodyStreamSend::new(hyper_body);

    // First call should return the error
    let result = stream.next().await;
    assert!(result.is_some());
    assert!(result.unwrap().is_err());

    // After error, stream is done
    assert!(stream.next().await.is_none());
}

#[tokio::test]
async fn body_stream_skips_non_data_frames() {
    use std::pin::Pin;
    use std::task::{Context, Poll};

    struct TrailerThenData {
        state: u8,
    }

    impl http_body::Body for TrailerThenData {
        type Data = Bytes;
        type Error = Error;
        fn poll_frame(
            mut self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<Option<Result<http_body::Frame<Self::Data>, Self::Error>>> {
            match self.state {
                0 => {
                    self.state = 1;
                    // Emit a trailers frame (non-data)
                    let mut trailers = http::HeaderMap::new();
                    trailers.insert("x-test", "val".parse().unwrap());
                    Poll::Ready(Some(Ok(http_body::Frame::trailers(trailers))))
                }
                1 => {
                    self.state = 2;
                    Poll::Ready(Some(Ok(http_body::Frame::data(Bytes::from(
                        "after_trailer",
                    )))))
                }
                _ => Poll::Ready(None),
            }
        }
    }

    let hyper_body: RequestBodySend = TrailerThenData { state: 0 }.boxed_unsync();
    let mut stream = BodyStreamSend::new(hyper_body);

    // The trailers frame should be skipped, only data returned
    let chunk = stream.next().await.unwrap().unwrap();
    assert_eq!(&chunk[..], b"after_trailer");

    // Then done
    assert!(stream.next().await.is_none());
}

#[tokio::test]
async fn body_stream_exposes_trailers_after_drain() {
    use std::pin::Pin;
    use std::task::{Context, Poll};

    // Realistic order: data frame, then a trailers frame, then end.
    struct DataThenTrailer {
        state: u8,
    }

    impl http_body::Body for DataThenTrailer {
        type Data = Bytes;
        type Error = Error;
        fn poll_frame(
            mut self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<Option<Result<http_body::Frame<Self::Data>, Self::Error>>> {
            match self.state {
                0 => {
                    self.state = 1;
                    Poll::Ready(Some(Ok(http_body::Frame::data(Bytes::from("payload")))))
                }
                1 => {
                    self.state = 2;
                    let mut t = http::HeaderMap::new();
                    t.insert("x-checksum", "abc123".parse().unwrap());
                    Poll::Ready(Some(Ok(http_body::Frame::trailers(t))))
                }
                _ => Poll::Ready(None),
            }
        }
    }

    let body: RequestBodySend = DataThenTrailer { state: 0 }.boxed_unsync();
    let mut stream = BodyStreamSend::new(body);

    // Trailers are not available until the body is fully drained.
    assert!(stream.trailers().is_none());
    let chunk = stream.next().await.unwrap().unwrap();
    assert_eq!(&chunk[..], b"payload");
    assert!(stream.next().await.is_none());

    let trailers = stream.trailers().expect("trailers should be captured");
    assert_eq!(trailers.get("x-checksum").unwrap(), "abc123");
}

#[tokio::test]
async fn body_stream_no_trailers_stays_none() {
    let body: RequestBodySend = http_body_util::Full::new(Bytes::from("no trailers"))
        .map_err(|never| match never {})
        .boxed_unsync();
    let mut stream = BodyStreamSend::new(body);
    while stream.next().await.is_some() {}
    assert!(stream.trailers().is_none());
}

#[tokio::test]
async fn body_stream_skips_empty_data_frames() {
    use std::pin::Pin;
    use std::task::{Context, Poll};

    // A body that emits: empty, "hello", empty, then end. The empty data
    // frames must not surface as `Some(Ok(b""))` — a consumer expects
    // `Some` to mean non-empty data and `None` to mean end of stream.
    struct EmptyThenData {
        state: u8,
    }

    impl http_body::Body for EmptyThenData {
        type Data = Bytes;
        type Error = Error;
        fn poll_frame(
            mut self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<Option<Result<http_body::Frame<Self::Data>, Self::Error>>> {
            let frame = match self.state {
                0 => http_body::Frame::data(Bytes::new()),
                1 => http_body::Frame::data(Bytes::from("hello")),
                2 => http_body::Frame::data(Bytes::new()),
                _ => return Poll::Ready(None),
            };
            self.state += 1;
            Poll::Ready(Some(Ok(frame)))
        }
    }

    let hyper_body: RequestBodySend = EmptyThenData { state: 0 }.boxed_unsync();
    let mut stream = BodyStreamSend::new(hyper_body);

    // Only the non-empty chunk is yielded.
    let chunk = stream.next().await.unwrap().unwrap();
    assert_eq!(&chunk[..], b"hello");

    // The trailing empty frame is skipped; stream terminates with None.
    assert!(
        stream.next().await.is_none(),
        "empty data frames must not be yielded as empty chunks"
    );
}

#[tokio::test]
async fn body_stream_without_observer_still_works() {
    let hyper_body: RequestBodySend = http_body_util::Full::new(Bytes::from("no observer"))
        .map_err(|never| match never {})
        .boxed_unsync();
    let mut stream = BodyStreamSend::new(hyper_body);

    let chunk = stream.next().await.unwrap().unwrap();
    assert_eq!(&chunk[..], b"no observer");
    assert!(stream.next().await.is_none());
}

#[tokio::test]
async fn body_stream_empty_with_only_trailers() {
    use std::pin::Pin;
    use std::task::{Context, Poll};

    struct TrailersOnlyBody {
        sent: bool,
    }

    impl http_body::Body for TrailersOnlyBody {
        type Data = Bytes;
        type Error = Error;
        fn poll_frame(
            mut self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<Option<Result<http_body::Frame<Self::Data>, Self::Error>>> {
            if !self.sent {
                self.sent = true;
                let mut trailers = http::HeaderMap::new();
                trailers.insert("x-trailer-key", "x-trailer-value".parse().unwrap());
                Poll::Ready(Some(Ok(http_body::Frame::trailers(trailers))))
            } else {
                Poll::Ready(None)
            }
        }
    }

    let hyper_body: RequestBodySend = TrailersOnlyBody { sent: false }.boxed_unsync();
    let mut stream = BodyStreamSend::new(hyper_body);

    // No data frames — next() returns None without panicking
    assert!(stream.next().await.is_none());
}

#[cfg(feature = "compio")]
#[test]
fn body_stream_local_skips_trailers() {
    use std::pin::Pin;
    use std::task::{Context, Poll};

    struct TrailerThenData {
        state: u8,
    }

    impl http_body::Body for TrailerThenData {
        type Data = Bytes;
        type Error = Error;
        fn poll_frame(
            mut self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<Option<Result<http_body::Frame<Self::Data>, Self::Error>>> {
            match self.state {
                0 => {
                    self.state = 1;
                    let mut trailers = http::HeaderMap::new();
                    trailers.insert("x-local-test", "val".parse().unwrap());
                    Poll::Ready(Some(Ok(http_body::Frame::trailers(trailers))))
                }
                1 => {
                    self.state = 2;
                    Poll::Ready(Some(Ok(http_body::Frame::data(Bytes::from(
                        "after_trailer",
                    )))))
                }
                _ => Poll::Ready(None),
            }
        }
    }

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let local_body = ResponseBodyLocal::from_inner(Box::pin(TrailerThenData { state: 0 }));
        let mut stream = BodyStreamLocal::from_response_body(local_body);

        // The trailers frame should be skipped, only data returned
        let chunk = stream.next().await.unwrap().unwrap();
        assert_eq!(&chunk[..], b"after_trailer");

        // Then done
        assert!(stream.next().await.is_none());
    });
}

#[cfg(feature = "compio")]
#[test]
fn body_stream_local_skips_empty_data_frames() {
    use std::pin::Pin;
    use std::task::{Context, Poll};

    struct EmptyThenData {
        state: u8,
    }

    impl http_body::Body for EmptyThenData {
        type Data = Bytes;
        type Error = Error;
        fn poll_frame(
            mut self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<Option<Result<http_body::Frame<Self::Data>, Self::Error>>> {
            let frame = match self.state {
                0 => http_body::Frame::data(Bytes::new()),
                1 => http_body::Frame::data(Bytes::from("hello")),
                2 => http_body::Frame::data(Bytes::new()),
                _ => return Poll::Ready(None),
            };
            self.state += 1;
            Poll::Ready(Some(Ok(frame)))
        }
    }

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let local_body = ResponseBodyLocal::from_inner(Box::pin(EmptyThenData { state: 0 }));
        let mut stream = BodyStreamLocal::from_response_body(local_body);

        let chunk = stream.next().await.unwrap().unwrap();
        assert_eq!(&chunk[..], b"hello");
        assert!(
            stream.next().await.is_none(),
            "empty data frames must not be yielded as empty chunks"
        );
    });
}
