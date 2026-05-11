#![cfg(feature = "tokio")]

mod common;
use common::*;

#[tokio::test]
async fn test_get_request() {
    let addr = start_server().await;
    let client = HttpEngine::<TokioRuntime, TcpConnector>::new(TcpConnector);

    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    let body = resp.text().await.unwrap();
    assert_eq!(body, "hello aioduct");
}

#[tokio::test]
async fn test_post_request() {
    let addr = start_server().await;
    let client = HttpEngine::<TokioRuntime, TcpConnector>::new(TcpConnector);

    let resp = client
        .post(&format!("http://{addr}/"))
        .unwrap()
        .body("request body")
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
}

#[tokio::test]
async fn test_connection_reuse() {
    let addr = start_server().await;
    let client = HttpEngine::<TokioRuntime, TcpConnector>::new(TcpConnector);
    let url = format!("http://{addr}/");

    let resp1 = client.get(&url).unwrap().send().await.unwrap();
    assert_eq!(resp1.status(), http::StatusCode::OK);
    let _ = resp1.text().await.unwrap();

    let resp2 = client.get(&url).unwrap().send().await.unwrap();
    assert_eq!(resp2.status(), http::StatusCode::OK);
    let body = resp2.text().await.unwrap();
    assert_eq!(body, "hello aioduct");
}

#[tokio::test]
async fn test_host_header_and_path() {
    let addr = start_server_with(echo_headers).await;
    let client = HttpEngine::<TokioRuntime, TcpConnector>::new(TcpConnector);

    let resp = client
        .get(&format!("http://{addr}/some/path?key=value"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    let body = resp.text().await.unwrap();
    assert!(
        body.contains(&format!("host={addr}")),
        "expected Host header to be set, got: {body}"
    );
    assert!(
        body.contains("path=/some/path"),
        "expected path-only URI, got: {body}"
    );
}

#[tokio::test]
async fn test_custom_header() {
    let addr = start_server_with(|req| async move {
        let custom = req
            .headers()
            .get("x-custom")
            .map(|v| v.to_str().unwrap_or(""))
            .unwrap_or("missing");
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(custom.to_string()))))
    })
    .await;
    let client = HttpEngine::<TokioRuntime, TcpConnector>::new(TcpConnector);

    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .header_str("x-custom", "test-value")
        .unwrap()
        .send()
        .await
        .unwrap();

    let body = resp.text().await.unwrap();
    assert_eq!(body, "test-value");
}

#[tokio::test]
async fn test_invalid_url() {
    let client = HttpEngine::<TokioRuntime, TcpConnector>::new(TcpConnector);
    assert!(client.get("not a url").is_err());
}

#[tokio::test]
async fn test_missing_scheme() {
    let client = HttpEngine::<TokioRuntime, TcpConnector>::new(TcpConnector);
    assert!(client.get("127.0.0.1/path").is_err());
}
#[tokio::test]
async fn test_query_params() {
    let addr = start_server_with(|req| async move {
        let query = req.uri().query().unwrap_or("none").to_owned();
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(query))))
    })
    .await;

    let client = HttpEngine::<TokioRuntime, TcpConnector>::new(TcpConnector);
    let resp = client
        .get(&format!("http://{addr}/search"))
        .unwrap()
        .query(&[("q", "hello world"), ("page", "1")])
        .send()
        .await
        .unwrap();

    let body = resp.text().await.unwrap();
    assert_eq!(body, "q=hello%20world&page=1");
}

#[tokio::test]
async fn test_default_user_agent() {
    let addr = start_server_with(|req| async move {
        let ua = req
            .headers()
            .get("user-agent")
            .map(|v| v.to_str().unwrap_or("").to_owned())
            .unwrap_or_default();
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(ua))))
    })
    .await;

    let client = HttpEngine::<TokioRuntime, TcpConnector>::new(TcpConnector);
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    let body = resp.text().await.unwrap();
    assert!(
        body.starts_with("aioduct/"),
        "expected default User-Agent, got: {body}"
    );
}

#[tokio::test]
async fn test_custom_default_headers() {
    let addr = start_server_with(|req| async move {
        let custom = req
            .headers()
            .get("x-default")
            .map(|v| v.to_str().unwrap_or("").to_owned())
            .unwrap_or_default();
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(custom))))
    })
    .await;

    let mut headers = http::HeaderMap::new();
    headers.insert("x-default", "from-client".parse().unwrap());
    let client = HttpEngine::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .default_headers(headers)
        .build();

    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    let body = resp.text().await.unwrap();
    assert_eq!(body, "from-client");
}

