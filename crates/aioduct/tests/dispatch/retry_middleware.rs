use super::*;

// ── 101. Retry budget exhaustion on connection error with middleware ──────────

#[tokio::test]
async fn retry_budget_exhaustion_on_connection_error_with_middleware() {
    // Use a port that's definitely not listening (connection refused = retryable error)
    let dead_port = {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        port
    };

    let error_count = Arc::new(AtomicU32::new(0));
    let error_count_clone = error_count.clone();
    let retry_count = Arc::new(AtomicU32::new(0));
    let retry_count_clone = retry_count.clone();

    struct TrackingMiddleware {
        error_count: Arc<AtomicU32>,
        retry_count: Arc<AtomicU32>,
    }

    impl aioduct::Middleware for TrackingMiddleware {
        fn on_error(&self, _error: &aioduct::Error, _uri: &http::Uri, _method: &http::Method) {
            self.error_count.fetch_add(1, Ordering::SeqCst);
        }
        fn on_retry(
            &self,
            _error: &aioduct::Error,
            _uri: &http::Uri,
            _method: &http::Method,
            _attempt: u32,
        ) {
            self.retry_count.fetch_add(1, Ordering::SeqCst);
        }
    }

    // Budget of 0 tokens: first retry attempt will be denied
    let budget = aioduct::RetryBudget::new(0, 1);
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .middleware(TrackingMiddleware {
            error_count: error_count_clone,
            retry_count: retry_count_clone,
        })
        .build()
        .unwrap();

    let result = client
        .get(&format!("http://127.0.0.1:{dead_port}/"))
        .unwrap()
        .retry(
            aioduct::RetryConfig::default()
                .max_retries(5)
                .initial_backoff(Duration::from_millis(1))
                .budget(budget),
        )
        .timeout(Duration::from_secs(2))
        .send()
        .await;

    assert!(result.is_err(), "should fail when budget is exhausted");
    // Middleware on_error should have been called (budget exhaustion path)
    assert!(
        error_count.load(Ordering::SeqCst) >= 1,
        "on_error should be called when budget exhausted, got {}",
        error_count.load(Ordering::SeqCst)
    );
}

// ── 102. Retry fully exhausted with middleware (all attempts fail) ────────────

#[tokio::test]
async fn retry_fully_exhausted_with_middleware_fires_error() {
    let dead_port = {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        port
    };

    let error_count = Arc::new(AtomicU32::new(0));
    let error_count_clone = error_count.clone();
    let retry_count = Arc::new(AtomicU32::new(0));
    let retry_count_clone = retry_count.clone();

    struct ErrorTrackMw {
        error_count: Arc<AtomicU32>,
        retry_count: Arc<AtomicU32>,
    }

    impl aioduct::Middleware for ErrorTrackMw {
        fn on_error(&self, _error: &aioduct::Error, _uri: &http::Uri, _method: &http::Method) {
            self.error_count.fetch_add(1, Ordering::SeqCst);
        }
        fn on_retry(
            &self,
            _error: &aioduct::Error,
            _uri: &http::Uri,
            _method: &http::Method,
            _attempt: u32,
        ) {
            self.retry_count.fetch_add(1, Ordering::SeqCst);
        }
    }

    // Large budget so it never blocks
    let budget = aioduct::RetryBudget::new(100, 1);
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .middleware(ErrorTrackMw {
            error_count: error_count_clone,
            retry_count: retry_count_clone,
        })
        .build()
        .unwrap();

    let result = client
        .get(&format!("http://127.0.0.1:{dead_port}/"))
        .unwrap()
        .retry(
            aioduct::RetryConfig::default()
                .max_retries(2)
                .initial_backoff(Duration::from_millis(1))
                .budget(budget),
        )
        .timeout(Duration::from_secs(5))
        .send()
        .await;

    assert!(result.is_err(), "all retries should be exhausted");
    // on_retry should have been called for each retry attempt
    assert_eq!(
        retry_count.load(Ordering::SeqCst),
        2,
        "on_retry should be called for each retry attempt"
    );
    // on_error should be called once at the end when retries are exhausted
    assert_eq!(
        error_count.load(Ordering::SeqCst),
        1,
        "on_error should be called once when retries exhausted"
    );
}

// ── 103. Non-retryable error with middleware fires on_error immediately ───────

#[tokio::test]
async fn non_retryable_error_with_middleware() {
    let error_count = Arc::new(AtomicU32::new(0));
    let error_count_clone = error_count.clone();

    struct NonRetryErrorMw {
        error_count: Arc<AtomicU32>,
    }

    impl aioduct::Middleware for NonRetryErrorMw {
        fn on_error(&self, _error: &aioduct::Error, _uri: &http::Uri, _method: &http::Method) {
            self.error_count.fetch_add(1, Ordering::SeqCst);
        }
    }

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .middleware(NonRetryErrorMw {
            error_count: error_count_clone,
        })
        .https_only(true)
        .build()
        .unwrap();

    // Sending to http:// with https_only triggers a non-retryable error
    let result = client
        .get("http://example.com/")
        .unwrap()
        .retry(
            aioduct::RetryConfig::default()
                .max_retries(3)
                .initial_backoff(Duration::from_millis(1)),
        )
        .send()
        .await;

    assert!(result.is_err());
    // on_error should be called for non-retryable errors
    assert_eq!(
        error_count.load(Ordering::SeqCst),
        1,
        "on_error should fire for non-retryable errors"
    );
}
