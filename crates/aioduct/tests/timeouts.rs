#![cfg(feature = "tokio")]

mod common;
use common::*;

#[tokio::test]
async fn test_request_timeout_triggers() {
    let addr = start_server_with(|_req| async {
        tokio::time::sleep(Duration::from_secs(5)).await;
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("slow"))))
    })
    .await;

    let client = HttpEngine::<TokioRuntime, TcpConnector>::new(TcpConnector);
    let result = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .timeout(Duration::from_millis(50))
        .send()
        .await;

    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        matches!(err, aioduct::Error::Timeout),
        "expected Timeout error, got: {err:?}"
    );
}

#[tokio::test]
async fn test_request_timeout_completes_in_time() {
    let addr = start_server().await;
    let client = HttpEngine::<TokioRuntime, TcpConnector>::new(TcpConnector);

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
    let addr = start_server_with(|_req| async {
        tokio::time::sleep(Duration::from_secs(5)).await;
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("slow"))))
    })
    .await;

    let client = HttpEngine::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .timeout(Duration::from_millis(50))
        .build();

    let result = client.get(&format!("http://{addr}/")).unwrap().send().await;

    assert!(result.is_err());
    assert!(matches!(result.unwrap_err(), aioduct::Error::Timeout));
}

#[tokio::test]
async fn test_request_timeout_overrides_client_timeout() {
    let addr = start_server_with(|_req| async {
        tokio::time::sleep(Duration::from_millis(100)).await;
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("delayed"))))
    })
    .await;

    let client = HttpEngine::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .timeout(Duration::from_millis(10))
        .build();

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
#[tokio::test]
async fn test_read_timeout_does_not_apply_to_headers() {
    // Note: aioduct's read_timeout only applies to body reads, not header wait.
    // Use request timeout for header wait timeouts.
    let addr = start_server_with(|_req| async {
        tokio::time::sleep(Duration::from_millis(150)).await;
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("slow headers"))))
    })
    .await;

    let client = HttpEngine::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .read_timeout(Duration::from_millis(100))
        .build();

    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    let body = resp.text().await.unwrap();
    assert_eq!(body, "slow headers");
}

#[tokio::test]
async fn test_read_timeout_applies_to_body() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut buf = vec![0u8; 4096];
        let _ = stream.read(&mut buf).await;

        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\nhello")
            .await
            .unwrap();
        stream.flush().await.unwrap();

        tokio::time::sleep(Duration::from_millis(500)).await;
        let _ = stream.write_all(b"world").await;
    });

    let client = HttpEngine::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .read_timeout(Duration::from_millis(100))
        .build();

    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    let body_result = resp.text().await;
    assert!(
        body_result.is_err(),
        "read_timeout should fire on slow body chunks"
    );
}

#[tokio::test]
async fn test_read_timeout_allows_slow_but_steady_body() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut buf = vec![0u8; 4096];
        let _ = stream.read(&mut buf).await;

        stream
            .write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n")
            .await
            .unwrap();
        stream.flush().await.unwrap();

        for i in 0..3 {
            tokio::time::sleep(Duration::from_millis(50)).await;
            let chunk = format!("1\r\n{i}\r\n");
            stream.write_all(chunk.as_bytes()).await.unwrap();
            stream.flush().await.unwrap();
        }

        stream.write_all(b"0\r\n\r\n").await.unwrap();
        stream.flush().await.unwrap();
    });

    let client = HttpEngine::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .read_timeout(Duration::from_millis(200))
        .build();

    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    let body = resp.text().await.unwrap();
    assert_eq!(body, "012", "slow-but-within-threshold body should succeed");
}

#[tokio::test]
async fn test_content_length_preserved_through_timeout() {
    let addr = start_server_with(|_req| async {
        Ok::<_, Infallible>(
            Response::builder()
                .header("content-length", "5")
                .body(Full::new(Bytes::from("hello")))
                .unwrap(),
        )
    })
    .await;

    let client = HttpEngine::<TokioRuntime, TcpConnector>::new(TcpConnector);
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .timeout(Duration::from_secs(1))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.content_length(), Some(5));
}

#[tokio::test]
async fn test_connect_timeout() {
    let client = HttpEngine::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .connect_timeout(Duration::from_millis(100))
        .build();

    let start = tokio::time::Instant::now();
    let result = client
        .get("http://192.0.2.1:81/slow")
        .unwrap()
        .timeout(Duration::from_secs(5))
        .send()
        .await;

    assert!(result.is_err(), "connect_timeout should fire");
    assert!(
        start.elapsed() < Duration::from_secs(2),
        "should timeout quickly, not wait for request timeout"
    );
}
