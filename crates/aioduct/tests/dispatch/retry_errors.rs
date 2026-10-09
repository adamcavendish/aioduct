use super::*;

async fn failing_client() -> (aioduct::TokioClient, Arc<AtomicU32>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);
    let calls = Arc::new(AtomicU32::new(0));
    let resolutions = calls.clone();
    let client = aioduct::TokioClient::builder()
        .resolver(move |_: &str, _: u16| {
            resolutions.fetch_add(1, Ordering::SeqCst);
            Box::pin(async move { Ok(addr) })
                as std::pin::Pin<
                    Box<
                        dyn std::future::Future<Output = std::io::Result<std::net::SocketAddr>>
                            + Send,
                    >,
                >
        })
        .build()
        .unwrap();
    (client, calls)
}

#[tokio::test]
async fn retry_budget_exhaustion_on_connection_error() {
    let (client, resolutions) = failing_client().await;
    let result = client
        .get("http://retry.test/")
        .unwrap()
        .retry(
            aioduct::RetryConfig::default()
                .max_retries(5)
                .initial_backoff(Duration::ZERO)
                .budget(aioduct::RetryBudget::new(0, 0)),
        )
        .timeout(Duration::from_secs(2))
        .send()
        .await;
    assert!(result.is_err());
    assert_eq!(resolutions.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn retry_exhaustion_on_connection_error() {
    let (client, resolutions) = failing_client().await;
    let result = client
        .get("http://retry.test/")
        .unwrap()
        .retry(
            aioduct::RetryConfig::default()
                .max_retries(2)
                .initial_backoff(Duration::ZERO)
                .budget(aioduct::RetryBudget::new(100, 0)),
        )
        .timeout(Duration::from_secs(2))
        .send()
        .await;
    assert!(result.is_err());
    assert_eq!(resolutions.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn non_retryable_error_is_returned() {
    let client = aioduct::TokioClient::builder()
        .https_only(true)
        .build()
        .unwrap();
    let result = client
        .get("http://example.com/")
        .unwrap()
        .retry(
            aioduct::RetryConfig::default()
                .max_retries(3)
                .initial_backoff(Duration::ZERO),
        )
        .send()
        .await;
    assert!(matches!(
        result.unwrap_err().error(),
        aioduct::Error::HttpsOnly(_)
    ));
}
