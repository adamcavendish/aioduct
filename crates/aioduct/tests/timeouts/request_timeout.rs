use super::*;

#[tokio::test]
async fn test_request_timeout_triggers() {
    let (addr, _counter) = h1_server_with(|_req| async {
        tokio::time::sleep(Duration::from_secs(5)).await;
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("slow"))))
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();
    let result = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .timeout(Duration::from_millis(50))
        .send()
        .await;

    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.is_timeout(), "expected Timeout error, got: {err:?}");
}

#[tokio::test]
async fn test_request_timeout_completes_in_time() {
    let (addr, _counter) = h1_server().await;
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();

    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .timeout(Duration::from_secs(5))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    let body = resp.text().await.unwrap();
    assert_eq!(body, "hello aioduct");
}

#[tokio::test]
async fn test_client_default_timeout_triggers() {
    let (addr, _counter) = h1_server_with(|_req| async {
        tokio::time::sleep(Duration::from_secs(5)).await;
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("slow"))))
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .timeout(Duration::from_millis(50))
        .build()
        .unwrap();

    let result = client.get(&format!("http://{addr}/")).unwrap().send().await;

    assert!(result.is_err());
    assert!(result.unwrap_err().is_timeout());
}

#[tokio::test]
async fn test_request_timeout_overrides_client_timeout() {
    let (addr, _counter) = h1_server_with(|_req| async {
        tokio::time::sleep(Duration::from_millis(100)).await;
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("delayed"))))
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .timeout(Duration::from_millis(10))
        .build()
        .unwrap();

    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .timeout(Duration::from_secs(5))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    let body = resp.text().await.unwrap();
    assert_eq!(body, "delayed");
}
