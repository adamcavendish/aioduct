#![cfg(all(
    not(target_arch = "wasm32"),
    any(feature = "tokio", feature = "smol", feature = "compio")
))]

use aioduct::{
    Error, Interest, Observer, RequestInfo, RequestObservation, ResponseSseEvent, SseEvent,
    SseStatisticsSnapshot, SseStreamExt,
};
#[cfg(any(feature = "tokio", feature = "smol", feature = "blocking"))]
use aioduct::{RequestFinish, RequestFinishReason};
use bytes::Bytes;
use http_body_util::Full;
use std::convert::Infallible;
#[cfg(any(feature = "tokio", feature = "smol", feature = "blocking"))]
use std::sync::{Arc, Mutex};

#[derive(Clone, Debug, PartialEq, Eq)]
enum Event {
    Started,
    Error {
        limit: Option<usize>,
        statistics: Option<SseStatisticsSnapshot>,
    },
    Ended(Option<SseStatisticsSnapshot>),
    Abandoned(Option<SseStatisticsSnapshot>),
}

fn record(event: ResponseSseEvent<'_>) -> Event {
    match event {
        ResponseSseEvent::Started => Event::Started,
        ResponseSseEvent::Error { error, statistics } => Event::Error {
            limit: match error {
                Error::SseEventTooLarge { limit_bytes } => Some(*limit_bytes),
                _ => None,
            },
            statistics: statistics.copied(),
        },
        ResponseSseEvent::Ended { statistics } => Event::Ended(statistics.copied()),
        ResponseSseEvent::Abandoned { statistics } => Event::Abandoned(statistics.copied()),
        _ => panic!("unexpected SSE fact"),
    }
}

#[derive(Default)]
#[cfg(any(feature = "tokio", feature = "smol", feature = "blocking"))]
struct Records {
    sse: Mutex<Vec<Event>>,
    http: Mutex<Vec<(bool, Option<u64>)>>,
}
#[cfg(any(feature = "tokio", feature = "smol", feature = "blocking"))]
struct Recorder(Arc<Records>, Interest);
#[cfg(any(feature = "tokio", feature = "smol", feature = "blocking"))]
impl Observer for Recorder {
    type Context = ();
    type Observation = Self;
    type ConnectionObservation = aioduct::NoopConnectionObservation;
    fn begin_connection(&self) -> (Interest, Self::ConnectionObservation) {
        (Interest::EMPTY, aioduct::NoopConnectionObservation)
    }

    fn begin(&self, _: RequestInfo<'_>, _: Option<()>) -> (Interest, Self) {
        (self.1, Self(self.0.clone(), self.1))
    }
}
#[cfg(any(feature = "tokio", feature = "smol", feature = "blocking"))]
impl RequestObservation for Recorder {
    fn on_response_sse(&self, event: ResponseSseEvent<'_>) {
        self.0.sse.lock().unwrap().push(record(event));
    }

    fn on_request_finish(&self, event: &RequestFinish<'_>) {
        self.0.http.lock().unwrap().push((
            matches!(event.reason, RequestFinishReason::Ended),
            event.original_data_bytes,
        ));
    }
}

fn server(payload: &'static [u8]) -> String {
    let addr = aioduct_test_server::h1::spawn_h1_server_with(move |_| async move {
        Ok::<_, Infallible>(
            http::Response::builder()
                .header("content-type", "text/event-stream")
                .header("cache-control", "max-age=60")
                .body(Full::new(Bytes::from_static(payload)))
                .unwrap(),
        )
    });
    format!("http://{addr}/")
}

const PAYLOAD: &[u8] = b":a\n:\n\nid: sticky\n\nretry: 3\nretry: 4\nretry: invalid\nretry: 18446744073709551616\n\nevent: custom\ndata:\n\nretry: 5\ndata: first\ndata: second\n\ndata: tail";
const TAIL: &[u8] = b"data: tail";

async fn consume<S: SseStreamExt>(mut stream: S) -> Result<usize, Error> {
    let mut delivered = 0;
    while let Some(event) = SseStreamExt::next(&mut stream).await {
        if matches!(event?, SseEvent::Message(_)) {
            delivered += 1;
        }
    }
    // Reliable end is delivered once, even after another next and drop.
    assert!(SseStreamExt::next(&mut stream).await.is_none());
    Ok(delivered)
}

#[cfg(any(feature = "tokio", feature = "smol"))]
fn consume_send<S>(stream: S) -> impl std::future::Future<Output = Result<usize, Error>> + Send
where
    S: SseStreamExt + Send,
    for<'a> S::Next<'a>: Send,
{
    consume(stream)
}

