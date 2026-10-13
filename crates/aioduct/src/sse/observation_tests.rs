use crate::error::Error;
use crate::observer::{
    Interest, LocalRequestObservationHandle, NoopConnectionObservation, Observer, RequestFinish,
    RequestFinishReason, RequestInfo, RequestObservation, ResponseInfo, ResponseSource,
    ResponseSseEvent, SendRequestObservationHandle, SseStatisticsSnapshot,
};
use crate::sse::SseEvent;
use crate::traits::SseStreamExt;
use bytes::Bytes;
use http_body::{Body, Frame};
use http_body_util::BodyExt;
use std::{
    collections::VecDeque,
    future::Future,
    pin::Pin,
    sync::{
        Arc, Barrier, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    task::{Context, Poll, Waker},
};

#[derive(Debug, PartialEq, Eq)]
enum Fact {
    Started,
    Error {
        source: Option<usize>,
        statistics: Option<SseStatisticsSnapshot>,
    },
    Ended(Option<SseStatisticsSnapshot>),
    Abandoned(Option<SseStatisticsSnapshot>),
    HttpEnded,
    HttpFailed(Option<usize>),
    HttpAbandoned,
}

struct Recorder {
    facts: Arc<Mutex<Vec<Fact>>>,
    panic_on_end: bool,
}

fn source(error: &Error) -> Option<usize> {
    match error {
        Error::Other(error) => Some(std::ptr::from_ref(&**error) as *const () as usize),
        _ => None,
    }
}

impl Observer for Recorder {
    type Context = ();
    type Observation = Self;
    type ConnectionObservation = NoopConnectionObservation;
    fn begin_connection(&self) -> (Interest, Self::ConnectionObservation) {
        (Interest::EMPTY, NoopConnectionObservation)
    }

    fn begin(&self, _: RequestInfo<'_>, _: Option<()>) -> (Interest, Self) {
        (
            Interest::SSE_STATISTICS | Interest::REQUEST_FINISH,
            Self {
                facts: self.facts.clone(),
                panic_on_end: self.panic_on_end,
            },
        )
    }
}

impl RequestObservation for Recorder {
    fn on_request_finish(&self, event: &RequestFinish<'_>) {
        let fact = match event.reason {
            RequestFinishReason::Ended => Fact::HttpEnded,
            RequestFinishReason::Failed { error } => Fact::HttpFailed(source(error)),
            RequestFinishReason::ConsumerAbandoned => Fact::HttpAbandoned,
            _ => panic!("unexpected HTTP finish"),
        };
        self.facts.lock().unwrap().push(fact);
    }

    fn on_response_sse(&self, event: ResponseSseEvent<'_>) {
        let fact = match event {
            ResponseSseEvent::Started => Fact::Started,
            ResponseSseEvent::Error { error, statistics } => Fact::Error {
                source: source(error),
                statistics: statistics.copied(),
            },
            ResponseSseEvent::Ended { statistics } => Fact::Ended(statistics.copied()),
            ResponseSseEvent::Abandoned { statistics } => Fact::Abandoned(statistics.copied()),
        };
        self.facts.lock().unwrap().push(fact);
        if self.panic_on_end && matches!(event, ResponseSseEvent::Ended { .. }) {
            panic!("callback panic");
        }
    }
}

enum Step {
    Data(&'static [u8]),
    Error(Error),
    Pending,
}

struct Script {
    steps: VecDeque<Step>,
    polls: Arc<AtomicUsize>,
}

impl Body for Script {
    type Data = Bytes;
    type Error = Error;
    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Error>>> {
        self.polls.fetch_add(1, Ordering::Relaxed);
        match self.steps.pop_front() {
            Some(Step::Data(data)) => Poll::Ready(Some(Ok(Frame::data(Bytes::from_static(data))))),
            Some(Step::Error(error)) => Poll::Ready(Some(Err(error))),
            Some(Step::Pending) => {
                cx.waker().wake_by_ref();
                Poll::Pending
            }
            None => Poll::Ready(None),
        }
    }
}

fn response(steps: impl IntoIterator<Item = Step>) -> (crate::Response, Arc<AtomicUsize>) {
    let polls = Arc::new(AtomicUsize::new(0));
    let body = Script {
        steps: steps.into_iter().collect(),
        polls: polls.clone(),
    }
    .boxed_unsync();
    (
        crate::Response::from_boxed(
            http::Response::new(body),
            "http://localhost/".parse().unwrap(),
        ),
        polls,
    )
}

fn send(
    steps: impl IntoIterator<Item = Step>,
    recorder: &Recorder,
) -> (crate::sse::SseStreamSend, Arc<AtomicUsize>) {
    let (mut response, polls) = response(steps);
    let method = http::Method::GET;
    let uri = "http://localhost/".parse().unwrap();
    let mut handle = SendRequestObservationHandle::new(
        recorder,
        RequestInfo {
            method: &method,
            initial_uri: &uri,
        },
        None,
    )
    .unwrap();
    handle.observe_headers(ResponseInfo {
        status: http::StatusCode::OK,
        version: Some(http::Version::HTTP_11),
        source: ResponseSource::Transport,
    });
    handle.attach_to_send_response(&mut response, false);
    drop(handle);
    (response.into_sse_stream(), polls)
}

fn local(
    steps: impl IntoIterator<Item = Step>,
    recorder: &Recorder,
) -> (crate::sse::SseStreamLocal, Arc<AtomicUsize>) {
    let (response, polls) = response(steps);
    let mut response = response.into_local();
    let method = http::Method::GET;
    let uri = "http://localhost/".parse().unwrap();
    let mut handle = LocalRequestObservationHandle::new(
        recorder,
        RequestInfo {
            method: &method,
            initial_uri: &uri,
        },
        None,
    )
    .unwrap();
    handle.observe_headers(ResponseInfo {
        status: http::StatusCode::OK,
        version: Some(http::Version::HTTP_11),
        source: ResponseSource::Transport,
    });
    handle.attach_to_local_response(&mut response, false);
    drop(handle);
    (response.into_sse_stream(), polls)
}

fn run<F: Future>(future: F) -> F::Output {
    let mut future = std::pin::pin!(future);
    let mut cx = Context::from_waker(Waker::noop());
    for _ in 0..100 {
        if let Poll::Ready(output) = future.as_mut().poll(&mut cx) {
            return output;
        }
    }
    panic!("script did not complete");
}

fn cancellation<S: SseStreamExt>(mut stream: S, recorder: &Recorder, polls: &AtomicUsize) {
    drop(SseStreamExt::next(&mut stream));
    assert_eq!(polls.load(Ordering::Relaxed), 0);
    for _ in 0..2 {
        let mut next = std::pin::pin!(SseStreamExt::next(&mut stream));
        assert!(
            next.as_mut()
                .poll(&mut Context::from_waker(Waker::noop()))
                .is_pending()
        );
        assert_eq!(*recorder.facts.lock().unwrap(), vec![Fact::Started]);
    }
    let SseEvent::Message(message) = run(SseStreamExt::next(&mut stream)).unwrap().unwrap() else {
        panic!("expected message");
    };
    assert_eq!(message.data, "partial continued");
    drop(stream);
    let facts = recorder.facts.lock().unwrap();
    assert_eq!(facts.len(), 3);
    assert_eq!(facts[1], Fact::HttpAbandoned);
    let Fact::Abandoned(Some(stats)) = facts[2] else {
        panic!("expected abandoned statistics");
    };
    assert_eq!(stats.messages_delivered, 1);
    assert_eq!(stats.buffered_input_bytes, 0);
}

#[test]
fn cancelling_native_next_preserves_partial_input_without_events() {
    for is_local in [false, true] {
        let recorder = Recorder {
            facts: Arc::new(Mutex::new(Vec::new())),
            panic_on_end: false,
        };
        let steps = [
            Step::Data(b"data: partial"),
            Step::Pending,
            Step::Pending,
            Step::Data(b" continued\n\n"),
        ];
        if is_local {
            let (stream, polls) = local(steps, &recorder);
            cancellation(stream, &recorder, &polls);
        } else {
            let (stream, polls) = send(steps, &recorder);
            cancellation(stream, &recorder, &polls);
        }
    }
}

#[test]
fn never_read_sse_stream_reports_real_zero_statistics_without_polling() {
    let recorder = Recorder {
        facts: Arc::new(Mutex::new(Vec::new())),
        panic_on_end: false,
    };
    let (stream, polls) = send([Step::Data(b"data: unread\n\n")], &recorder);
    assert_eq!(*recorder.facts.lock().unwrap(), vec![Fact::Started]);
    drop(stream);
    assert_eq!(polls.load(Ordering::Relaxed), 0);
    assert_eq!(
        *recorder.facts.lock().unwrap(),
        vec![
            Fact::Started,
            Fact::HttpAbandoned,
            Fact::Abandoned(Some(SseStatisticsSnapshot::default()))
        ]
    );
}

#[test]
fn decoder_error_does_not_end_http_and_later_body_eof_can_succeed() {
    fn check<S: SseStreamExt>(stream: S, recorder: &Recorder) {
        let mut stream = stream.with_max_payload_size(12);
        assert!(matches!(
            run(SseStreamExt::next(&mut stream)).unwrap().unwrap_err(),
            Error::SseEventTooLarge { limit_bytes: 12 }
        ));
        assert_eq!(recorder.facts.lock().unwrap().len(), 2); // Started + SSE Error, no HTTP failure.
        assert!(matches!(
            run(SseStreamExt::next(&mut stream)).unwrap().unwrap(),
            SseEvent::Message(_)
        ));
        assert!(run(SseStreamExt::next(&mut stream)).is_none());
        drop(stream);
        let facts = recorder.facts.lock().unwrap();
        assert_eq!(facts.len(), 4);
        assert_eq!(facts[2], Fact::HttpEnded);
        let Fact::Ended(Some(stats)) = facts[3] else {
            panic!("expected SSE end");
        };
        assert_eq!(stats.messages_delivered, 1);
    }
    for is_local in [false, true] {
        let recorder = Recorder {
            facts: Arc::new(Mutex::new(Vec::new())),
            panic_on_end: false,
        };
        let steps = [Step::Data(b"data: oversized-input\n\ndata: ok\n\n")];
        if is_local {
            check(local(steps, &recorder).0, &recorder);
        } else {
            check(send(steps, &recorder).0, &recorder);
        }
    }
}

#[test]
fn observed_and_unobserved_streams_share_fragmented_parser_behavior() {
    use crate::sse::SseStream;
    fn collect<S: SseStreamExt>(mut stream: S) -> Vec<String> {
        let mut events = Vec::new();
        while let Some(event) = run(SseStreamExt::next(&mut stream)) {
            events.push(format!("{event:?}"));
        }
        events
    }
    let fixtures: &[&'static [u8]] = &[
        b"\xef\xbb\xbf:comment\r\nretry: 7\r\n\r\ndata:\r\nevent: custom\r\n\r\ndata: tail",
        b"retry: 3\nretry: invalid\ndata: first\ndata: second\n\n",
        b"data: invalid-\xff-utf8\n\nid: sticky\n\ndata: next\n\n",
        b"data: much-too-large-for-the-limit\n\ndata: ok\n\n",
    ];
    for fixture in fixtures {
        for stride in 1..=8 {
            let recorder = Recorder {
                facts: Arc::new(Mutex::new(Vec::new())),
                panic_on_end: false,
            };
            let steps = || fixture.chunks(stride).map(Step::Data);
            let observed = send(steps(), &recorder).0.with_max_payload_size(24);
            let (response, _) = response(steps());
            let (body, _) = response.into_body().into_parts();
            let unobserved = SseStream::new(body).with_max_payload_size(24);
            assert_eq!(collect(observed), collect(unobserved), "stride={stride}");
        }
    }
}

fn original_error<S: SseStreamExt>(mut stream: S, recorder: &Recorder) {
    let error = run(SseStreamExt::next(&mut stream)).unwrap().unwrap_err();
    let expected_source = source(&error).unwrap();
    assert_eq!(
        &recorder.facts.lock().unwrap()[..],
        &[
            Fact::Started,
            Fact::HttpFailed(Some(expected_source)),
            Fact::Error {
                source: Some(expected_source),
                statistics: Some(SseStatisticsSnapshot::default())
            },
        ]
    );
    assert!(matches!(
        run(SseStreamExt::next(&mut stream)).unwrap().unwrap(),
        SseEvent::Message(_)
    ));
    assert!(run(SseStreamExt::next(&mut stream)).is_none());
    drop(stream);
    let facts = recorder.facts.lock().unwrap();
    assert_eq!(facts.len(), 4);
    let Fact::Ended(Some(stats)) = facts[3] else {
        panic!("expected SSE end, not second HTTP finish");
    };
    assert_eq!(stats.messages_delivered, 1);
}

#[test]
fn original_noncloneable_body_error_is_borrowed_by_both_lifecycles() {
    #[derive(Debug)]
    struct Marker(u64);
    impl std::fmt::Display for Marker {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "source {}", self.0)
        }
    }

    impl std::error::Error for Marker {}
    for is_local in [false, true] {
        let recorder = Recorder {
            facts: Arc::new(Mutex::new(Vec::new())),
            panic_on_end: false,
        };
        let steps = [
            Step::Error(Error::Other(Box::new(Marker(7)))),
            Step::Data(b"data: recovered\n\n"),
        ];
        if is_local {
            let (stream, _) = local(steps, &recorder);
            original_error(stream, &recorder);
        } else {
            let (stream, _) = send(steps, &recorder);
            original_error(stream, &recorder);
        }
    }
}

