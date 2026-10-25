#![cfg(all(not(target_arch = "wasm32"), feature = "tokio"))]

use std::convert::Infallible;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use aioduct::runtime::tokio_rt::{TcpConnector, TokioRuntime};
use aioduct::{
    AttemptEvent, ConnectionAcquisitionEvent, ConnectionAcquisitionPhase,
    ConnectionAcquisitionResource, ConnectionAssociation, HttpEngineSend, Interest, Observer,
    PoolLookupResult, RequestFinish, RequestFinishReason, RequestInfo, RequestObservation,
    RequestStart, ResponseHeaders, ResponseInfo, ResponseSource,
};
use bytes::Bytes;
use http_body_util::{BodyExt, Full};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Reason {
    Ended,
    Failed,
    ConsumerAbandoned,
    ExecutionAbandoned,
    HandedOff,
}

#[derive(Debug, Clone, Copy)]
struct Finish {
    reason: Reason,
    response: Option<ResponseInfo>,
    attempts: u64,
    bytes: Option<u64>,
}

#[derive(Default)]
struct Records {
    begins: AtomicUsize,
    contexts: Mutex<Vec<Option<u64>>>,
    starts: AtomicUsize,
    headers: Mutex<Vec<ResponseInfo>>,
    finishes: Mutex<Vec<Finish>>,
    body_events: Mutex<Vec<BodyFact>>,
    attempts: Mutex<Vec<AttemptFact>>,
    diagnostics: Mutex<Vec<DiagnosticFact>>,
    backoff_started: tokio::sync::Notify,
    cleanup_started: tokio::sync::Notify,
    rate_limit_started: tokio::sync::Notify,
    acquisitions: Mutex<Vec<ConnectionAcquisitionPhase>>,
    acquisition_attempts: Mutex<Vec<u64>>,
    pool_lookups: Mutex<Vec<PoolLookupResult>>,
    acquired: Mutex<
        Vec<(
            ConnectionAcquisitionResource,
            ConnectionAssociation,
            http::Version,
        )>,
    >,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AttemptFact {
    Started(u64),
    Redirect,
    Retry,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DiagnosticFact {
    RateLimit(u64, aioduct::RateLimitState),
    CleanupStarted(u64),
    Drained(u64, Option<u64>),
    Stopped(u64),
    CleanupFailed(u64),
    Backoff(u64, aioduct::BackoffState),
    Failed(u64),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BodyFact {
    Ended,
    Failed,
    Abandoned,
    HandedOff,
}

struct Recorder {
    records: Arc<Records>,
    interest: Interest,
}

struct Observation(Arc<Records>);

impl Observer for Recorder {
    type Context = u64;
    type Observation = Observation;
    type ConnectionObservation = aioduct::NoopConnectionObservation;
    fn begin_connection(&self) -> (Interest, Self::ConnectionObservation) {
        (Interest::EMPTY, aioduct::NoopConnectionObservation)
    }

    fn begin(&self, _: RequestInfo<'_>, context: Option<u64>) -> (Interest, Observation) {
        self.records.begins.fetch_add(1, Ordering::Relaxed);
        self.records.contexts.lock().unwrap().push(context);
        (self.interest, Observation(self.records.clone()))
    }
}

impl RequestObservation for Observation {
    fn on_request_start(&self, _: &RequestStart<'_>) {
        self.0.starts.fetch_add(1, Ordering::Relaxed);
    }

    fn on_response_headers(&self, event: &ResponseHeaders) {
        self.0.headers.lock().unwrap().push(event.response);
    }

    fn on_request_finish(&self, event: &RequestFinish<'_>) {
        let reason = match event.reason {
            RequestFinishReason::Ended => Reason::Ended,
            RequestFinishReason::Failed { .. } => Reason::Failed,
            RequestFinishReason::ConsumerAbandoned => Reason::ConsumerAbandoned,
            RequestFinishReason::ExecutionAbandoned => Reason::ExecutionAbandoned,
            RequestFinishReason::HandedOff => Reason::HandedOff,
            _ => panic!("unexpected finish reason"),
        };
        self.0.finishes.lock().unwrap().push(Finish {
            reason,
            response: event.response,
            attempts: event.attempt_count,
            bytes: event.original_data_bytes,
        });
    }

    fn on_response_body(&self, event: &aioduct::ResponseBodyEvent<'_>) {
        let fact = match event {
            aioduct::ResponseBodyEvent::Ended { .. } => BodyFact::Ended,
            aioduct::ResponseBodyEvent::Failed { .. } => BodyFact::Failed,
            aioduct::ResponseBodyEvent::Abandoned { .. } => BodyFact::Abandoned,
            aioduct::ResponseBodyEvent::HandedOff { .. } => BodyFact::HandedOff,
            _ => panic!("unexpected response body event"),
        };
        self.0.body_events.lock().unwrap().push(fact);
    }

    fn on_attempt(&self, event: &AttemptEvent<'_>) {
        let diagnostic = match event {
            AttemptEvent::RateLimit { attempt, state, .. } => {
                if *state == aioduct::RateLimitState::Started {
                    self.0.rate_limit_started.notify_one();
                }
                Some(DiagnosticFact::RateLimit(*attempt, *state))
            }
            AttemptEvent::CleanupStarted { attempt, .. } => {
                self.0.cleanup_started.notify_one();
                Some(DiagnosticFact::CleanupStarted(*attempt))
            }
            AttemptEvent::Cleanup {
                attempt,
                state,
                bytes,
                ..
            } => match state {
                aioduct::CleanupState::Drained => Some(DiagnosticFact::Drained(*attempt, *bytes)),
                aioduct::CleanupState::Stopped => Some(DiagnosticFact::Stopped(*attempt)),
                aioduct::CleanupState::Failed { .. } => {
                    Some(DiagnosticFact::CleanupFailed(*attempt))
                }
                _ => None,
            },
            AttemptEvent::Backoff { attempt, state, .. } => {
                if *state == aioduct::BackoffState::Started {
                    self.0.backoff_started.notify_one();
                }
                Some(DiagnosticFact::Backoff(*attempt, *state))
            }
            AttemptEvent::Failed { attempt, .. } => Some(DiagnosticFact::Failed(*attempt)),
            _ => None,
        };
        if let Some(diagnostic) = diagnostic {
            self.0.diagnostics.lock().unwrap().push(diagnostic);
        }
        let fact = match event {
            AttemptEvent::Started { attempt, .. } => AttemptFact::Started(*attempt),
            AttemptEvent::ResponseDisposition { disposition, .. } => match disposition {
                aioduct::ResponseDisposition::Redirect => AttemptFact::Redirect,
                aioduct::ResponseDisposition::Retry => AttemptFact::Retry,
                _ => return,
            },
            _ => return,
        };
        self.0.attempts.lock().unwrap().push(fact);
    }

    fn on_connection_acquisition(&self, event: &ConnectionAcquisitionEvent<'_>) {
        let attempt = match event {
            ConnectionAcquisitionEvent::PoolLookup { attempt, .. }
            | ConnectionAcquisitionEvent::ConnectionAcquired { attempt, .. }
            | ConnectionAcquisitionEvent::PhaseStarted { attempt, .. }
            | ConnectionAcquisitionEvent::PhaseCompleted { attempt, .. }
            | ConnectionAcquisitionEvent::Failed { attempt, .. }
            | ConnectionAcquisitionEvent::Cancelled { attempt, .. }
            | ConnectionAcquisitionEvent::PhaseSkipped { attempt, .. } => *attempt,
            _ => panic!("unexpected acquisition event"),
        };
        self.0.acquisition_attempts.lock().unwrap().push(attempt);
        if let ConnectionAcquisitionEvent::PoolLookup { result, .. } = event {
            self.0.pool_lookups.lock().unwrap().push(*result);
            return;
        }
        if let ConnectionAcquisitionEvent::ConnectionAcquired {
            resource,
            connection,
            version,
            ..
        } = event
        {
            self.0
                .acquired
                .lock()
                .unwrap()
                .push((*resource, *connection, *version));
            return;
        }
        let Some(phase) = (match event {
            ConnectionAcquisitionEvent::PhaseStarted { phase, .. }
            | ConnectionAcquisitionEvent::PhaseCompleted { phase, .. }
            | ConnectionAcquisitionEvent::Failed { phase, .. }
            | ConnectionAcquisitionEvent::Cancelled { phase, .. } => Some(*phase),
            ConnectionAcquisitionEvent::PhaseSkipped { phase, .. } => Some(*phase),
            _ => None,
        }) else {
            return;
        };
        self.0.acquisitions.lock().unwrap().push(phase);
    }
}

fn interests() -> Interest {
    Interest::REQUEST_START
        .union(Interest::RESPONSE_HEADERS)
        .union(Interest::REQUEST_FINISH)
        .union(Interest::ATTEMPT)
        .union(Interest::RESPONSE_BODY_BYTES)
}

fn client(
    records: &Arc<Records>,
    interest: Interest,
) -> HttpEngineSend<TokioRuntime, TcpConnector, Recorder> {
    HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .observer(Recorder {
            records: records.clone(),
            interest,
        })
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap()
}

async fn server() -> String {
    let (addr, _) = aioduct_test_server::h1::h1_server_with(|_| async {
        Ok::<_, Infallible>(http::Response::new(Full::new(Bytes::from_static(b"hello"))))
    })
    .await;
    format!("http://{addr}/")
}

#[derive(Clone, Copy, Debug)]
enum SelectedBodyFailure {
    Truncated,
    ReadTimeout,
    ProcessingTimeout,
    #[cfg(feature = "gzip")]
    Decode,
}

impl SelectedBodyFailure {
    fn scenarios() -> Vec<(Self, bool)> {
        let failures = [
            Self::Truncated,
            Self::ReadTimeout,
            Self::ProcessingTimeout,
            #[cfg(feature = "gzip")]
            Self::Decode,
        ];
        failures
            .into_iter()
            .flat_map(|failure| {
                // An overall attempt timeout no longer covers body reads after
                // send returns; this case specifically tests cache processing.
                [true, false].into_iter().filter_map(move |cache| {
                    (cache || !matches!(failure, Self::ProcessingTimeout))
                        .then_some((failure, cache))
                })
            })
            .collect()
    }

    fn assert_error(self, error: &aioduct::Error) {
        match self {
            Self::Truncated => assert!(matches!(error, aioduct::Error::Hyper(_))),
            Self::ReadTimeout => assert!(matches!(error, aioduct::Error::ReadTimeout)),
            Self::ProcessingTimeout => assert!(matches!(error, aioduct::Error::Timeout)),
            #[cfg(feature = "gzip")]
            Self::Decode => match error {
                aioduct::Error::Other(source) => {
                    assert!(source.downcast_ref::<std::io::Error>().is_some());
                }
                _ => panic!("expected decoder error, got {error:?}"),
            },
        }
    }

    fn expected_bytes(self) -> u64 {
        match self {
            Self::Truncated => 2,
            Self::ReadTimeout | Self::ProcessingTimeout => 0,
            #[cfg(feature = "gzip")]
            Self::Decode => 8,
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum ObservationMode {
    Enabled,
    Disabled,
    Noop,
}

// A second request would succeed, so these tests distinguish a real retry from
// merely returning the same error after exhausting retries. Accepted sockets
// are not counted as requests: only fully received request headers are counted.
async fn selected_body_failure_server(
    failure: SelectedBodyFailure,
) -> (String, Arc<AtomicUsize>, tokio::task::JoinHandle<()>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let requests = Arc::new(AtomicUsize::new(0));
    let server_requests = requests.clone();
    let task = tokio::spawn(async move {
        let mut connections = tokio::task::JoinSet::new();
        loop {
            tokio::select! {
                accepted = listener.accept() => {
                    let (mut socket, _) = accepted.unwrap();
                    let requests = server_requests.clone();
                    connections.spawn(async move {
                        let mut head = Vec::new();
                        while !head.ends_with(b"\r\n\r\n") {
                            let mut byte = [0];
                            socket.read_exact(&mut byte).await.unwrap();
                            head.push(byte[0]);
                        }
                        if requests.fetch_add(1, Ordering::Relaxed) > 0 {
                            socket.write_all(b"HTTP/1.1 200 OK\r\nCache-Control: max-age=60\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok").await.unwrap();
                            return;
                        }
                        let header = match failure {
                            #[cfg(feature = "gzip")]
                            SelectedBodyFailure::Decode => b"HTTP/1.1 200 OK\r\nCache-Control: max-age=60\r\nContent-Encoding: gzip\r\nContent-Length: 8\r\nConnection: close\r\n\r\n".as_slice(),
                            _ => b"HTTP/1.1 200 OK\r\nCache-Control: max-age=60\r\nContent-Length: 5\r\nConnection: close\r\n\r\n".as_slice(),
                        };
                        socket.write_all(header).await.unwrap();
                        match failure {
                            SelectedBodyFailure::Truncated => {
                                socket.write_all(b"ab").await.unwrap();
                                // Deliver the partial DATA before the transport reports EOF.
                                tokio::time::sleep(Duration::from_millis(20)).await;
                            }
                            SelectedBodyFailure::ReadTimeout | SelectedBodyFailure::ProcessingTimeout => std::future::pending::<()>().await,
                            #[cfg(feature = "gzip")]
                            SelectedBodyFailure::Decode => socket.write_all(b"not-gzip").await.unwrap(),
                        }
                    });
                }
                result = connections.join_next(), if !connections.is_empty() => {
                    result.unwrap().unwrap();
                }
            }
        }
    });
    (format!("http://{addr}/"), requests, task)
}

fn retry_body_errors(
    errors: &Arc<AtomicUsize>,
    statuses: &Arc<AtomicUsize>,
) -> aioduct::RetryConfig {
    let errors = errors.clone();
    let statuses = statuses.clone();
    aioduct::RetryConfig::default()
        .max_retries(1)
        .initial_backoff(Duration::ZERO)
        .classify(move |context| match context.outcome() {
            aioduct::RetryOutcome::Error(_) => {
                errors.fetch_add(1, Ordering::Relaxed);
                aioduct::RetryDecision::Retry
            }
            aioduct::RetryOutcome::Status(_) => {
                statuses.fetch_add(1, Ordering::Relaxed);
                aioduct::RetryDecision::UseDefault
            }
            _ => panic!("unexpected retry outcome"),
        })
}

fn assert_selected_body_failure(
    records: &Records,
    mode: ObservationMode,
    failure: SelectedBodyFailure,
) {
    if matches!(mode, ObservationMode::Enabled) {
        assert_eq!(records.begins.load(Ordering::Relaxed), 1);
        let headers = records.headers.lock().unwrap();
        assert_eq!(headers.len(), 1);
        assert_eq!(headers[0].status, http::StatusCode::OK);
        assert_eq!(headers[0].source, ResponseSource::Transport);
        let finished = records.finishes.lock().unwrap();
        assert_eq!(finished.len(), 1);
        assert_eq!(finished[0].reason, Reason::Failed);
        assert_eq!(finished[0].response, Some(headers[0]));
        assert_eq!(finished[0].attempts, 1);
        assert_eq!(finished[0].bytes, Some(failure.expected_bytes()));
    } else {
        assert_eq!(records.begins.load(Ordering::Relaxed), 0);
        assert!(records.headers.lock().unwrap().is_empty());
        assert!(records.finishes.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn selected_body_failures_bypass_retry_with_and_without_cache_or_observation() {
    for (failure, cache) in SelectedBodyFailure::scenarios() {
        for mode in [
            ObservationMode::Enabled,
            ObservationMode::Disabled,
            ObservationMode::Noop,
        ] {
            let (url, requests, task) = selected_body_failure_server(failure).await;
            let records = Arc::new(Records::default());
            let errors = Arc::new(AtomicUsize::new(0));
            let statuses = Arc::new(AtomicUsize::new(0));
            let mut builder = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
                .retry(retry_body_errors(&errors, &statuses))
                .timeout(Duration::from_secs(5));
            if cache {
                builder = builder.cache(aioduct::HttpCache::new());
            }
            if matches!(failure, SelectedBodyFailure::ReadTimeout) {
                builder = builder.read_timeout(Duration::from_millis(100));
            }
            if matches!(failure, SelectedBodyFailure::ProcessingTimeout) {
                builder = builder.timeout(Duration::from_millis(200));
            }
            let error = if matches!(mode, ObservationMode::Noop) {
                let result = builder.build().unwrap().get(&url).unwrap().send().await;
                if cache {
                    result.unwrap_err().into_error()
                } else {
                    result.unwrap().bytes().await.unwrap_err()
                }
            } else {
                let client = builder
                    .observer(Recorder {
                        records: records.clone(),
                        interest: interests(),
                    })
                    .build()
                    .unwrap();
                let mut request = client.get(&url).unwrap();
                if matches!(mode, ObservationMode::Disabled) {
                    request = request.disable_observation();
                }
                let result = request.send().await;
                if cache {
                    result.unwrap_err().into_error()
                } else {
                    result.unwrap().bytes().await.unwrap_err()
                }
            };
            failure.assert_error(&error);
            assert_eq!(
                requests.load(Ordering::Relaxed),
                1,
                "{failure:?} {mode:?} cache={cache}"
            );
            assert_eq!(statuses.load(Ordering::Relaxed), 1);
            assert_eq!(errors.load(Ordering::Relaxed), 0);
            assert_selected_body_failure(&records, mode, failure);
            task.abort();
            let _ = task.await;
        }
    }
}

#[cfg(feature = "compio")]
#[tokio::test]
async fn local_selected_body_failures_bypass_retry_with_and_without_cache_or_observation() {
    for (failure, cache) in SelectedBodyFailure::scenarios() {
        for mode in [
            ObservationMode::Enabled,
            ObservationMode::Disabled,
            ObservationMode::Noop,
        ] {
            let (url, requests, task) = selected_body_failure_server(failure).await;
            let records = Arc::new(Records::default());
            let errors = Arc::new(AtomicUsize::new(0));
            let statuses = Arc::new(AtomicUsize::new(0));
            let local_records = records.clone();
            let retry = retry_body_errors(&errors, &statuses);
            let error = tokio::task::spawn_blocking(move || {
                compio_runtime::Runtime::new().unwrap().block_on(async {
                    let mut builder = aioduct::HttpEngineLocal::<
                        aioduct::runtime::compio_rt::CompioRuntime,
                        aioduct::runtime::compio_rt::TcpConnector,
                    >::builder()
                    .retry(retry)
                    .timeout(Duration::from_secs(5));
                    if cache {
                        builder = builder.cache(aioduct::HttpCache::new());
                    }
                    if matches!(failure, SelectedBodyFailure::ReadTimeout) {
                        builder = builder.read_timeout(Duration::from_millis(100));
                    }
                    if matches!(failure, SelectedBodyFailure::ProcessingTimeout) {
                        builder = builder.timeout(Duration::from_millis(200));
                    }
                    if matches!(mode, ObservationMode::Noop) {
                        let result = builder
                            .build_local()
                            .unwrap()
                            .get_local(&url)
                            .unwrap()
                            .send()
                            .await;
                        if cache {
                            result.unwrap_err()
                        } else {
                            result.unwrap().bytes().await.unwrap_err()
                        }
                    } else {
                        let client = builder
                            .observer(Recorder {
                                records: local_records,
                                interest: interests(),
                            })
                            .build_local()
                            .unwrap();
                        let mut request = client.get_local(&url).unwrap();
                        if matches!(mode, ObservationMode::Disabled) {
                            request = request.disable_observation();
                        }
                        let result = request.send().await;
                        if cache {
                            result.unwrap_err()
                        } else {
                            result.unwrap().bytes().await.unwrap_err()
                        }
                    }
                })
            })
            .await
            .unwrap();
            failure.assert_error(&error);
            assert_eq!(
                requests.load(Ordering::Relaxed),
                1,
                "{failure:?} {mode:?} cache={cache}"
            );
            assert_eq!(statuses.load(Ordering::Relaxed), 1);
            assert_eq!(errors.load(Ordering::Relaxed), 0);
            assert_selected_body_failure(&records, mode, failure);
            task.abort();
            let _ = task.await;
        }
    }
}

#[tokio::test]
async fn begin_is_lazy_disabled_skips_begin_and_context_is_optional() {
    let url = server().await;
    let records = Arc::new(Records::default());
    let client = client(&records, interests());
    let unused = client.get(&url).unwrap().send();
    assert_eq!(records.begins.load(Ordering::Relaxed), 0);
    drop(unused);
    client
        .get(&url)
        .unwrap()
        .observation_context(42)
        .disable_observation()
        .send()
        .await
        .unwrap()
        .bytes()
        .await
        .unwrap();
    assert_eq!(records.begins.load(Ordering::Relaxed), 0);
    client
        .get(&url)
        .unwrap()
        .send()
        .await
        .unwrap()
        .bytes()
        .await
        .unwrap();
    client
        .get(&url)
        .unwrap()
        .observation_context(7)
        .send()
        .await
        .unwrap()
        .bytes()
        .await
        .unwrap();
    assert_eq!(*records.contexts.lock().unwrap(), vec![None, Some(7)]);
    assert_eq!(records.starts.load(Ordering::Relaxed), 2);
    assert_eq!(records.finishes.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn headers_do_not_finish_request_and_raw_response_keeps_owner() {
    let records = Arc::new(Records::default());
    let client = client(&records, interests());
    // The output remains the ordinary response type even for a custom Observer.
    let response: aioduct::Response = client.get(&server().await).unwrap().send().await.unwrap();
    assert_eq!(records.headers.lock().unwrap().len(), 1);
    assert!(records.finishes.lock().unwrap().is_empty());
    let bytes = response
        .into_http_response()
        .into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes();
    assert_eq!(bytes, "hello");
    let finished = records.finishes.lock().unwrap();
    assert_eq!(finished.len(), 1);
    assert_eq!(finished[0].reason, Reason::Ended);
    assert_eq!(finished[0].attempts, 1);
    assert_eq!(finished[0].bytes, Some(5));
    assert_eq!(
        finished[0].response.unwrap().source,
        ResponseSource::Transport
    );
}

#[tokio::test]
async fn connection_acquisition_reports_pool_lookup_without_per_read_events() {
    let records = Arc::new(Records::default());
    let client = client(
        &records,
        Interest::CONNECTION_ACQUISITION | Interest::REQUEST_FINISH,
    );
    let url = server().await;

    client
        .get(&url)
        .unwrap()
        .send()
        .await
        .unwrap()
        .bytes()
        .await
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

    assert_eq!(
        *records.acquisitions.lock().unwrap(),
        vec![
            ConnectionAcquisitionPhase::PoolLookup,
            ConnectionAcquisitionPhase::PoolLookup,
            ConnectionAcquisitionPhase::Dns,
            ConnectionAcquisitionPhase::Dns,
            ConnectionAcquisitionPhase::TcpConnect,
            ConnectionAcquisitionPhase::TcpConnect,
            ConnectionAcquisitionPhase::PoolLookup,
            ConnectionAcquisitionPhase::PoolLookup,
        ]
    );

    let acquired = records.acquired.lock().unwrap();
    assert!(records.attempts.lock().unwrap().is_empty());
    assert!(
        records
            .acquisition_attempts
            .lock()
            .unwrap()
            .iter()
            .all(|attempt| *attempt == 1)
    );
    assert_eq!(acquired.len(), 2);
    assert_eq!(acquired[0].0, ConnectionAcquisitionResource::Connection);
    assert_eq!(acquired[0].2, http::Version::HTTP_11);
    assert_eq!(acquired[1].1, acquired[0].1);
    assert_eq!(
        *records.pool_lookups.lock().unwrap(),
        vec![PoolLookupResult::Miss, PoolLookupResult::Hit]
    );
}

#[tokio::test]
async fn unconsumed_response_is_abandoned_once() {
    let records = Arc::new(Records::default());
    let client = client(&records, interests());
    drop(client.get(&server().await).unwrap().send().await.unwrap());
    let finished = records.finishes.lock().unwrap();
    assert_eq!(finished.len(), 1);
    assert_eq!(finished[0].reason, Reason::ConsumerAbandoned);
    assert_eq!(finished[0].bytes, Some(0));
}

#[tokio::test]
async fn only_final_headers_are_reported_across_redirect_and_configured_retry() {
    let count = Arc::new(AtomicUsize::new(0));
    let count_server = count.clone();
    let (addr, _) = aioduct_test_server::h1::h1_server_with(move |request| {
        let count = count_server.clone();
        async move {
            let response = if request.uri().path() == "/redirect" {
                http::Response::builder()
                    .status(302)
                    .header("location", "/final")
                    .body(Full::new(Bytes::from_static(b"discarded")))
                    .unwrap()
            } else if count.fetch_add(1, Ordering::Relaxed) == 0 {
                http::Response::builder()
                    .status(503)
                    .body(Full::new(Bytes::from_static(b"retry")))
                    .unwrap()
            } else {
                http::Response::new(Full::new(Bytes::from_static(b"selected")))
            };
            Ok::<_, Infallible>(response)
        }
    })
    .await;
    let records = Arc::new(Records::default());
    let client = client(&records, interests());
    let response = client
        .get(&format!("http://{addr}/redirect"))
        .unwrap()
        .retry(
            aioduct::RetryConfig::default()
                .max_retries(1)
                .initial_backoff(Duration::ZERO),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(response.bytes().await.unwrap(), "selected");
    assert_eq!(records.begins.load(Ordering::Relaxed), 1);
    assert_eq!(records.headers.lock().unwrap().len(), 1);
    let finished = records.finishes.lock().unwrap();
    assert_eq!(finished.len(), 1);
    assert_eq!(finished[0].reason, Reason::Ended);
    assert_eq!(finished[0].attempts, 3);
    assert_eq!(finished[0].bytes, Some(8));
    assert_eq!(
        *records.attempts.lock().unwrap(),
        vec![
            AttemptFact::Started(1),
            AttemptFact::Redirect,
            AttemptFact::Started(2),
            AttemptFact::Retry,
            AttemptFact::Started(3),
        ]
    );
    assert_eq!(
        *records.diagnostics.lock().unwrap(),
        vec![
            DiagnosticFact::CleanupStarted(1),
            DiagnosticFact::Drained(1, Some(9)),
            DiagnosticFact::CleanupStarted(2),
            DiagnosticFact::Stopped(2),
            DiagnosticFact::Backoff(2, aioduct::BackoffState::Started),
            DiagnosticFact::Backoff(2, aioduct::BackoffState::Completed),
        ]
    );
}

#[tokio::test]
async fn cancelled_backoff_does_not_start_another_attempt() {
    let (addr, _) = aioduct_test_server::h1::h1_server_with(|_| async {
        Ok::<_, Infallible>(
            http::Response::builder()
                .status(503)
                .body(Full::new(Bytes::from_static(b"retry")))
                .unwrap(),
        )
    })
    .await;
    let records = Arc::new(Records::default());
    let client = client(&records, interests());
    let request = client.get(&format!("http://{addr}/")).unwrap().retry(
        aioduct::RetryConfig::default()
            .max_retries(1)
            .initial_backoff(Duration::from_secs(60)),
    );
    let mut send = Box::pin(request.send());
    tokio::select! {
        result = &mut send => panic!("request ended before cancellation: {result:?}"),
        () = records.backoff_started.notified() => {},
        () = tokio::time::sleep(Duration::from_secs(5)) => panic!("backoff did not start"),
    }
    drop(send);
    assert_eq!(
        *records.diagnostics.lock().unwrap(),
        vec![
            DiagnosticFact::CleanupStarted(1),
            DiagnosticFact::Stopped(1),
            DiagnosticFact::Backoff(1, aioduct::BackoffState::Started),
            DiagnosticFact::Backoff(1, aioduct::BackoffState::Cancelled),
        ]
    );
    assert_eq!(
        *records.attempts.lock().unwrap(),
        vec![AttemptFact::Started(1), AttemptFact::Retry]
    );
    let finishes = records.finishes.lock().unwrap();
    assert_eq!(finishes.len(), 1);
    assert_eq!(finishes[0].attempts, 1);
    assert_eq!(finishes[0].reason, Reason::ExecutionAbandoned);
}

#[tokio::test]
async fn configured_retry_reports_each_execution_failure_once() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    let records = Arc::new(Records::default());
    let client = client(&records, interests() | Interest::CONNECTION_ACQUISITION);
    client
        .get(&format!("http://{address}/"))
        .unwrap()
        .retry(
            aioduct::RetryConfig::default()
                .max_retries(1)
                .initial_backoff(Duration::ZERO)
                .classify(|_| aioduct::RetryDecision::Retry),
        )
        .send()
        .await
        .unwrap_err();
    let diagnostics = records.diagnostics.lock().unwrap();
    let acquisition_attempts = records.acquisition_attempts.lock().unwrap();
    assert!(acquisition_attempts.contains(&1));
    assert!(acquisition_attempts.contains(&2));
    assert!(
        acquisition_attempts
            .iter()
            .all(|attempt| [1, 2].contains(attempt))
    );
    assert_eq!(
        *diagnostics,
        vec![
            DiagnosticFact::Failed(1),
            DiagnosticFact::Backoff(1, aioduct::BackoffState::Started),
            DiagnosticFact::Backoff(1, aioduct::BackoffState::Completed),
            DiagnosticFact::Failed(2),
        ]
    );
}

#[tokio::test]
async fn rate_limit_admission_is_one_operation_separate_from_pool_acquisition() {
    for cancelled in [false, true] {
        let records = Arc::new(Records::default());
        let limiter = aioduct::RateLimiter::new(1, Duration::from_secs(60));
        if cancelled {
            assert!(limiter.try_acquire());
        }
        let client = aioduct::TokioClient::builder()
            .rate_limiter(limiter)
            .observer(Recorder {
                records: records.clone(),
                interest: interests() | Interest::CONNECTION_ACQUISITION,
            })
            .build()
            .unwrap();
        let url = server().await;
        let mut send = Box::pin(client.get(&url).unwrap().send());
        if cancelled {
            tokio::select! {
                result = &mut send => panic!("unexpected result: {result:?}"),
                () = records.rate_limit_started.notified() => {},
            }
            drop(send);
            assert!(records.acquisitions.lock().unwrap().is_empty());
        } else {
            send.await.unwrap().bytes().await.unwrap();
            assert!(!records.acquisitions.lock().unwrap().is_empty());
        }
        assert_eq!(
            *records.diagnostics.lock().unwrap(),
            vec![
                DiagnosticFact::RateLimit(1, aioduct::RateLimitState::Started),
                DiagnosticFact::RateLimit(
                    1,
                    if cancelled {
                        aioduct::RateLimitState::Cancelled
                    } else {
                        aioduct::RateLimitState::Admitted
                    }
                ),
            ]
        );
    }
}

#[cfg(feature = "compio")]
#[tokio::test]
async fn local_rate_limit_cancellation_does_not_invent_pool_work() {
    let records = Arc::new(Records::default());
    let local_records = records.clone();
    tokio::task::spawn_blocking(move || {
        compio_runtime::Runtime::new().unwrap().block_on(async {
            let limiter = aioduct::RateLimiter::new(1, Duration::from_secs(60));
            assert!(limiter.try_acquire());
            let client = aioduct::CompioClient::builder()
                .rate_limiter(limiter)
                .observer(Recorder {
                    records: local_records.clone(),
                    interest: interests() | Interest::CONNECTION_ACQUISITION,
                })
                .build_local()
                .unwrap();
            let mut send = Box::pin(client.get_local("http://127.0.0.1:9/").unwrap().send());
            let mut waiting = Box::pin(local_records.rate_limit_started.notified());
            assert!(matches!(
                futures_util::future::select(&mut send, &mut waiting).await,
                futures_util::future::Either::Right(_)
            ));
            drop(send);
        });
    })
    .await
    .unwrap();
    assert!(records.acquisitions.lock().unwrap().is_empty());
    assert_eq!(
        *records.diagnostics.lock().unwrap(),
        vec![
            DiagnosticFact::RateLimit(1, aioduct::RateLimitState::Started),
            DiagnosticFact::RateLimit(1, aioduct::RateLimitState::Cancelled),
        ]
    );
}

#[cfg(feature = "compio")]
#[tokio::test]
async fn local_configured_retry_has_cleanup_and_actual_backoff() {
    let count = Arc::new(AtomicUsize::new(0));
    let server_count = count.clone();
    let (addr, _) = aioduct_test_server::h1::h1_server_with(move |_| {
        let count = server_count.clone();
        async move {
            Ok::<_, Infallible>(
                http::Response::builder()
                    .status(if count.fetch_add(1, Ordering::Relaxed) == 0 {
                        503
                    } else {
                        200
                    })
                    .body(Full::new(Bytes::from_static(b"ok")))
                    .unwrap(),
            )
        }
    })
    .await;
    let records = Arc::new(Records::default());
    let local_records = records.clone();
    tokio::task::spawn_blocking(move || {
        compio_runtime::Runtime::new().unwrap().block_on(async {
            let client = aioduct::HttpEngineLocal::<
                aioduct::runtime::compio_rt::CompioRuntime,
                aioduct::runtime::compio_rt::TcpConnector,
            >::builder()
            .retry(
                aioduct::RetryConfig::default()
                    .max_retries(1)
                    .initial_backoff(Duration::ZERO),
            )
            .observer(Recorder {
                records: local_records,
                interest: interests(),
            })
            .build_local()
            .unwrap();
            assert_eq!(
                client
                    .get_local(&format!("http://{addr}/"))
                    .unwrap()
                    .send()
                    .await
                    .unwrap()
                    .bytes()
                    .await
                    .unwrap(),
                "ok"
            );
        });
    })
    .await
    .unwrap();
    assert_eq!(
        *records.diagnostics.lock().unwrap(),
        vec![
            DiagnosticFact::CleanupStarted(1),
            DiagnosticFact::Stopped(1),
            DiagnosticFact::Backoff(1, aioduct::BackoffState::Started),
            DiagnosticFact::Backoff(1, aioduct::BackoffState::Completed),
        ]
    );
    assert_eq!(records.finishes.lock().unwrap()[0].attempts, 2);
}

#[tokio::test]
async fn redirect_cleanup_error_is_not_consumer_abandonment() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        for response in [
            &b"HTTP/1.1 302 Found\r\nLocation: /final\r\nContent-Length: 8\r\nConnection: close\r\n\r\nx"[..],
            &b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok"[..],
        ] {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut headers = Vec::new();
            while !headers.ends_with(b"\r\n\r\n") {
                let mut byte = [0];
                socket.read_exact(&mut byte).await.unwrap();
                headers.push(byte[0]);
            }
            socket.write_all(response).await.unwrap();
            socket.shutdown().await.unwrap();
        }
    });
    let records = Arc::new(Records::default());
    let client = client(&records, interests());
    assert_eq!(
        client
            .get(&format!("http://{addr}/"))
            .unwrap()
            .send()
            .await
            .unwrap()
            .bytes()
            .await
            .unwrap(),
        "ok"
    );
    task.await.unwrap();
    assert_eq!(
        *records.diagnostics.lock().unwrap(),
        vec![
            DiagnosticFact::CleanupStarted(1),
            DiagnosticFact::CleanupFailed(1)
        ]
    );
    assert_eq!(records.finishes.lock().unwrap()[0].reason, Reason::Ended);
}

#[tokio::test]
async fn cancelled_redirect_drain_stops_cleanup_without_consumer_abandonment() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut headers = Vec::new();
        while !headers.ends_with(b"\r\n\r\n") {
            let mut byte = [0];
            socket.read_exact(&mut byte).await.unwrap();
            headers.push(byte[0]);
        }
        socket
            .write_all(b"HTTP/1.1 302 Found\r\nLocation: /final\r\nContent-Length: 20\r\n\r\n")
            .await
            .unwrap();
        std::future::pending::<()>().await;
    });
    let records = Arc::new(Records::default());
    let client = client(&records, interests());
    let mut send = Box::pin(client.get(&format!("http://{address}/")).unwrap().send());
    tokio::select! {
        result = &mut send => panic!("unexpected request end: {result:?}"),
        () = records.cleanup_started.notified() => {},
        () = tokio::time::sleep(Duration::from_secs(5)) => panic!("cleanup did not start"),
    }
    drop(send);
    assert_eq!(
        *records.diagnostics.lock().unwrap(),
        vec![
            DiagnosticFact::CleanupStarted(1),
            DiagnosticFact::Stopped(1)
        ]
    );
    {
        let finishes = records.finishes.lock().unwrap();
        assert_eq!(finishes.len(), 1);
        assert_eq!(finishes[0].reason, Reason::ExecutionAbandoned);
        assert_eq!(finishes[0].attempts, 1);
    }
    task.abort();
    let _ = task.await;
}

#[tokio::test]
async fn cache_preread_ends_before_replay_drop_and_fresh_hit_has_zero_attempts() {
    let (addr, _) = aioduct_test_server::h1::h1_server_with(|_| async {
        Ok::<_, Infallible>(
            http::Response::builder()
                .header("cache-control", "max-age=60")
                .body(Full::new(Bytes::from_static(b"cached")))
                .unwrap(),
        )
    })
    .await;
    let url = format!("http://{addr}/");
    let records = Arc::new(Records::default());
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .observer(Recorder {
            records: records.clone(),
            interest: interests(),
        })
        .cache(aioduct::HttpCache::new())
        .build()
        .unwrap();
    let response = client.get(&url).unwrap().send().await.unwrap();
    assert_eq!(records.finishes.lock().unwrap().len(), 1);
    drop(response);
    let response = client.get(&url).unwrap().send().await.unwrap();
    assert_eq!(records.finishes.lock().unwrap().len(), 1);
    response.bytes().await.unwrap();
    let finished = records.finishes.lock().unwrap();
    assert_eq!(finished.len(), 2);
    assert_eq!(finished[0].reason, Reason::Ended);
    assert_eq!(finished[0].attempts, 1);
    assert_eq!(finished[0].bytes, Some(6));
    assert_eq!(finished[1].reason, Reason::Ended);
    assert_eq!(finished[1].attempts, 0);
    assert_eq!(
        finished[1].response.unwrap().source,
        ResponseSource::CacheHit
    );
}

#[tokio::test]
async fn bytes_only_delivers_finish_without_start_or_headers_callbacks() {
    let records = Arc::new(Records::default());
    let client = client(&records, Interest::RESPONSE_BODY_BYTES);
    client
        .get(&server().await)
        .unwrap()
        .send()
        .await
        .unwrap()
        .bytes()
        .await
        .unwrap();
    assert_eq!(records.starts.load(Ordering::Relaxed), 0);
    assert!(records.headers.lock().unwrap().is_empty());
    let finished = records.finishes.lock().unwrap();
    assert_eq!(finished.len(), 1);
    assert_eq!(finished[0].bytes, Some(5));
}

#[tokio::test]
async fn empty_interest_creates_no_followup_callbacks() {
    let records = Arc::new(Records::default());
    let client = client(&records, Interest::EMPTY);
    client
        .get(&server().await)
        .unwrap()
        .send()
        .await
        .unwrap()
        .bytes()
        .await
        .unwrap();
    assert_eq!(records.begins.load(Ordering::Relaxed), 1);
    assert_eq!(records.starts.load(Ordering::Relaxed), 0);
    assert!(records.headers.lock().unwrap().is_empty());
    assert!(records.finishes.lock().unwrap().is_empty());
}

#[tokio::test]
async fn cancelled_execution_is_not_consumer_abandonment() {
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let started_tx = Arc::new(Mutex::new(Some(started_tx)));
    let (addr, _) = aioduct_test_server::h1::h1_server_with(move |request| {
        let started_tx = started_tx.clone();
        async move {
            if request.uri().path() == "/prime" {
                return Ok::<_, Infallible>(http::Response::new(Full::new(Bytes::from_static(
                    b"prime",
                ))));
            }
            if let Some(tx) = started_tx.lock().unwrap().take() {
                let _ = tx.send(());
            }
            std::future::pending::<()>().await;
            Ok::<_, Infallible>(http::Response::new(Full::new(Bytes::new())))
        }
    })
    .await;
    let records = Arc::new(Records::default());
    let client = client(&records, interests());
    client
        .get(&format!("http://{addr}/prime"))
        .unwrap()
        .send()
        .await
        .unwrap()
        .bytes()
        .await
        .unwrap();
    let mut send = Box::pin(client.get(&format!("http://{addr}/")).unwrap().send());
    tokio::select! {
        result = &mut send => panic!("unexpected result: {result:?}"),
        _ = started_rx => {}
    }
    drop(send);
    let finished = records.finishes.lock().unwrap();
    assert_eq!(finished.len(), 2);
    assert_eq!(finished[1].reason, Reason::ExecutionAbandoned);
    assert_eq!(finished[1].attempts, 1);
    assert_eq!(finished[1].response, None);
}

#[cfg(feature = "gzip")]
#[tokio::test]
async fn summary_counts_encoded_data_not_decoded_output() {
    use std::io::Write;
    let data = vec![b'x'; 8192];
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(&data).unwrap();
    let encoded = Bytes::from(encoder.finish().unwrap());
    let encoded_len = encoded.len() as u64;
    let (addr, _) = aioduct_test_server::h1::h1_server_with(move |_| {
        let encoded = encoded.clone();
        async move {
            Ok::<_, Infallible>(
                http::Response::builder()
                    .header("content-encoding", "gzip")
                    .body(Full::new(encoded))
                    .unwrap(),
            )
        }
    })
    .await;
    let records = Arc::new(Records::default());
    let client = client(&records, interests());
    assert_eq!(
        client
            .get(&format!("http://{addr}/"))
            .unwrap()
            .send()
            .await
            .unwrap()
            .bytes()
            .await
            .unwrap(),
        data
    );
    let finished = records.finishes.lock().unwrap();
    assert_eq!(finished[0].reason, Reason::Ended);
    assert_eq!(finished[0].bytes, Some(encoded_len));
}

#[cfg(feature = "gzip")]
#[tokio::test]
async fn encoded_data_completion_does_not_finish_an_unconfirmed_decoder() {
    use std::io::Write;
    let data = vec![b'x'; 8192];
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(&data).unwrap();
    let encoded = Bytes::from(encoder.finish().unwrap());
    let encoded_len = encoded.len() as u64;
    let (addr, _) = aioduct_test_server::h1::h1_server_with(move |_| {
        let encoded = encoded.clone();
        async move {
            Ok::<_, Infallible>(
                http::Response::builder()
                    .header("content-encoding", "gzip")
                    .body(Full::new(encoded))
                    .unwrap(),
            )
        }
    })
    .await;
    let records = Arc::new(Records::default());
    let client = client(&records, interests());
    let mut body = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap()
        .into_bytes_stream();
    assert_eq!(body.next().await.unwrap().unwrap(), data);
    // The final original DATA was delivered, but the decoder has not polled
    // EOF/finalized. Observation must not drain or finish the decoder for us.
    assert!(records.finishes.lock().unwrap().is_empty());
    drop(body);
    let finishes = records.finishes.lock().unwrap();
    assert_eq!(finishes.len(), 1);
    assert_eq!(finishes[0].reason, Reason::ConsumerAbandoned);
    assert_eq!(finishes[0].bytes, Some(encoded_len));
}

#[cfg(feature = "compio")]
#[test]
fn local_observation_accepts_non_send_state_and_fixed_response() {
    use std::cell::RefCell;
    use std::io::{Read, Write};
    use std::rc::Rc;
    struct LocalObserver(Rc<RefCell<Vec<Reason>>>);
    impl Observer for LocalObserver {
        type Context = Rc<()>;
        type Observation = Self;
        type ConnectionObservation = aioduct::NoopConnectionObservation;
        fn begin_connection(&self) -> (Interest, Self::ConnectionObservation) {
            (Interest::EMPTY, aioduct::NoopConnectionObservation)
        }

        fn begin(&self, _: RequestInfo<'_>, _: Option<Self::Context>) -> (Interest, Self) {
            (Interest::REQUEST_FINISH, Self(self.0.clone()))
        }
    }

    impl RequestObservation for LocalObserver {
        fn on_request_finish(&self, event: &RequestFinish<'_>) {
            assert_eq!(event.attempt_count, 1);
            assert!(matches!(event.reason, RequestFinishReason::Ended));
            self.0.borrow_mut().push(Reason::Ended);
        }
    }
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        let mut head = Vec::new();
        while !head.ends_with(b"\r\n\r\n") {
            let mut byte = [0];
            socket.read_exact(&mut byte).unwrap();
            head.push(byte[0]);
        }
        socket
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nlocal")
            .unwrap();
    });
    let records = Rc::new(RefCell::new(Vec::new()));
    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = aioduct::HttpEngineLocal::<
            aioduct::runtime::compio_rt::CompioRuntime,
            aioduct::runtime::compio_rt::TcpConnector,
        >::builder()
        .observer(LocalObserver(records.clone()))
        .build_local()
        .unwrap();
        let response: aioduct::Response<aioduct::body::ResponseBodyLocal> = client
            .get_local(&format!("http://{addr}/"))
            .unwrap()
            .observation_context(Rc::new(()))
            .send()
            .await
            .unwrap();
        assert_eq!(response.bytes().await.unwrap(), "local");
    });
    server.join().unwrap();
    assert_eq!(*records.borrow(), vec![Reason::Ended]);
}

#[tokio::test]
async fn reliable_empty_body_and_last_data_do_not_require_an_extra_read() {
    let records = Arc::new(Records::default());
    let client = client(&records, interests());
    let url = server().await;
    drop(client.head(&url).unwrap().send().await.unwrap());
    assert_eq!(records.finishes.lock().unwrap()[0].reason, Reason::Ended);
    let mut body = client
        .get(&url)
        .unwrap()
        .send()
        .await
        .unwrap()
        .into_bytes_stream();
    assert_eq!(body.next().await.unwrap().unwrap(), "hello");
    drop(body);
    let finished = records.finishes.lock().unwrap();
    assert_eq!(finished.len(), 2);
    assert_eq!(finished[1].reason, Reason::Ended);
}

#[tokio::test]
async fn byte_stream_reports_only_one_body_terminal_fact() {
    let records = Arc::new(Records::default());
    let client = client(&records, Interest::REQUEST_FINISH | Interest::RESPONSE_BODY);
    let response = client.get(&server().await).unwrap().send().await.unwrap();
    let mut body = response.into_bytes_stream();
    while body.next().await.transpose().unwrap().is_some() {}

    assert_eq!(&*records.body_events.lock().unwrap(), &[BodyFact::Ended]);
    assert_eq!(records.finishes.lock().unwrap()[0].reason, Reason::Ended);
}

#[tokio::test]
async fn cache_revalidation_and_stale_fallback_keep_selected_identity_and_attempts() {
    for (upstream_status, source, directive) in [
        (304, ResponseSource::CacheRevalidated, "must-revalidate"),
        (
            503,
            ResponseSource::CacheStaleIfError,
            "stale-if-error=3600",
        ),
    ] {
        let requests = Arc::new(AtomicUsize::new(0));
        let server_requests = requests.clone();
        let (addr, _) = aioduct_test_server::h1::h1_server_with(move |request| {
            let server_requests = server_requests.clone();
            async move {
                let response = if server_requests.fetch_add(1, Ordering::Relaxed) == 0 {
                    http::Response::builder()
                        .header("cache-control", format!("max-age=0, {directive}"))
                        .header("etag", "\"v1\"")
                        .body(Full::new(Bytes::from_static(b"cached")))
                        .unwrap()
                } else {
                    assert_eq!(request.headers()["if-none-match"], "\"v1\"");
                    http::Response::builder()
                        .status(upstream_status)
                        .header("etag", "\"v1\"")
                        .body(Full::new(Bytes::new()))
                        .unwrap()
                };
                Ok::<_, Infallible>(response)
            }
        })
        .await;
        let records = Arc::new(Records::default());
        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
            .observer(Recorder {
                records: records.clone(),
                interest: interests(),
            })
            .cache(aioduct::HttpCache::new())
            .build()
            .unwrap();
        let url = format!("http://{addr}/#retained");
        for _ in 0..2 {
            let response = client.get(&url).unwrap().send().await.unwrap();
            assert_eq!(response.status(), http::StatusCode::OK);
            assert_eq!(response.fragment(), Some("retained"));
            assert_eq!(response.bytes().await.unwrap(), "cached");
        }
        let headers = records.headers.lock().unwrap();
        assert_eq!(headers.len(), 2);
        assert_eq!(headers[1].source, source);
        assert_eq!(headers[1].status, http::StatusCode::OK);
        let finishes = records.finishes.lock().unwrap();
        assert_eq!(finishes.len(), 2);
        assert_eq!(finishes[1].response, Some(headers[1]));
        assert_eq!(finishes[1].reason, Reason::Ended);
        assert_eq!(finishes[1].attempts, 1);
        assert_eq!(finishes[1].bytes, Some(6));
        assert_eq!(requests.load(Ordering::Relaxed), 2);
    }
}

#[tokio::test]
async fn sse_buffered_events_and_parser_errors_do_not_reopen_http_lifecycle() {
    let payload = b"data: first\n\ndata: second\n\n";
    let (addr, _) = aioduct_test_server::h1::h1_server_with(move |_| async move {
        Ok::<_, Infallible>(http::Response::new(Full::new(Bytes::from_static(payload))))
    })
    .await;
    let records = Arc::new(Records::default());
    let client = client(&records, interests());
    let mut sse: aioduct::sse::SseStreamSend = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap()
        .into_sse_stream();
    assert!(sse.next().await.unwrap().is_ok());
    assert_eq!(records.finishes.lock().unwrap().len(), 1);
    assert_eq!(records.finishes.lock().unwrap()[0].reason, Reason::Ended);
    drop(sse); // Unconsumed parser-buffered events are not HTTP abandonment.
    assert_eq!(records.finishes.lock().unwrap().len(), 1);

    let mut sse = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap()
        .into_sse_stream()
        .with_max_payload_size(1);
    assert!(matches!(
        sse.next().await.unwrap(),
        Err(aioduct::Error::SseEventTooLarge { .. })
    ));
    drop(sse);
    let finishes = records.finishes.lock().unwrap();
    assert_eq!(finishes.len(), 2);
    assert_eq!(finishes[1].reason, Reason::Ended);
    assert_eq!(finishes[1].bytes, Some(payload.len() as u64));
}

#[tokio::test]
async fn owned_builder_preserves_observation_context_and_opt_out() {
    use aioduct::traits::{HttpClient, RequestBuilderExt};
    let records = Arc::new(Records::default());
    let client = client(&records, interests());
    let url = server().await;
    HttpClient::get(&client, &url)
        .unwrap()
        .observation_context(12)
        .send()
        .await
        .unwrap()
        .bytes()
        .await
        .unwrap();
    HttpClient::get(&client, &url)
        .unwrap()
        .disable_observation()
        .send()
        .await
        .unwrap()
        .bytes()
        .await
        .unwrap();
    assert_eq!(*records.contexts.lock().unwrap(), vec![Some(12)]);
    assert_eq!(records.finishes.lock().unwrap().len(), 1);
}

#[cfg(all(feature = "blocking", feature = "smol"))]
#[test]
fn blocking_smol_observation_supports_context_and_opt_out() {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        for _ in 0..2 {
            let (mut socket, _) = listener.accept().unwrap();
            let mut headers = Vec::new();
            while !headers.ends_with(b"\r\n\r\n") {
                let mut byte = [0];
                socket.read_exact(&mut byte).unwrap();
                headers.push(byte[0]);
            }
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
                .unwrap();
        }
    });
    let records = Arc::new(Records::default());
    let engine = aioduct::SmolClient::builder()
        .observer(Recorder {
            records: records.clone(),
            interest: interests(),
        })
        .build()
        .unwrap();
    let client: aioduct::BlockingSmolClient<Recorder> =
        aioduct::blocking::BlockingClient::new(engine).unwrap();
    let url = format!("http://{addr}/");
    assert_eq!(
        client
            .get(&url)
            .unwrap()
            .observation_context(23)
            .send()
            .unwrap()
            .bytes()
            .unwrap(),
        "ok"
    );
    assert_eq!(
        client
            .get(&url)
            .unwrap()
            .disable_observation()
            .send()
            .unwrap()
            .bytes()
            .unwrap(),
        "ok"
    );
    server.join().unwrap();
    assert_eq!(*records.contexts.lock().unwrap(), vec![Some(23)]);
    assert_eq!(records.finishes.lock().unwrap().len(), 1);
}

