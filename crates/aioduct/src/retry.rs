use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
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
    /// Optional budget limiting total retry rate.
    pub budget: Option<RetryBudget>,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            max_retries: 3,
            initial_backoff: Duration::from_millis(100),
            max_backoff: Duration::from_secs(30),
            backoff_multiplier: 2.0,
            retry_on_status: true,
            budget: None,
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

    /// Attach a retry budget to limit the total retry rate.
    pub fn budget(mut self, budget: RetryBudget) -> Self {
        self.budget = Some(budget);
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

/// A token-bucket retry budget that prevents retry storms.
///
/// Each successful request deposits tokens (up to a cap). Each retry attempt
/// withdraws one token. When the budget is exhausted, retries are suppressed
/// until more tokens accumulate.
#[derive(Debug, Clone)]
pub struct RetryBudget {
    inner: Arc<RetryBudgetInner>,
}

#[derive(Debug)]
struct RetryBudgetInner {
    tokens: AtomicU32,
    max_tokens: u32,
    deposit_amount: u32,
}

impl RetryBudget {
    /// Create a retry budget.
    ///
    /// - `max_tokens`: maximum tokens in the bucket (retry capacity).
    /// - `deposit_per_success`: tokens added per successful (non-retried) request.
    pub fn new(max_tokens: u32, deposit_per_success: u32) -> Self {
        Self {
            inner: Arc::new(RetryBudgetInner {
                tokens: AtomicU32::new(max_tokens),
                max_tokens,
                deposit_amount: deposit_per_success,
            }),
        }
    }

    /// Try to withdraw one retry token. Returns `true` if a retry is allowed.
    pub(crate) fn try_withdraw(&self) -> bool {
        self.inner
            .tokens
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                if current > 0 { Some(current - 1) } else { None }
            })
            .is_ok()
    }

    /// Deposit tokens after a successful request.
    pub(crate) fn deposit(&self) {
        let inner = &self.inner;
        inner
            .tokens
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                let new = current.saturating_add(inner.deposit_amount);
                Some(new.min(inner.max_tokens))
            })
            .ok();
    }

    /// Returns the current number of available tokens.
    pub fn available(&self) -> u32 {
        self.inner.tokens.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_values() {
        let cfg = RetryConfig::default();
        assert_eq!(cfg.max_retries, 3);
        assert_eq!(cfg.initial_backoff, Duration::from_millis(100));
        assert_eq!(cfg.max_backoff, Duration::from_secs(30));
        assert!((cfg.backoff_multiplier - 2.0).abs() < f64::EPSILON);
        assert!(cfg.retry_on_status);
        assert!(cfg.budget.is_none());
    }

    #[test]
    fn builder_chain() {
        let cfg = RetryConfig::default()
            .max_retries(5)
            .initial_backoff(Duration::from_millis(200))
            .max_backoff(Duration::from_secs(60))
            .backoff_multiplier(3.0)
            .retry_on_status(false);
        assert_eq!(cfg.max_retries, 5);
        assert_eq!(cfg.initial_backoff, Duration::from_millis(200));
        assert_eq!(cfg.max_backoff, Duration::from_secs(60));
        assert!((cfg.backoff_multiplier - 3.0).abs() < f64::EPSILON);
        assert!(!cfg.retry_on_status);
    }

    #[test]
    fn delay_for_attempt_exponential() {
        let cfg = RetryConfig::default();
        assert_eq!(cfg.delay_for_attempt(0), Duration::from_millis(100));
        assert_eq!(cfg.delay_for_attempt(1), Duration::from_millis(200));
        assert_eq!(cfg.delay_for_attempt(2), Duration::from_millis(400));
        assert_eq!(cfg.delay_for_attempt(3), Duration::from_millis(800));
    }

    #[test]
    fn delay_capped_at_max_backoff() {
        let cfg = RetryConfig::default().max_backoff(Duration::from_millis(300));
        assert_eq!(cfg.delay_for_attempt(0), Duration::from_millis(100));
        assert_eq!(cfg.delay_for_attempt(1), Duration::from_millis(200));
        assert_eq!(cfg.delay_for_attempt(2), Duration::from_millis(300));
        assert_eq!(cfg.delay_for_attempt(10), Duration::from_millis(300));
    }

    #[test]
    fn is_retryable_for_io_hyper_timeout() {
        assert!(is_retryable_error(&Error::Timeout));
        assert!(is_retryable_error(&Error::Io(std::io::Error::other(
            "test"
        ))));
    }

    #[test]
    fn not_retryable_for_status_and_invalid_url() {
        assert!(!is_retryable_error(&Error::Status(
            http::StatusCode::NOT_FOUND
        )));
        assert!(!is_retryable_error(&Error::InvalidUrl("bad".into())));
        assert!(!is_retryable_error(&Error::Other("misc".into())));
    }

    #[test]
    fn budget_starts_full() {
        let budget = RetryBudget::new(10, 1);
        assert_eq!(budget.available(), 10);
    }

    #[test]
    fn budget_withdraw_exhaustion() {
        let budget = RetryBudget::new(3, 1);
        assert!(budget.try_withdraw());
        assert!(budget.try_withdraw());
        assert!(budget.try_withdraw());
        assert!(!budget.try_withdraw());
        assert_eq!(budget.available(), 0);
    }

    #[test]
    fn budget_deposit_adds_tokens() {
        let budget = RetryBudget::new(5, 2);
        budget.try_withdraw();
        budget.try_withdraw();
        assert_eq!(budget.available(), 3);
        budget.deposit();
        assert_eq!(budget.available(), 5);
    }

    #[test]
    fn budget_deposit_capped_at_max() {
        let budget = RetryBudget::new(3, 5);
        budget.deposit();
        assert_eq!(budget.available(), 3);
    }

    #[test]
    fn budget_clone_shares_state() {
        let a = RetryBudget::new(2, 1);
        let b = a.clone();
        assert!(a.try_withdraw());
        assert!(b.try_withdraw());
        assert!(!a.try_withdraw());
    }

    #[test]
    fn config_with_budget() {
        let budget = RetryBudget::new(10, 1);
        let cfg = RetryConfig::default().budget(budget);
        assert!(cfg.budget.is_some());
    }
}
