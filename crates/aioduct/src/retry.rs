use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use http::{HeaderMap, StatusCode};

use crate::error::Error;

/// Configuration for automatic request retry with exponential backoff.
#[derive(Clone)]
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
    /// Optional custom classifier consulted before the built-in retry rules.
    pub(crate) classifier: Option<Classifier>,
}

type Classifier = Arc<dyn Fn(&RetryContext<'_>) -> RetryDecision + Send + Sync>;

impl std::fmt::Debug for RetryConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RetryConfig")
            .field("max_retries", &self.max_retries)
            .field("initial_backoff", &self.initial_backoff)
            .field("max_backoff", &self.max_backoff)
            .field("backoff_multiplier", &self.backoff_multiplier)
            .field("retry_on_status", &self.retry_on_status)
            .field("budget", &self.budget)
            .field("classifier", &self.classifier.as_ref().map(|_| "<fn>"))
            .finish()
    }
}

/// What produced a retry decision point: a response status or a transport error.
#[non_exhaustive]
#[derive(Debug)]
pub enum RetryOutcome<'a> {
    /// The server returned a final response with this status.
    Status(StatusCode),
    /// The attempt failed with a transport-level error before a full response.
    Error(&'a Error),
}

/// Context passed to a custom retry classifier.
///
/// Use the accessors to inspect what happened and decide whether to retry.
#[derive(Debug)]
pub struct RetryContext<'a> {
    outcome: RetryOutcome<'a>,
    method: &'a http::Method,
    attempt: u32,
    max_retries: u32,
}

impl<'a> RetryContext<'a> {
    /// What produced this decision point (a status or a transport error).
    pub fn outcome(&self) -> &RetryOutcome<'a> {
        &self.outcome
    }

    /// The request method (useful for idempotency-aware policies).
    pub fn method(&self) -> &http::Method {
        self.method
    }

    /// The zero-based index of the attempt that just finished.
    pub fn attempt(&self) -> u32 {
        self.attempt
    }

    /// The configured maximum number of retries.
    pub fn max_retries(&self) -> u32 {
        self.max_retries
    }
}

