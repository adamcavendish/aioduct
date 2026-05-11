//! LoadTest builder and controller — orchestrates the entire test run.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use aioduct::HttpEngine;
use aioduct::runtime::{ConnectorSend, RuntimePoll};

use crate::cancel::CancellationToken;
use crate::executor::Executor;
use crate::feeder::FeederFactory;
use crate::metrics::{RequestRecord, TestSummary};
use crate::output::OutputSet;
use crate::scenario::Scenario;
use crate::vu::VuContext;

/// Builder for configuring and running a load test.
pub struct LoadTest<R: RuntimePoll, C: ConnectorSend> {
    client: Option<HttpEngine<R, C>>,
    scenario: Option<Arc<dyn Scenario<R, C>>>,
    executor: Executor,
    feeder_factory: Option<Box<dyn FeederFactory>>,
    outputs: OutputSet,
    stagger_interval: Duration,
}

impl<R: RuntimePoll, C: ConnectorSend> LoadTest<R, C> {
    /// Create a new load test builder.
    pub fn new() -> Self {
        Self {
            client: None,
            scenario: None,
            executor: Executor::PerVuIterations {
                vus: 1,
                iterations: 1,
            },
            feeder_factory: None,
            outputs: OutputSet::new(),
            stagger_interval: Duration::from_millis(10),
        }
    }

    /// Set the HTTP client. If not set, a default client is built.
    pub fn client(mut self, client: HttpEngine<R, C>) -> Self {
        self.client = Some(client);
        self
    }

    /// Set the scenario to run.
    pub fn scenario(mut self, scenario: impl Scenario<R, C>) -> Self {
        self.scenario = Some(Arc::new(scenario));
        self
    }

    /// Set the executor (controls VU count, duration, iterations).
    pub fn executor(mut self, executor: Executor) -> Self {
        self.executor = executor;
        self
    }

    /// Set the data feeder factory.
    pub fn feeder(mut self, factory: impl FeederFactory) -> Self {
        self.feeder_factory = Some(Box::new(factory));
        self
    }

    /// Add an output backend.
    pub fn output(mut self, output: impl crate::output::Output) -> Self {
        self.outputs.add(output);
        self
    }

    /// Set the stagger interval between VU starts.
    pub fn stagger_interval(mut self, interval: Duration) -> Self {
        self.stagger_interval = interval;
        self
    }