#[cfg(feature = "gzip")]
#[tokio::test]
async fn cache_preread_decode_failure_keeps_headers_and_is_not_ended() {
    let (addr, _) = aioduct_test_server::h1::h1_server_with(|_| async {
        Ok::<_, Infallible>(
            http::Response::builder()
                .header("content-encoding", "gzip")
                .header("cache-control", "max-age=60")
                .body(Full::new(Bytes::from_static(b"not-gzip")))
                .unwrap(),
        )
    })
    .await;
    let records = Arc::new(Records::default());
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .observer(Recorder {
            records: records.clone(),
            interest: interests(),
        })
        .cache(aioduct::HttpCache::new())
        .build()
        .unwrap();
    assert!(
        client
            .get(&format!("http://{addr}/"))
            .unwrap()
            .send()
            .await
            .is_err()
    );
    assert_eq!(records.headers.lock().unwrap().len(), 1);
    let finished = records.finishes.lock().unwrap();
    assert_eq!(finished.len(), 1);
    assert_eq!(finished[0].reason, Reason::Failed);
    assert_eq!(finished[0].bytes, Some(8));
}

#[tokio::test]
async fn request_timeout_still_covers_internal_cache_preread() {
    // A raw server gives the cache a cacheable response, but holds the body.
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let cache_addr = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        while !request.ends_with(b"\r\n\r\n") {
            let mut byte = [0];
            socket.read_exact(&mut byte).await.unwrap();
            request.push(byte[0]);
        }
        socket
            .write_all(b"HTTP/1.1 200 OK\r\nCache-Control: max-age=60\r\nContent-Length: 5\r\n\r\n")
            .await
            .unwrap();
        std::future::pending::<()>().await;
    });
    let records = Arc::new(Records::default());
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .observer(Recorder {
            records: records.clone(),
            interest: interests(),
        })
        .cache(aioduct::HttpCache::new())
        .build()
        .unwrap();
    let error = client
        .get(&format!("http://{cache_addr}/"))
        .unwrap()
        .timeout(Duration::from_millis(30))
        .send()
        .await
        .unwrap_err();
    assert!(matches!(error.into_error(), aioduct::Error::Timeout));
    task.abort();
    let finished = records.finishes.lock().unwrap();
    assert_eq!(finished.len(), 1);
    assert_eq!(finished[0].reason, Reason::Failed);
    assert_eq!(records.headers.lock().unwrap().len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn request_callbacks_work_across_64_concurrent_threads() {
    let url = server().await;
    let records = Arc::new(Records::default());
    let client = client(&records, interests());
    // Poll requests on 64 OS threads while keeping pooled connection drivers
    // on the test runtime, which stays alive until every request has finished.
    // Separate short-lived runtimes can stop a driver that another thread is
    // still using through the shared pool.
    let runtime = tokio::runtime::Handle::current();
    let barrier = Arc::new(std::sync::Barrier::new(64));
    let threads: Vec<_> = (0..64)
        .map(|_| {
            let client = client.clone();
            let url = url.clone();
            let barrier = barrier.clone();
            let runtime = runtime.clone();
            std::thread::spawn(move || {
                barrier.wait();
                runtime.block_on(async {
                    assert_eq!(
                        client
                            .get(&url)
                            .unwrap()
                            // Allow for scheduling 64 OS threads on a loaded CI runner.
                            .timeout(Duration::from_secs(30))
                            .send()
                            .await
                            .unwrap()
                            .bytes()
                            .await
                            .unwrap(),
                        "hello"
                    );
                });
            })
        })
        .collect();
    tokio::task::spawn_blocking(move || {
        for thread in threads {
            thread.join().unwrap();
        }
    })
    .await
    .unwrap();
    assert_eq!(records.begins.load(Ordering::Relaxed), 64);
    assert_eq!(records.headers.lock().unwrap().len(), 64);
    let finished = records.finishes.lock().unwrap();
    assert_eq!(finished.len(), 64);
    assert!(finished.iter().all(|event| event.reason == Reason::Ended
        && event.attempts == 1
        && event.bytes == Some(5)));
}

#[tokio::test]
async fn upgrade_candidate_is_not_completed_by_its_empty_body() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut head = Vec::new();
        while !head.ends_with(b"\r\n\r\n") {
            head.push(socket.read_u8().await.unwrap());
        }
        socket
            .write_all(
                b"HTTP/1.1 101 Switching Protocols\r\nConnection: upgrade\r\nUpgrade: test\r\n\r\n",
            )
            .await
            .unwrap();
        std::future::pending::<()>().await;
    });
    let records = Arc::new(Records::default());
    let client = client(&records, interests());
    let response = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .header_str("connection", "upgrade")
        .unwrap()
        .header_str("upgrade", "test")
        .unwrap()
        .send()
        .await
        .unwrap();
    assert!(records.finishes.lock().unwrap().is_empty());
    let upgraded = response.upgrade().await.unwrap();
    assert_eq!(
        records.finishes.lock().unwrap()[0].reason,
        Reason::HandedOff
    );
    drop(upgraded);
    assert_eq!(records.finishes.lock().unwrap().len(), 1);
    server.abort();
}
