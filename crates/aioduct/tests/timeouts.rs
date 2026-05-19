#![cfg(feature = "tokio")]

use std::convert::Infallible;
use std::time::Duration;

use bytes::Bytes;
use http_body_util::Full;
use hyper::Response;

use aioduct::HttpEngineSend;
use aioduct::runtime::TokioRuntime;
use aioduct::runtime::tokio_rt::TcpConnector;

use aioduct_test_server::h1::{h1_server, h1_server_with};

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
#[tokio::test]
async fn test_read_timeout_does_not_apply_to_headers() {
    // Note: aioduct's read_timeout only applies to body reads, not header wait.
    // Use request timeout for header wait timeouts.
    let (addr, _counter) = h1_server_with(|_req| async {
        tokio::time::sleep(Duration::from_millis(150)).await;
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("slow headers"))))
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .read_timeout(Duration::from_millis(100))
        .build()
        .unwrap();

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

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .read_timeout(Duration::from_millis(100))
        .build()
        .unwrap();

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

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .read_timeout(Duration::from_millis(200))
        .build()
        .unwrap();

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
    let (addr, _counter) = h1_server_with(|_req| async {
        Ok::<_, Infallible>(
            Response::builder()
                .header("content-length", "5")
                .body(Full::new(Bytes::from("hello")))
                .unwrap(),
        )
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();
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
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .connect_timeout(Duration::from_millis(100))
        .build()
        .unwrap();

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

#[tokio::test]
async fn client_timeout_triggers_on_slow_response() {
    let (addr, _counter) = h1_server_with(|_req| async {
        tokio::time::sleep(Duration::from_millis(300)).await;
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("slow"))))
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .timeout(Duration::from_millis(100))
        .build()
        .unwrap();

    let result = client.get(&format!("http://{addr}/")).unwrap().send().await;

    let err = result.unwrap_err();
    assert!(err.is_timeout(), "expected timeout, got: {err:?}");
}

#[tokio::test]
async fn per_request_timeout_triggers_on_slow_response() {
    let (addr, _counter) = h1_server_with(|_req| async {
        tokio::time::sleep(Duration::from_millis(300)).await;
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("slow"))))
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();

    let result = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .timeout(Duration::from_millis(100))
        .send()
        .await;

    let err = result.unwrap_err();
    assert!(err.is_timeout(), "expected timeout, got: {err:?}");
}

#[tokio::test]
async fn connect_timeout_with_unreachable_ip() {
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .connect_timeout(Duration::from_millis(100))
        .build()
        .unwrap();

    let result = client
        .get("http://192.0.2.1:81/slow")
        .unwrap()
        .timeout(Duration::from_secs(5))
        .send()
        .await;

    let err = result.unwrap_err();
    assert!(
        err.is_timeout() || err.is_connect(),
        "expected timeout or connect error, got: {err:?}"
    );
}

#[tokio::test]
async fn read_timeout_does_not_apply_to_headers() {
    // Unlike reqwest, aioduct's read_timeout only applies to body reads.
    // Use request timeout for header wait timeouts.
    let (addr, _counter) = h1_server_with(|_req| async {
        tokio::time::sleep(Duration::from_millis(200)).await;
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("slow headers"))))
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .read_timeout(Duration::from_millis(100))
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();

    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    assert_eq!(resp.text().await.unwrap(), "slow headers");
}

#[tokio::test]
async fn request_timeout_overrides_client_timeout() {
    let (addr, _counter) = h1_server_with(|_req| async {
        tokio::time::sleep(Duration::from_millis(150)).await;
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("delayed"))))
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .timeout(Duration::from_millis(50))
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
    assert_eq!(resp.text().await.unwrap(), "delayed");
}

#[tokio::test]
async fn timeout_fast_response_succeeds() {
    let (addr, _counter) = h1_server().await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();

    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    assert_eq!(resp.content_length(), Some(13));
    let text = resp.text().await.unwrap();
    assert_eq!(text, "hello aioduct");
}

#[tokio::test]
async fn connect_timeout_does_not_affect_fast_connects() {
    let (addr, _counter) = h1_server().await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .connect_timeout(Duration::from_secs(5))
        .build()
        .unwrap();

    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
}