#[tokio::test]
async fn test_request_headers_override_defaults() {
    let addr = start_server_with(|req| async move {
        let ua = req
            .headers()
            .get("user-agent")
            .map(|v| v.to_str().unwrap_or("").to_owned())
            .unwrap_or_default();
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(ua))))
    })
    .await;

    let client = HttpEngine::<TokioRuntime, TcpConnector>::new(TcpConnector);
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .header_str("user-agent", "custom-agent/1.0")
        .unwrap()
        .send()
        .await
        .unwrap();

    let body = resp.text().await.unwrap();
    assert_eq!(body, "custom-agent/1.0");
}

#[tokio::test]
async fn test_put_request() {
    use http_body_util::BodyExt;

    let addr = start_server_with(|req| async move {
        let method = req.method().to_string();
        let body = req.into_body().collect().await.unwrap().to_bytes();
        let resp_body = format!("method={method} body={}", String::from_utf8_lossy(&body));
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(resp_body))))
    })
    .await;

    let client = HttpEngine::<TokioRuntime, TcpConnector>::new(TcpConnector);
    let resp = client
        .put(&format!("http://{addr}/"))
        .unwrap()
        .body("update data")
        .send()
        .await
        .unwrap();

    let body = resp.text().await.unwrap();
    assert!(body.contains("method=PUT"), "expected PUT, got: {body}");
    assert!(
        body.contains("body=update data"),
        "expected body, got: {body}"
    );
}

#[tokio::test]
async fn test_patch_request() {
    let addr = start_server_with(|req| async move {
        let method = req.method().to_string();
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(method))))
    })
    .await;

    let client = HttpEngine::<TokioRuntime, TcpConnector>::new(TcpConnector);
    let resp = client
        .patch(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.text().await.unwrap(), "PATCH");
}

#[tokio::test]
async fn test_delete_request() {
    let addr = start_server_with(|req| async move {
        let method = req.method().to_string();
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(method))))
    })
    .await;

    let client = HttpEngine::<TokioRuntime, TcpConnector>::new(TcpConnector);
    let resp = client
        .delete(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.text().await.unwrap(), "DELETE");
}

#[tokio::test]
async fn test_head_request() {
    let addr = start_server_with(|req| async move {
        let method = req.method().to_string();
        Ok::<_, Infallible>(
            Response::builder()
                .header("x-method", method)
                .header("content-length", "1000")
                .body(Full::new(Bytes::new()))
                .unwrap(),
        )
    })
    .await;

    let client = HttpEngine::<TokioRuntime, TcpConnector>::new(TcpConnector);
    let resp = client
        .head(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    assert_eq!(
        resp.headers().get("x-method").unwrap().to_str().unwrap(),
        "HEAD"
    );
    assert_eq!(resp.content_length(), Some(1000));
}

#[tokio::test]
async fn test_query_params_with_existing_query() {
    let addr = start_server_with(|req| async move {
        let query = req.uri().query().unwrap_or("").to_string();
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(query))))
    })
    .await;

    let client = HttpEngine::<TokioRuntime, TcpConnector>::new(TcpConnector);
    let resp = client
        .get(&format!("http://{addr}/?existing=1"))
        .unwrap()
        .query(&[("extra", "2")])
        .send()
        .await
        .unwrap();

    let body = resp.text().await.unwrap();
    assert!(
        body.contains("existing=1"),
        "expected existing, got: {body}"
    );
    assert!(body.contains("extra=2"), "expected extra, got: {body}");
}

#[tokio::test]
async fn test_no_default_headers() {
    let addr = start_server_with(|req| async move {
        let ua = req
            .headers()
            .get("user-agent")
            .map(|v| v.to_str().unwrap_or("").to_owned())
            .unwrap_or_else(|| "none".to_owned());
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(ua))))
    })
    .await;

    let client = HttpEngine::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .no_default_headers()
        .build();

    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    let body = resp.text().await.unwrap();
    assert_eq!(body, "none");
}

#[tokio::test]
async fn test_custom_method() {
    let addr = start_server_with(|req| async move {
        let method = req.method().to_string();
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(method))))
    })
    .await;

    let client = HttpEngine::<TokioRuntime, TcpConnector>::new(TcpConnector);
    let resp = client
        .request(http::Method::OPTIONS, &format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.text().await.unwrap(), "OPTIONS");
}

#[tokio::test]
async fn test_multiple_headers_same_name() {
    let addr = start_server_with(|req| async move {
        let values: Vec<String> = req
            .headers()
            .get_all("x-multi")
            .iter()
            .map(|v| v.to_str().unwrap().to_string())
            .collect();
        let body = values.join(",");
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(body))))
    })
    .await;

    let client = HttpEngine::<TokioRuntime, TcpConnector>::new(TcpConnector);
    let mut headers = http::HeaderMap::new();
    headers.append("x-multi", "value1".parse().unwrap());
    headers.append("x-multi", "value2".parse().unwrap());

    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .headers(headers)
        .send()
        .await
        .unwrap();

    let body = resp.text().await.unwrap();
    assert!(body.contains("value1"), "expected value1, got: {body}");
    assert!(body.contains("value2"), "expected value2, got: {body}");
}
