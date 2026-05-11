//! Metrics types and aggregation.

pub mod histogram;
pub mod registry;

use std::time::Instant;

use serde::Serialize;

/// A collection of key-value tag pairs attached to a metric sample.
pub type Tags = Vec<(&'static str, String)>;

/// A single metric sample emitted by a VU.
#[derive(Debug, Clone)]
pub struct Sample {
    /// Metric name (e.g. "ttft_ms", "http_req_duration").
    pub name: &'static str,
    /// The metric value.
    pub value: f64,
    /// Key-value tags for grouping/filtering.
    pub tags: Tags,
    /// When this sample was recorded.
    pub timestamp: Instant,
    /// Which VU produced this sample.
    pub vu_id: usize,
}

/// Per-request structured record emitted after each request completes.
#[derive(Debug, Clone, Serialize)]
pub struct RequestRecord {
    pub timestamp: String,
    pub traceparent: String,
    pub vu: usize,
    pub iteration: usize,
    pub status_code: u16,
    pub success: u8,
    pub latency_ms: f64,
    pub error_category: String,
    pub error_msg: String,
    /// Additional user-defined metrics for this request.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// Aggregated test summary computed at the end of a load test.
#[derive(Debug, Clone, Serialize)]
pub struct TestSummary {
    pub total_requests: u64,
    pub successful_requests: u64,
    pub failed_requests: u64,
    pub total_duration_secs: f64,
    pub requests_per_second: f64,
    /// Per-metric aggregations.
    pub metrics: Vec<MetricSummary>,
}

/// Aggregated statistics for a single metric name.
#[derive(Debug, Clone, Serialize)]
pub struct MetricSummary {
    pub name: String,
    pub count: usize,
    pub min: f64,
    pub max: f64,
    pub avg: f64,
    pub med: f64,
    pub p90: f64,
    pub p95: f64,
    pub p99: f64,
}
