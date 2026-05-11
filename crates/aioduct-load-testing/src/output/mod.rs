//! Output trait and built-in output backends.

pub mod console;
pub mod csv;
pub mod jsonl;

use crate::metrics::{RequestRecord, Sample, TestSummary};

/// Pluggable output backend for receiving metrics and request records.
///
/// Implementations must be fast and non-blocking in `record()` and `request_done()`
/// since they are called on the hot path. Buffer internally if needed.
pub trait Output: Send + Sync + 'static {
    /// Called for each metric sample.
    fn record(&self, sample: &Sample);

    /// Called after each request completes with a structured record.
    fn request_done(&self, record: &RequestRecord);

    /// Called once at test end with aggregated results.
    fn summary(&self, summary: &TestSummary);

    /// Flush buffered data. Called during graceful shutdown.
    fn flush(&self);
}

/// Fan-out to multiple outputs.
pub struct OutputSet {
    outputs: Vec<Box<dyn Output>>,
}

impl OutputSet {
    pub fn new() -> Self {
        Self {
            outputs: Vec::new(),
        }
    }

    pub fn add(&mut self, output: impl Output) {
        self.outputs.push(Box::new(output));
    }

    pub fn record(&self, sample: &Sample) {
        for output in &self.outputs {
            output.record(sample);
        }
    }

    pub fn request_done(&self, record: &RequestRecord) {
        for output in &self.outputs {
            output.request_done(record);
        }
    }

    pub fn summary(&self, summary: &TestSummary) {
        for output in &self.outputs {
            output.summary(summary);
        }
    }

    pub fn flush(&self) {
        for output in &self.outputs {
            output.flush();
        }
    }
}

impl Default for OutputSet {
    fn default() -> Self {
        Self::new()
    }
}