    /// Run the load test. Blocks until completion or cancellation.
    pub async fn run(self) -> TestSummary
    where
        C: Default,
    {
        let client = self
            .client
            .unwrap_or_else(|| HttpEngine::builder(C::default()).build());
        let scenario = self
            .scenario
            .expect("scenario must be set before calling run()");
        let vus = self.executor.vus();
        let outputs = Arc::new(self.outputs);
        let cancel = CancellationToken::new();
        let start = Instant::now();

        let total_requests = Arc::new(AtomicU64::new(0));
        let successful_requests = Arc::new(AtomicU64::new(0));
        let failed_requests = Arc::new(AtomicU64::new(0));

        // Duration-based cancellation for timed executors
        let duration_limit = match &self.executor {
            Executor::ConstantVus { duration, .. } => Some(*duration),
            Executor::ConstantArrivalRate { duration, .. } => Some(*duration),
            _ => None,
        };
        if let Some(dur) = duration_limit {
            let cancel_clone = cancel.clone();
            R::spawn_send(async move {
                R::sleep(dur).await;
                cancel_clone.cancel();
            });
        }

        // RampingVus: schedule cancellation at end of all stages
        if let Executor::RampingVus { stages } = &self.executor {
            let total_dur: Duration = stages.iter().map(|(_, d)| *d).sum();
            let cancel_clone = cancel.clone();
            R::spawn_send(async move {
                R::sleep(total_dur).await;
                cancel_clone.cancel();
            });
        }

        // Shared iteration counter for SharedIterations executor
        let shared_iter_counter = Arc::new(AtomicU64::new(0));
        let max_shared_iters = match &self.executor {
            Executor::SharedIterations { iterations, .. } => Some(*iterations as u64),
            _ => None,
        };

        // Per-VU iterations limit
        let per_vu_iters = match &self.executor {
            Executor::PerVuIterations { iterations, .. } => Some(*iterations),
            _ => None,
        };

        // Channel for VU completion notification (replaces spin-wait)
        let (done_tx, done_rx) = async_channel::bounded::<()>(vus);

        // Spawn VU tasks
        for vu_id in 0..vus {
            let client = client.clone();
            let scenario = scenario.clone();
            let outputs = outputs.clone();
            let cancel = cancel.child_token();
            let stagger = self.stagger_interval * vu_id as u32;
            let total_req = total_requests.clone();
            let success_req = successful_requests.clone();
            let fail_req = failed_requests.clone();
            let shared_counter = shared_iter_counter.clone();
            let done_tx = done_tx.clone();

            let feeder: Box<dyn crate::feeder::Feeder> =
                if let Some(ref factory) = self.feeder_factory {
                    factory.create(vu_id, vus)
                } else {
                    Box::new(NullFeeder)
                };

            R::spawn_send(async move {
                if !stagger.is_zero() {
                    R::sleep(stagger).await;
                }

                let mut ctx =
                    VuContext::new(vu_id, client, feeder, outputs.clone(), cancel.clone());

                if let Err(e) = scenario.on_start(&mut ctx).await {
                    eprintln!("[VU{vu_id}] on_start error: {e}");
                }

                loop {
                    if cancel.is_cancelled() {
                        break;
                    }

                    if let Some(max) = per_vu_iters
                        && ctx.iteration >= max
                    {
                        break;
                    }
                    if let Some(max) = max_shared_iters {
                        let idx = shared_counter.fetch_add(1, Ordering::Relaxed);
                        if idx >= max {
                            break;
                        }
                    }

                    let iter_start = Instant::now();
                    let result = scenario.run(&mut ctx).await;
                    let latency = iter_start.elapsed();

                    total_req.fetch_add(1, Ordering::Relaxed);
                    match &result {
                        Ok(()) => {
                            success_req.fetch_add(1, Ordering::Relaxed);
                        }
                        Err(e) => {
                            fail_req.fetch_add(1, Ordering::Relaxed);
                            eprintln!("[VU{vu_id}] iter={} error: {e}", ctx.iteration);
                        }
                    }

                    let record = RequestRecord {
                        timestamp: chrono::Utc::now().to_rfc3339(),
                        traceparent: String::new(),
                        vu: vu_id,
                        iteration: ctx.iteration,
                        status_code: ctx.last_status_code(),
                        success: u8::from(result.is_ok()),
                        latency_ms: latency.as_secs_f64() * 1000.0,
                        error_category: String::new(),
                        error_msg: result.err().map(|e| e.to_string()).unwrap_or_default(),
                        extra: serde_json::Map::new(),
                    };
                    outputs.request_done(&record);

                    ctx.increment_iteration();
                }

                if let Err(e) = scenario.on_stop(&mut ctx).await {
                    eprintln!("[VU{vu_id}] on_stop error: {e}");
                }

                let _ = done_tx.send(()).await;
            });
        }

        // Drop our copy so the channel closes when all VUs finish
        drop(done_tx);

        // Wait for all VUs to complete
        for _ in 0..vus {
            let _ = done_rx.recv().await;
        }

        let elapsed = start.elapsed();
        let total = total_requests.load(Ordering::Relaxed);
        let success = successful_requests.load(Ordering::Relaxed);
        let fail = failed_requests.load(Ordering::Relaxed);

        let summary = TestSummary {
            total_requests: total,
            successful_requests: success,
            failed_requests: fail,
            total_duration_secs: elapsed.as_secs_f64(),
            requests_per_second: if elapsed.as_secs_f64() > 0.0 {
                total as f64 / elapsed.as_secs_f64()
            } else {
                0.0
            },
            metrics: Vec::new(),
        };

        outputs.summary(&summary);
        outputs.flush();

        summary
    }
}

impl<R: RuntimePoll, C: ConnectorSend> Default for LoadTest<R, C> {
    fn default() -> Self {
        Self::new()
    }
}

/// A feeder that always returns None (for tests without data).
struct NullFeeder;

impl crate::feeder::Feeder for NullFeeder {
    fn next_record(&mut self) -> Option<serde_json::Value> {
        None
    }
}
