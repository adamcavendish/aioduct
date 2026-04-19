use std::time::Duration;

use crate::error::Error;

/// Configuration for automatic request retry with exponential backoff.
#[derive(Debug, Clone)]
pub struct RetryConfig {
    /// Maximum number of retry attempts.
    pub max_retries: u32,
    /// Delay before the first retry.
    pub initial_backoff: Duration,
    /// Maximum delay between retries.
    pub max_backoff: Duration,
    /// Multiplier applied to backoff on each attempt.
    pub backoff_multiplier: f64,
    /// Whether to retry on 5xx server errors.
    pub retry_on_status: bool,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            max_retries: 3,
            initial_backoff: Duration::from_millis(100),
            max_backoff: Duration::from_secs(30),
            backoff_multiplier: 2.0,
            retry_on_status: true,
        }
    }
}

impl RetryConfig {
    /// Set the maximum retry count.
    pub fn max_retries(mut self, n: u32) -> Self {
        self.max_retries = n;
        self
    }

    /// Set the initial backoff duration.
    pub fn initial_backoff(mut self, d: Duration) -> Self {
        self.initial_backoff = d;
        self
    }

    /// Set the maximum backoff duration.
    pub fn max_backoff(mut self, d: Duration) -> Self {
        self.max_backoff = d;
        self
    }

    /// Set the backoff multiplier.
    pub fn backoff_multiplier(mut self, m: f64) -> Self {
        self.backoff_multiplier = m;
        self
    }

    /// Enable or disable retry on server errors.
    pub fn retry_on_status(mut self, enabled: bool) -> Self {
        self.retry_on_status = enabled;
        self
    }

    pub(crate) fn delay_for_attempt(&self, attempt: u32) -> Duration {
        let millis =
            self.initial_backoff.as_millis() as f64 * self.backoff_multiplier.powi(attempt as i32);
        let delay = Duration::from_millis(millis as u64);
        delay.min(self.max_backoff)
    }
}

pub(crate) fn is_retryable_error(err: &Error) -> bool {
    matches!(err, Error::Io(_) | Error::Hyper(_) | Error::Timeout)
}