#[cfg(any(feature = "smol", feature = "blocking"))]
fn assert_protocol_statistics(records: &Records) {
    let events = records.sse.lock().unwrap();
    assert_eq!(events.len(), 2); // No callbacks for messages, retry fields, or comments.
    assert_eq!(events[0], Event::Started);
    let Event::Ended(Some(stats)) = events[1] else {
        panic!("expected final statistics");
    };
    assert_eq!(stats.messages_delivered, 2);
    assert_eq!(stats.comment_lines_parsed, 2);
    assert_eq!(stats.retry_fields_accepted, 3);
    assert_eq!(stats.buffered_input_bytes, TAIL.len());
    assert!(stats.peak_buffered_input_bytes >= TAIL.len());
    assert!(stats.peak_buffered_input_bytes <= PAYLOAD.len());
}

#[cfg(feature = "tokio")]
#[tokio::test]
async fn sse_statistics_are_independent_of_cache_and_request_finish() {
    let url = server(PAYLOAD);
    for cache in [false, true] {
        let records = Arc::new(Records::default());
        let mut builder = aioduct::TokioClient::builder().observer(Recorder(
            records.clone(),
            Interest::SSE_STATISTICS | Interest::RESPONSE_BODY_BYTES,
        ));
        if cache {
            builder = builder.cache(aioduct::HttpCache::new());
        }
        let client = builder.build().unwrap();
        for _ in 0..2 {
            let response = client.get(&url).unwrap().send().await.unwrap();
            let before = records.http.lock().unwrap().len();
            if cache {
                assert!(before > 0);
            }
            let stream: aioduct::SseStreamSend = response.into_sse_stream();
            let messages = tokio::spawn(consume_send(stream)).await.unwrap().unwrap();
            assert_eq!(messages, 2);
        }
        let events = records.sse.lock().unwrap();
        assert_eq!(events.len(), 4);
        for pair in events.as_chunks::<2>().0 {
            assert_eq!(pair[0], Event::Started);
            let Event::Ended(Some(stats)) = pair[1] else {
                panic!("expected statistics");
            };
            assert_eq!(stats.messages_delivered, 2);
            assert_eq!(stats.comment_lines_parsed, 2);
            assert_eq!(stats.retry_fields_accepted, 3);
            assert_eq!(stats.buffered_input_bytes, TAIL.len());
        }
        let http = records.http.lock().unwrap();
        assert_eq!(http.len(), 2);
        assert_eq!(http[0], (true, Some(PAYLOAD.len() as u64)));
        // A cache hit measures its selected body, not the earlier network request.
        assert_eq!(http[1], (true, Some(PAYLOAD.len() as u64)));
    }
}

#[cfg(all(feature = "tokio", feature = "gzip"))]
#[tokio::test]
async fn sse_protocol_statistics_do_not_replace_encoded_http_data_counts() {
    use std::io::Write;
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(PAYLOAD).unwrap();
    let encoded = Bytes::from(encoder.finish().unwrap());
    let encoded_len = encoded.len() as u64;
    let addr = aioduct_test_server::h1::spawn_h1_server_with(move |_| {
        let encoded = encoded.clone();
        async move {
            Ok::<_, Infallible>(
                http::Response::builder()
                    .header("content-type", "text/event-stream")
                    .header("content-encoding", "gzip")
                    .header("cache-control", "max-age=60")
                    .body(Full::new(encoded))
                    .unwrap(),
            )
        }
    });
    for cache in [false, true] {
        let records = Arc::new(Records::default());
        let mut builder = aioduct::TokioClient::builder().observer(Recorder(
            records.clone(),
            Interest::SSE_STATISTICS | Interest::RESPONSE_BODY_BYTES,
        ));
        if cache {
            builder = builder.cache(aioduct::HttpCache::new());
        }
        let client = builder.build().unwrap();
        let stream = client
            .get(&format!("http://{addr}/"))
            .unwrap()
            .send()
            .await
            .unwrap()
            .into_sse_stream();
        assert_eq!(consume_send(stream).await.unwrap(), 2);
        assert_eq!(
            *records.http.lock().unwrap(),
            vec![(true, Some(encoded_len))]
        );
        let events = records.sse.lock().unwrap();
        let Event::Ended(Some(stats)) = events[1] else {
            panic!("expected statistics");
        };
        assert_eq!(stats.messages_delivered, 2);
        assert_eq!(stats.comment_lines_parsed, 2);
        assert_eq!(stats.retry_fields_accepted, 3);
        assert_eq!(stats.buffered_input_bytes, TAIL.len());
    }
}

