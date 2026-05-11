//! Virtual User context — the handle passed to scenarios.

use std::sync::Arc;
use std::time::Instant;

use aioduct::HttpEngine;
use aioduct::runtime::{ConnectorSend, RuntimePoll};
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::cancel::CancellationToken;
use crate::feeder::Feeder;
use crate::metrics::{Sample, Tags};
use crate::output::OutputSet;

/// Context passed to each [`Scenario::run`](crate::scenario::Scenario::run) call.
///
/// Provides access to the shared HTTP client, data feeder, metric recording,
/// and cancellation signal.
pub struct VuContext<R: RuntimePoll, C: ConnectorSend> {
    /// This VU's numeric ID (0-based).
    pub vu_id: usize,
    /// Current iteration number (0-based, increments each call to `run()`).
    pub iteration: usize,

    client: HttpEngine<R, C>,
    feeder: Box<dyn Feeder>,
    outputs: Arc<OutputSet>,
    cancel: CancellationToken,
    start_time: Instant,
    last_status_code: u16,
}

impl<R: RuntimePoll, C: ConnectorSend> VuContext<R, C> {
    pub(crate) fn new(
        vu_id: usize,
        client: HttpEngine<R, C>,
        feeder: Box<dyn Feeder>,
        outputs: Arc<OutputSet>,
        cancel: CancellationToken,
    ) -> Self {
        Self {
            vu_id,
            iteration: 0,
            client,
            feeder,
            outputs,
            cancel,
            start_time: Instant::now(),
            last_status_code: 0,
        }
    }

    /// Get the shared HTTP client for making requests.
    pub fn client(&self) -> &HttpEngine<R, C> {
        &self.client
    }

    /// Record the HTTP status code for the current iteration.
    ///
    /// Call this after receiving a response so the framework can include
    /// it in the [`RequestRecord`](crate::metrics::RequestRecord).
    pub fn set_status_code(&mut self, code: u16) {
        self.last_status_code = code;
    }

    /// Get the status code recorded for the current iteration.
    pub(crate) fn last_status_code(&self) -> u16 {
        self.last_status_code
    }

    /// Get the next data record from the feeder, deserialized into `T`.
    /// Returns `Err(FeederExhausted)` when no more data is available.
    pub fn feed<T: DeserializeOwned>(&mut self) -> crate::error::Result<T> {
        let value = self
            .feeder
            .next_record()
            .ok_or(crate::error::Error::FeederExhausted)?;
        serde_json::from_value(value).map_err(Into::into)
    }

    /// Get the next raw JSON value from the feeder.
    pub fn feed_raw(&mut self) -> crate::error::Result<Value> {
        self.feeder
            .next_record()
            .ok_or(crate::error::Error::FeederExhausted)
    }

    /// Record a metric sample.
    pub fn record(&self, name: &'static str, value: f64) {
        self.record_with_tags(name, value, Vec::new());
    }

    /// Record a metric sample with tags.
    pub fn record_with_tags(&self, name: &'static str, value: f64, tags: Tags) {
        let sample = Sample {
            name,
            value,
            tags,
            timestamp: Instant::now(),
            vu_id: self.vu_id,
        };
        self.outputs.record(&sample);
    }

    /// Mark the current request as successful.
    pub fn success(&self) {
        self.record("request_success", 1.0);
    }

    /// Mark the current request as failed.
    pub fn fail(&self, _reason: &str) {
        self.record("request_fail", 1.0);
    }

    /// Check if cancellation has been requested.
    pub fn is_cancelled(&self) -> bool {
        self.cancel.is_cancelled()
    }

    /// Async wait — resolves when cancellation is triggered.
    pub async fn cancelled(&self) {
        self.cancel.cancelled().await;
    }

    /// Elapsed time since this VU started.
    pub fn elapsed(&self) -> std::time::Duration {
        self.start_time.elapsed()
    }

    pub(crate) fn increment_iteration(&mut self) {
        self.iteration += 1;
        self.last_status_code = 0;
    }
}
