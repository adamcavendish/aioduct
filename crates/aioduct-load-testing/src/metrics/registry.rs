//! Metrics registry for collecting and aggregating samples during a test run.

use std::collections::HashMap;

use super::histogram::Histogram;
use super::{MetricSummary, Sample};

/// Collects metric samples during a test and computes aggregated summaries.
pub struct MetricsRegistry {
    histograms: HashMap<&'static str, Histogram>,
}

impl MetricsRegistry {
    pub fn new() -> Self {
        Self {
            histograms: HashMap::new(),
        }
    }

    /// Record a sample into the registry.
    pub fn record(&mut self, sample: &Sample) {
        self.histograms
            .entry(sample.name)
            .or_default()
            .add(sample.value);
    }

    /// Compute summary statistics for all collected metrics.
    pub fn summarize(&mut self) -> Vec<MetricSummary> {
        self.histograms
            .iter_mut()
            .map(|(name, hist)| MetricSummary {
                name: name.to_string(),
                count: hist.count(),
                min: hist.min(),
                max: hist.max(),
                avg: hist.avg(),
                med: hist.median(),
                p90: hist.p90(),
                p95: hist.p95(),
                p99: hist.p99(),
            })
            .collect()
    }
}

impl Default for MetricsRegistry {
    fn default() -> Self {
        Self::new()
    }
}