#[cfg(feature = "tokio")]
#[tokio::test]
async fn sse_drop_keeps_completed_http_and_reports_only_delivered_messages() {
    let url = server(b"data: first\n\ndata: second\n\n");
    let records = Arc::new(Records::default());
    let client = aioduct::TokioClient::builder()
        .observer(Recorder(
            records.clone(),
            Interest::SSE_STATISTICS | Interest::REQUEST_FINISH,
        ))
        .cache(aioduct::HttpCache::new())
        .build()
        .unwrap();
    let response = client.get(&url).unwrap().send().await.unwrap();
    assert_eq!(*records.http.lock().unwrap(), vec![(true, None)]);
    let mut stream = response.into_sse_stream();
    assert_eq!(*records.sse.lock().unwrap(), vec![Event::Started]);
    stream.next().await.unwrap().unwrap();
    assert_eq!(*records.sse.lock().unwrap(), vec![Event::Started]);
    drop(stream);
    let events = records.sse.lock().unwrap();
    let Event::Abandoned(Some(stats)) = events[1] else {
        panic!("expected abandoned");
    };
    assert_eq!(stats.messages_delivered, 1);
    assert_eq!(stats.buffered_input_bytes, b"data: second\n\n".len());
    assert_eq!(events.len(), 2);
    assert_eq!(*records.http.lock().unwrap(), vec![(true, None)]);
}

#[cfg(feature = "tokio")]
#[tokio::test]
async fn recoverable_sse_error_keeps_http_result_and_continues_statistics() {
    let url = server(b":discarded\ndata: much-too-large\n\n:parsed\ndata: ok\n\n");
    let records = Arc::new(Records::default());
    let client = aioduct::TokioClient::builder()
        .observer(Recorder(
            records.clone(),
            Interest::SSE_STATISTICS | Interest::REQUEST_FINISH,
        ))
        .cache(aioduct::HttpCache::new())
        .build()
        .unwrap();
    let mut stream = client
        .get(&url)
        .unwrap()
        .send()
        .await
        .unwrap()
        .into_sse_stream()
        .with_max_payload_size(20);
    assert!(matches!(
        stream.next().await.unwrap().unwrap_err(),
        Error::SseEventTooLarge { limit_bytes: 20 }
    ));
    assert_eq!(*records.http.lock().unwrap(), vec![(true, None)]);
    assert!(matches!(
        stream.next().await.unwrap().unwrap(),
        SseEvent::Message(_)
    ));
    assert!(stream.next().await.is_none());
    drop(stream);
    let events = records.sse.lock().unwrap();
    assert_eq!(events.len(), 3);
    let Event::Error {
        limit: Some(20),
        statistics: Some(first),
    } = events[1]
    else {
        panic!("expected original limit error");
    };
    assert_eq!(first.messages_delivered, 0);
    assert_eq!(first.comment_lines_parsed, 0); // No rescan of discarded input.
    let Event::Ended(Some(last)) = events[2] else {
        panic!("expected ended");
    };
    assert_eq!(last.messages_delivered, 1);
    assert_eq!(last.comment_lines_parsed, 1);
}

#[cfg(feature = "tokio")]
#[tokio::test]
async fn sse_lifecycle_only_has_no_statistics_and_never_read_stream_is_abandoned() {
    let url = server(b"data: hello\n\n");
    let records = Arc::new(Records::default());
    let client = aioduct::TokioClient::builder()
        .observer(Recorder(records.clone(), Interest::SSE_LIFECYCLE))
        .build()
        .unwrap();
    drop(
        client
            .get(&url)
            .unwrap()
            .send()
            .await
            .unwrap()
            .into_sse_stream(),
    );
    assert_eq!(
        *records.sse.lock().unwrap(),
        vec![Event::Started, Event::Abandoned(None)]
    );
    records.sse.lock().unwrap().clear();
    let stream = client
        .get(&url)
        .unwrap()
        .send()
        .await
        .unwrap()
        .into_sse_stream();
    assert_eq!(consume_send(stream).await.unwrap(), 1);
    assert_eq!(
        *records.sse.lock().unwrap(),
        vec![Event::Started, Event::Ended(None)]
    );
    assert!(records.http.lock().unwrap().is_empty());
}

#[cfg(feature = "tokio")]
#[tokio::test]
async fn ordinary_body_and_disabled_request_do_not_create_sse_lifecycles() {
    let url = server(PAYLOAD);
    let records = Arc::new(Records::default());
    let client = aioduct::TokioClient::builder()
        .observer(Recorder(records.clone(), Interest::SSE_STATISTICS))
        .build()
        .unwrap();
    client
        .get(&url)
        .unwrap()
        .send()
        .await
        .unwrap()
        .bytes()
        .await
        .unwrap();
    assert!(records.sse.lock().unwrap().is_empty());
    let stream = client
        .get(&url)
        .unwrap()
        .disable_observation()
        .send()
        .await
        .unwrap()
        .into_sse_stream();
    assert_eq!(consume_send(stream).await.unwrap(), 2);
    assert!(records.sse.lock().unwrap().is_empty());
}

