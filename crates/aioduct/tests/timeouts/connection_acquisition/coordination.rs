use super::*;

#[tokio::test]
async fn stale_pooled_fallback_starts_a_fresh_connection_budget() {
    let phase_delay = Duration::from_millis(180);
    let (addr, server) = start_delayed_stale_h1_server(phase_delay).await;
    let connector = DelayAfterFirstConnector {
        inner: TcpConnector,
        attempts: Arc::new(AtomicUsize::new(0)),
        delay: phase_delay,
    };
    let client =
        HttpEngineSend::<TokioRuntime, DelayAfterFirstConnector>::builder_with_connector(connector)
            .connect_timeout(Duration::from_millis(300))
            .timeout(Duration::from_secs(2))
            .build()
            .unwrap();
    let url = format!("http://{addr}/");

    let warm = client.get(&url).unwrap().send().await.unwrap();
    assert_eq!(warm.text().await.unwrap(), "ok");

    let response = client.get(&url).unwrap().send().await.unwrap();
    assert_eq!(response.text().await.unwrap(), "fresh");
    server.await.unwrap();
}

#[tokio::test]
async fn h2_coordination_wait_uses_the_connection_deadline() {
    let connector = PendingConnector::default();
    let client =
        HttpEngineSend::<TokioRuntime, PendingConnector>::builder_with_connector(connector.clone())
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap();
    let url = "http://127.0.0.1:9/";

    let first_client = client.clone();
    let first = tokio::spawn(async move {
        first_client
            .get(url)
            .unwrap()
            .h2c_prior_knowledge()
            .connect_timeout(Duration::from_secs(3))
            .send()
            .await
    });
    tokio::time::timeout(Duration::from_millis(200), async {
        while connector.attempts() == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("first connection attempt did not start");

    let error = client
        .get(url)
        .unwrap()
        .h2c_prior_knowledge()
        .connect_timeout(Duration::from_millis(80))
        .send()
        .await
        .unwrap_err();

    first.abort();
    assert_connect_timeout(&error);
    assert_eq!(
        connector.attempts(),
        1,
        "waiter must not start a fresh dial"
    );
}