/// A custom classifier's decision for a single retry decision point.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryDecision {
    /// Retry the request (subject to `max_retries` and any retry budget).
    Retry,
    /// Do not retry; return the response or error to the caller.
    DoNotRetry,
    /// Defer to the built-in classification rules.
    UseDefault,
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
            classifier: None,
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

    /// Set the backoff multiplier. Must be finite and non-negative.
    ///
    /// # Panics
    /// Panics if `m` is NaN, infinite, or negative.
    pub fn backoff_multiplier(mut self, m: f64) -> Self {
        assert!(
            m.is_finite() && m >= 0.0,
            "backoff_multiplier must be finite and non-negative, got {m}"
        );
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

    /// Set a custom classifier consulted before the built-in retry rules.
    ///
    /// The closure receives a [`RetryContext`] describing the status or error,
    /// the request method, and the attempt counters. Return:
    /// - [`RetryDecision::Retry`] to retry (still bounded by [`max_retries`] and
    ///   any [`budget`]). This is an explicit opt-in, so it applies even to
    ///   non-idempotent methods.
    /// - [`RetryDecision::DoNotRetry`] to stop and return the response or error.
    /// - [`RetryDecision::UseDefault`] to defer to the built-in classification.
    ///
    /// [`max_retries`]: Self::max_retries
    /// [`budget`]: Self::budget
    pub fn classify<F>(mut self, classifier: F) -> Self
    where
        F: Fn(&RetryContext<'_>) -> RetryDecision + Send + Sync + 'static,
    {
        self.classifier = Some(Arc::new(classifier));
        self
    }

    /// Evaluate the custom classifier for a status outcome, if one is set.
    pub(crate) fn classify_status(
        &self,
        status: StatusCode,
        method: &http::Method,
        attempt: u32,
    ) -> RetryDecision {
        match &self.classifier {
            Some(f) => f(&RetryContext {
                outcome: RetryOutcome::Status(status),
                method,
                attempt,
                max_retries: self.max_retries,
            }),
            None => RetryDecision::UseDefault,
        }
    }

    /// Evaluate the custom classifier for an error outcome, if one is set.
    pub(crate) fn classify_error(
        &self,
        error: &Error,
        method: &http::Method,
        attempt: u32,
    ) -> RetryDecision {
        match &self.classifier {
            Some(f) => f(&RetryContext {
                outcome: RetryOutcome::Error(error),
                method,
                attempt,
                max_retries: self.max_retries,
            }),
            None => RetryDecision::UseDefault,
        }
    }

    pub(crate) fn delay_for_attempt(&self, attempt: u32) -> Duration {
        let millis =
            self.initial_backoff.as_millis() as f64 * self.backoff_multiplier.powi(attempt as i32);
        let max_millis = self.max_backoff.as_millis() as f64;
        let clamped = millis.min(max_millis).max(0.0);
        let delay = Duration::from_millis(clamped as u64);
        delay.min(self.max_backoff)
    }
}

pub(crate) fn is_retryable_error(err: &Error) -> bool {
    match err {
        Error::Io(_)
        | Error::Hyper(_)
        | Error::Timeout
        | Error::ConnectTimeout
        | Error::ReadTimeout => true,
        Error::RemoteAddr { source, .. } => source
            .downcast_ref::<Error>()
            .is_some_and(is_retryable_error),
        _ => false,
    }
}

pub(crate) fn is_retryable_status(status: StatusCode) -> bool {
    status.is_server_error()
        || matches!(
            status,
            StatusCode::REQUEST_TIMEOUT | StatusCode::TOO_EARLY | StatusCode::TOO_MANY_REQUESTS
        )
}

pub(crate) fn is_idempotent(method: &http::Method) -> bool {
    matches!(
        *method,
        http::Method::GET
            | http::Method::HEAD
            | http::Method::PUT
            | http::Method::DELETE
            | http::Method::OPTIONS
            | http::Method::TRACE
    )
}

pub(crate) fn parse_retry_after(headers: &HeaderMap) -> Option<Duration> {
    let value = headers.get(http::header::RETRY_AFTER)?;
    let s = value.to_str().ok()?;

    if let Ok(secs) = s.trim().parse::<u64>() {
        return Some(Duration::from_secs(secs));
    }

    let target = parse_http_date(s.trim())?;
    let now = std::time::SystemTime::now();
    target.duration_since(now).ok()
}

fn parse_http_date(s: &str) -> Option<std::time::SystemTime> {
    let parts: Vec<&str> = s.split_whitespace().collect();
    if parts.len() < 6 {
        return None;
    }

    let day: u64 = parts[1].parse().ok()?;
    let month = match parts[2].to_lowercase().as_str() {
        "jan" => 1u64,
        "feb" => 2,
        "mar" => 3,
        "apr" => 4,
        "may" => 5,
        "jun" => 6,
        "jul" => 7,
        "aug" => 8,
        "sep" => 9,
        "oct" => 10,
        "nov" => 11,
        "dec" => 12,
        _ => return None,
    };
    let year: u64 = parts[3].parse().ok()?;
    let time_parts: Vec<&str> = parts[4].split(':').collect();
    if time_parts.len() != 3 {
        return None;
    }
    let hour: u64 = time_parts[0].parse().ok()?;
    let min: u64 = time_parts[1].parse().ok()?;
    let sec: u64 = time_parts[2].parse().ok()?;

    let days_before_month = [0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334];
    let m = month.checked_sub(1)? as usize;
    if m >= 12 {
        return None;
    }

    let mut days = year.checked_sub(1970)? * 365;
    if year > 1970 {
        days += (year - 1) / 4 - 1969 / 4;
        days -= (year - 1) / 100 - 1969 / 100;
        days += (year - 1) / 400 - 1969 / 400;
    }
    days += days_before_month[m];
    if month > 2
        && (year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400)))
    {
        days += 1;
    }
    days += day.checked_sub(1)?;

    let unix_secs = days * 86400 + hour * 3600 + min * 60 + sec;
    Some(std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(unix_secs))
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
        assert!(cfg.classifier.is_none());
    }

    #[test]
    fn classifier_status_overrides_default() {
        // A 404 is normally not retryable; the classifier forces a retry.
        let cfg = RetryConfig::default().classify(|ctx| match ctx.outcome() {
            RetryOutcome::Status(s) if *s == StatusCode::NOT_FOUND => RetryDecision::Retry,
            _ => RetryDecision::UseDefault,
        });
        assert_eq!(
            cfg.classify_status(StatusCode::NOT_FOUND, &http::Method::GET, 0),
            RetryDecision::Retry
        );
        // 500 falls through to UseDefault (built-in rules still apply).
        assert_eq!(
            cfg.classify_status(StatusCode::INTERNAL_SERVER_ERROR, &http::Method::GET, 0),
            RetryDecision::UseDefault
        );
    }

    #[test]
    fn classifier_can_suppress_default_retry() {
        // 503 is normally retryable; the classifier suppresses it.
        let cfg = RetryConfig::default().classify(|ctx| match ctx.outcome() {
            RetryOutcome::Status(s) if *s == StatusCode::SERVICE_UNAVAILABLE => {
                RetryDecision::DoNotRetry
            }
            _ => RetryDecision::UseDefault,
        });
        assert_eq!(
            cfg.classify_status(StatusCode::SERVICE_UNAVAILABLE, &http::Method::GET, 0),
            RetryDecision::DoNotRetry
        );
    }

    #[test]
    fn classifier_error_outcome_and_context() {
        let cfg = RetryConfig::default().classify(|ctx| {
            assert_eq!(ctx.method(), &http::Method::POST);
            assert_eq!(ctx.max_retries(), 3);
            match ctx.outcome() {
                RetryOutcome::Error(e) => {
                    if e.is_timeout() {
                        RetryDecision::DoNotRetry
                    } else {
                        RetryDecision::Retry
                    }
                }
                RetryOutcome::Status(_) => RetryDecision::UseDefault,
            }
        });
        assert_eq!(
            cfg.classify_error(&Error::Timeout, &http::Method::POST, 1),
            RetryDecision::DoNotRetry
        );
        assert_eq!(
            cfg.classify_error(&Error::Other("boom".into()), &http::Method::POST, 1),
            RetryDecision::Retry
        );
    }

    #[test]
    fn no_classifier_defaults_to_use_default() {
        let cfg = RetryConfig::default();
        assert_eq!(
            cfg.classify_status(StatusCode::OK, &http::Method::GET, 0),
            RetryDecision::UseDefault
        );
        assert_eq!(
            cfg.classify_error(&Error::Timeout, &http::Method::GET, 0),
            RetryDecision::UseDefault
        );
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
    fn retryable_statuses_include_5xx_and_explicit_4xx() {
        assert!(is_retryable_status(StatusCode::INTERNAL_SERVER_ERROR));
        assert!(is_retryable_status(StatusCode::BAD_GATEWAY));
        assert!(is_retryable_status(StatusCode::SERVICE_UNAVAILABLE));
        assert!(is_retryable_status(StatusCode::REQUEST_TIMEOUT));
        assert!(is_retryable_status(StatusCode::TOO_EARLY));
        assert!(is_retryable_status(StatusCode::TOO_MANY_REQUESTS));
    }

    #[test]
    fn retryable_statuses_exclude_other_4xx_and_success() {
        assert!(!is_retryable_status(StatusCode::OK));
        assert!(!is_retryable_status(StatusCode::BAD_REQUEST));
        assert!(!is_retryable_status(StatusCode::UNAUTHORIZED));
        assert!(!is_retryable_status(StatusCode::NOT_FOUND));
        assert!(!is_retryable_status(StatusCode::CONFLICT));
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

    #[test]
    fn parse_retry_after_seconds() {
        let mut headers = HeaderMap::new();
        headers.insert(http::header::RETRY_AFTER, "120".parse().unwrap());
        let delay = parse_retry_after(&headers).unwrap();
        assert_eq!(delay, Duration::from_secs(120));
    }

    #[test]
    fn parse_retry_after_zero() {
        let mut headers = HeaderMap::new();
        headers.insert(http::header::RETRY_AFTER, "0".parse().unwrap());
        let delay = parse_retry_after(&headers).unwrap();
        assert_eq!(delay, Duration::from_secs(0));
    }

    #[test]
    fn parse_retry_after_missing() {
        let headers = HeaderMap::new();
        assert!(parse_retry_after(&headers).is_none());
    }

    #[test]
    fn parse_retry_after_invalid() {
        let mut headers = HeaderMap::new();
        headers.insert(http::header::RETRY_AFTER, "not-a-number".parse().unwrap());
        assert!(parse_retry_after(&headers).is_none());
    }

    #[test]
    fn parse_http_date_valid() {
        let dt = parse_http_date("Wed, 21 Oct 2015 07:28:00 GMT");
        assert!(dt.is_some());
        let secs = dt
            .unwrap()
            .duration_since(std::time::SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        assert_eq!(secs, 1445412480);
    }

    #[test]
    fn parse_http_date_too_few_parts() {
        assert!(parse_http_date("Wed, 21 Oct").is_none());
    }

    #[test]
    fn parse_http_date_bad_month() {
        assert!(parse_http_date("Wed, 21 Xyz 2015 07:28:00 GMT").is_none());
    }

    #[test]
    fn parse_http_date_bad_day() {
        assert!(parse_http_date("Wed, XX Oct 2015 07:28:00 GMT").is_none());
    }

    #[test]
    fn parse_http_date_bad_year() {
        assert!(parse_http_date("Wed, 21 Oct XXXX 07:28:00 GMT").is_none());
    }

    #[test]
    fn parse_http_date_bad_time_format() {
        assert!(parse_http_date("Wed, 21 Oct 2015 07:28 GMT").is_none());
    }

    #[test]
    fn parse_http_date_bad_time_values() {
        assert!(parse_http_date("Wed, 21 Oct 2015 AA:BB:CC GMT").is_none());
    }

    #[test]
    fn parse_http_date_all_months() {
        let months = [
            "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
        ];
        for m in months {
            let date = format!("Wed, 15 {m} 2020 12:00:00 GMT");
            assert!(parse_http_date(&date).is_some(), "failed for month {m}");
        }
    }

    #[test]
    fn parse_http_date_leap_year() {
        let dt = parse_http_date("Sat, 29 Feb 2020 00:00:00 GMT");
        assert!(dt.is_some());
    }

    #[test]
    fn parse_retry_after_http_date_in_future() {
        let future = std::time::SystemTime::now() + Duration::from_secs(3600);
        let secs = future
            .duration_since(std::time::SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let epoch = std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(secs);
        let dur = epoch
            .duration_since(std::time::SystemTime::UNIX_EPOCH)
            .unwrap();
        let days = dur.as_secs() / 86400;
        let year = 1970 + days / 365;
        let formatted = format!("Wed, 01 Jan {year} 12:00:00 GMT");
        let mut headers = HeaderMap::new();
        headers.insert(http::header::RETRY_AFTER, formatted.parse().unwrap());
        // May or may not parse depending on exact date math, just ensure no panic
        let _ = parse_retry_after(&headers);
    }

    #[test]
    fn idempotent_methods() {
        assert!(is_idempotent(&http::Method::GET));
        assert!(is_idempotent(&http::Method::HEAD));
        assert!(is_idempotent(&http::Method::PUT));
        assert!(is_idempotent(&http::Method::DELETE));
        assert!(is_idempotent(&http::Method::OPTIONS));
        assert!(is_idempotent(&http::Method::TRACE));
    }

    #[test]
    fn non_idempotent_methods() {
        assert!(!is_idempotent(&http::Method::POST));
        assert!(!is_idempotent(&http::Method::PATCH));
    }

    #[test]
    #[should_panic(expected = "backoff_multiplier must be finite and non-negative")]
    fn backoff_multiplier_rejects_nan() {
        RetryConfig::default().backoff_multiplier(f64::NAN);
    }

    #[test]
    #[should_panic(expected = "backoff_multiplier must be finite and non-negative")]
    fn backoff_multiplier_rejects_infinity() {
        RetryConfig::default().backoff_multiplier(f64::INFINITY);
    }

    #[test]
    #[should_panic(expected = "backoff_multiplier must be finite and non-negative")]
    fn backoff_multiplier_rejects_negative() {
        RetryConfig::default().backoff_multiplier(-1.0);
    }

    #[test]
    fn backoff_multiplier_accepts_zero() {
        let cfg = RetryConfig::default().backoff_multiplier(0.0);
        assert_eq!(cfg.delay_for_attempt(1), Duration::ZERO);
    }

    // ────────────────────────────────────────────────────────
    // is_retryable_status full coverage
    // ────────────────────────────────────────────────────────

    #[test]
    fn is_retryable_status_too_early_is_retryable() {
        assert!(is_retryable_status(StatusCode::TOO_EARLY));
    }

    #[test]
    fn is_retryable_status_too_many_requests_is_retryable() {
        assert!(is_retryable_status(StatusCode::TOO_MANY_REQUESTS));
    }

    #[test]
    fn is_retryable_status_service_unavailable_is_retryable() {
        assert!(is_retryable_status(StatusCode::SERVICE_UNAVAILABLE));
    }

    #[test]
    fn is_retryable_status_internal_server_error_is_retryable() {
        assert!(is_retryable_status(StatusCode::INTERNAL_SERVER_ERROR));
    }

    #[test]
    fn is_retryable_status_ok_not_retryable() {
        assert!(!is_retryable_status(StatusCode::OK));
    }

    #[test]
    fn is_retryable_status_bad_request_not_retryable() {
        assert!(!is_retryable_status(StatusCode::BAD_REQUEST));
    }

    #[test]
    fn is_retryable_status_unauthorized_not_retryable() {
        assert!(!is_retryable_status(StatusCode::UNAUTHORIZED));
    }

    #[test]
    fn is_retryable_status_not_found_not_retryable() {
        assert!(!is_retryable_status(StatusCode::NOT_FOUND));
    }

    #[test]
    fn is_retryable_status_moved_permanently_not_retryable() {
        assert!(!is_retryable_status(StatusCode::MOVED_PERMANENTLY));
    }

    // ────────────────────────────────────────────────────────
    // parse_retry_after edge cases
    // ────────────────────────────────────────────────────────

    #[test]
    fn parse_retry_after_whitespace_around_value() {
        let mut headers = HeaderMap::new();
        headers.insert(http::header::RETRY_AFTER, " 120 ".parse().unwrap());
        let delay = parse_retry_after(&headers).unwrap();
        assert_eq!(delay, Duration::from_secs(120));
    }

    #[test]
    fn parse_retry_after_very_large_value_no_panic() {
        let mut headers = HeaderMap::new();
        headers.insert(http::header::RETRY_AFTER, "999999999".parse().unwrap());
        let delay = parse_retry_after(&headers).unwrap();
        assert_eq!(delay, Duration::from_secs(999999999));
    }

    #[test]
    fn parse_retry_after_empty_string_returns_none() {
        let mut headers = HeaderMap::new();
        headers.insert(http::header::RETRY_AFTER, "".parse().unwrap());
        assert!(parse_retry_after(&headers).is_none());
    }

    #[test]
    fn parse_retry_after_whitespace_only_returns_none() {
        let mut headers = HeaderMap::new();
        headers.insert(http::header::RETRY_AFTER, "   ".parse().unwrap());
        assert!(parse_retry_after(&headers).is_none());
    }

    // ────────────────────────────────────────────────────────
    // RetryBudget edge cases
    // ────────────────────────────────────────────────────────

    #[test]
    fn budget_max_tokens_zero() {
        let budget = RetryBudget::new(0, 1);
        assert_eq!(budget.available(), 0);
        assert!(!budget.try_withdraw());
    }

    #[test]
    fn budget_deposit_restores_exhausted_tokens() {
        let budget = RetryBudget::new(2, 1);
        assert!(budget.try_withdraw());
        assert!(budget.try_withdraw());
        assert_eq!(budget.available(), 0);
        assert!(!budget.try_withdraw());
        budget.deposit();
        assert_eq!(budget.available(), 1);
        assert!(budget.try_withdraw());
    }

    #[test]
    fn budget_multiple_withdraws_until_exhaustion_consistent() {
        let budget = RetryBudget::new(5, 1);
        let mut count = 0;
        while budget.try_withdraw() {
            count += 1;
        }
        assert_eq!(count, 5);
        for _ in 0..10 {
            assert!(!budget.try_withdraw());
        }
    }

    // ────────────────────────────────────────────────────────
    // delay_for_attempt boundary
    // ────────────────────────────────────────────────────────

    #[test]
    fn delay_for_attempt_first_is_initial_backoff() {
        let cfg = RetryConfig::default();
        assert_eq!(cfg.delay_for_attempt(0), cfg.initial_backoff);
    }

    #[test]
    fn delay_for_attempt_at_max_backoff_boundary() {
        let cfg = RetryConfig::default().max_backoff(Duration::from_millis(500));
        // 100 * 2^3 = 800, capped at max_backoff 500
        assert_eq!(cfg.delay_for_attempt(3), Duration::from_millis(500));
    }

    #[test]
    fn delay_for_attempt_zero_multiplier_zero_delay() {
        let cfg = RetryConfig::default().backoff_multiplier(0.0);
        assert_eq!(cfg.delay_for_attempt(1), Duration::ZERO);
        assert_eq!(cfg.delay_for_attempt(10), Duration::ZERO);
    }

    // ────────────────────────────────────────────────────────
    // Idempotency
    // ────────────────────────────────────────────────────────

    #[test]
    fn post_not_idempotent() {
        assert!(!is_idempotent(&http::Method::POST));
    }

    #[test]
    fn patch_not_idempotent() {
        assert!(!is_idempotent(&http::Method::PATCH));
    }

    #[test]
    fn get_head_put_delete_options_trace_idempotent() {
        assert!(is_idempotent(&http::Method::GET));
        assert!(is_idempotent(&http::Method::HEAD));
        assert!(is_idempotent(&http::Method::PUT));
        assert!(is_idempotent(&http::Method::DELETE));
        assert!(is_idempotent(&http::Method::OPTIONS));
        assert!(is_idempotent(&http::Method::TRACE));
    }

    // ────────────────────────────────────────────────────────
    // RequestBody::Streaming try_clone
    // ────────────────────────────────────────────────────────

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn request_body_streaming_try_clone_returns_none() {
        use crate::body::RequestBody;
        use http_body_util::BodyExt;
        let streaming_body: crate::body::RequestBodySend = http_body_util::Empty::new()
            .map_err(|never| match never {})
            .boxed_unsync();
        let body = RequestBody::Streaming(streaming_body);
        assert!(body.try_clone().is_none());
    }
}