#[test]
fn sse_terminal_callback_panic_does_not_create_abandonment() {
    for is_local in [false, true] {
        let recorder = Recorder {
            facts: Arc::new(Mutex::new(Vec::new())),
            panic_on_end: true,
        };
        fn check<S: SseStreamExt>(mut stream: S) {
            assert!(
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run(SseStreamExt::next(
                    &mut stream
                ))))
                .is_err()
            );
            drop(stream);
        }
        if is_local {
            check(local([], &recorder).0);
        } else {
            check(send([], &recorder).0);
        }
        assert_eq!(
            *recorder.facts.lock().unwrap(),
            vec![
                Fact::Started,
                Fact::HttpEnded,
                Fact::Ended(Some(SseStatisticsSnapshot::default()))
            ]
        );
    }
}

#[test]
fn production_sse_receiver_supports_64_concurrent_threads() {
    let facts = Arc::new(Mutex::new(Vec::new()));
    let barrier = Arc::new(Barrier::new(64));
    let threads: Vec<_> = (0..64)
        .map(|_| {
            let recorder = Recorder {
                facts: facts.clone(),
                panic_on_end: false,
            };
            let barrier = barrier.clone();
            let stream = send([Step::Data(b"data: one\n\n")], &recorder).0;
            std::thread::spawn(move || {
                barrier.wait();
                let mut stream = stream;
                assert!(run(stream.next()).unwrap().is_ok());
                assert!(run(stream.next()).is_none());
            })
        })
        .collect();
    for thread in threads {
        thread.join().unwrap();
    }
    let facts = facts.lock().unwrap();
    assert_eq!(facts.len(), 64 * 3);
    assert_eq!(
        facts
            .iter()
            .filter(|fact| matches!(fact, Fact::Started))
            .count(),
        64
    );
    assert_eq!(
        facts
            .iter()
            .filter(|fact| matches!(fact, Fact::HttpEnded))
            .count(),
        64
    );
    assert_eq!(
        facts
            .iter()
            .filter(
                |fact| matches!(fact, Fact::Ended(Some(stats)) if stats.messages_delivered == 1)
            )
            .count(),
        64
    );
}
