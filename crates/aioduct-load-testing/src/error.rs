//! Error types for the load testing framework.

use thiserror::Error;

/// Errors that can occur during load test execution.
#[derive(Debug, Error)]
pub enum Error {
    /// An error from the underlying HTTP client.
    #[error("http: {0}")]
    Http(#[from] aioduct::error::Error),

    /// An I/O error (file reading, output writing).
    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    /// JSON serialization/deserialization error.
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),

    /// CSV error.
    #[error("csv: {0}")]
    Csv(#[from] csv::Error),

    /// Data feeder exhausted — no more records available.
    #[error("feeder exhausted")]
    FeederExhausted,

    /// Configuration error.
    #[error("config: {0}")]
    Config(String),

    /// Scenario returned an error.
    #[error("scenario: {0}")]
    Scenario(String),
}

/// Result type for load testing operations.
pub type Result<T> = std::result::Result<T, Error>;