#[cfg(feature = "smol")]
#[test]
fn generic_send_sse_reader_runs_on_smol() {
    let url = server(PAYLOAD);
    let records = Arc::new(Records::default());
    smol::block_on(async {
        let client = aioduct::SmolClient::builder()
            .observer(Recorder(records.clone(), Interest::SSE_STATISTICS))
            .build()
            .unwrap();
        let stream = client
            .get(&url)
            .unwrap()
            .send()
            .await
            .unwrap()
            .into_sse_stream();
        assert_eq!(smol::spawn(consume_send(stream)).await.unwrap(), 2);
    });
    assert_protocol_statistics(&records);
}

#[cfg(feature = "compio")]
#[test]
fn local_sse_accepts_rc_observation_and_generic_non_send_next() {
    use std::cell::RefCell;
    use std::rc::Rc;
    struct Local(Rc<RefCell<Vec<Event>>>);
    impl Observer for Local {
        type Context = Rc<()>;
        type Observation = Self;
        type ConnectionObservation = aioduct::NoopConnectionObservation;
        fn begin_connection(&self) -> (Interest, Self::ConnectionObservation) {
            (Interest::EMPTY, aioduct::NoopConnectionObservation)
        }

        fn begin(&self, _: RequestInfo<'_>, _: Option<Rc<()>>) -> (Interest, Self) {
            (Interest::SSE_STATISTICS, Self(self.0.clone()))
        }
    }

    impl RequestObservation for Local {
        fn on_response_sse(&self, event: ResponseSseEvent<'_>) {
            self.0.borrow_mut().push(record(event));
        }
    }
    let events = Rc::new(RefCell::new(Vec::new()));
    let url = server(PAYLOAD);
    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = aioduct::CompioClient::builder()
            .observer(Local(events.clone()))
            .build_local()
            .unwrap();
        let stream: aioduct::SseStreamLocal = client
            .get_local(&url)
            .unwrap()
            .observation_context(Rc::new(()))
            .send()
            .await
            .unwrap()
            .into_sse_stream();
        assert_eq!(consume(stream).await.unwrap(), 2);
    });
    assert_eq!(events.borrow().len(), 2);
    let Event::Ended(Some(stats)) = events.borrow()[1] else {
        panic!("expected statistics");
    };
    assert_eq!(stats.messages_delivered, 2);
    assert_eq!(stats.comment_lines_parsed, 2);
    assert_eq!(stats.retry_fields_accepted, 3);
    assert_eq!(stats.buffered_input_bytes, TAIL.len());
}

#[cfg(all(feature = "blocking", feature = "tokio"))]
#[test]
fn blocking_tokio_reuses_associated_sse_stream() {
    let records = Arc::new(Records::default());
    let engine = aioduct::TokioClient::builder()
        .observer(Recorder(records.clone(), Interest::SSE_STATISTICS))
        .build()
        .unwrap();
    let client: aioduct::BlockingTokioClient<Recorder> =
        aioduct::blocking::BlockingClient::new(engine).unwrap();
    let mut stream = client
        .get(&server(PAYLOAD))
        .unwrap()
        .send()
        .unwrap()
        .into_sse_stream();
    while let Some(event) = stream.next() {
        event.unwrap();
    }
    drop(client); // The SSE stream owns a reference to its persistent session.
    assert!(stream.next().is_none());
    drop(stream);
    assert_protocol_statistics(&records);
}

#[cfg(all(feature = "blocking", feature = "smol"))]
#[test]
fn blocking_smol_reuses_associated_sse_stream() {
    let records = Arc::new(Records::default());
    let engine = aioduct::SmolClient::builder()
        .observer(Recorder(records.clone(), Interest::SSE_STATISTICS))
        .build()
        .unwrap();
    let client: aioduct::BlockingSmolClient<Recorder> =
        aioduct::blocking::BlockingClient::new(engine).unwrap();
    let mut stream = client
        .get(&server(PAYLOAD))
        .unwrap()
        .send()
        .unwrap()
        .into_sse_stream();
    while let Some(event) = stream.next() {
        event.unwrap();
    }
    drop(stream);
    assert_protocol_statistics(&records);
}

#[cfg(all(feature = "blocking", feature = "compio"))]
#[test]
fn blocking_compio_reuses_associated_local_sse_stream() {
    let records = Arc::new(Records::default());
    let engine = aioduct::CompioClient::builder()
        .observer(Recorder(records.clone(), Interest::SSE_STATISTICS))
        .build_local()
        .unwrap();
    let client: aioduct::BlockingCompioClient<Recorder> =
        aioduct::blocking::BlockingClient::new(engine).unwrap();
    let mut stream = client
        .get(&server(PAYLOAD))
        .unwrap()
        .send()
        .unwrap()
        .into_sse_stream();
    while let Some(event) = stream.next() {
        event.unwrap();
    }
    drop(stream);
    assert_protocol_statistics(&records);
}
