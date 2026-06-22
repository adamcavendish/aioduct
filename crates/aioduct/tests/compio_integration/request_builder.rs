use super::*;

#[test]
fn test_compio_query_params() {
    let addr = start_server_with_tokio(|req| async move {
        let query = req.uri().query().unwrap_or("none").to_owned();
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(query))))
    });

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::new();
        let resp = client
            .get_local(&format!("http://{addr}/"))
            .unwrap()
            .query(&[("key", "val"), ("a", "b")])
            .send()
            .await
            .unwrap();

        let body = resp.text().await.unwrap();
        assert!(body.contains("key=val"));
        assert!(body.contains("a=b"));
    });
}

#[cfg(feature = "json")]
#[test]
fn test_compio_json_body() {
    let addr = start_server_with_tokio(|req| async move {
        use http_body_util::BodyExt;
        let ct = req
            .headers()
            .get("content-type")
            .map(|v| v.to_str().unwrap_or("").to_owned())
            .unwrap_or_default();
        let body = req.collect().await.unwrap().to_bytes();
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(format!(
            "{}|{}",
            ct,
            String::from_utf8_lossy(&body)
        )))))
    });

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::new();
        let resp = client
            .post_local(&format!("http://{addr}/"))
            .unwrap()
            .json(&serde_json::json!({"key": "value"}))
            .unwrap()
            .send()
            .await
            .unwrap();

        let body = resp.text().await.unwrap();
        assert!(body.starts_with("application/json|"));
        assert!(body.contains("\"key\""));
        assert!(body.contains("\"value\""));
    });
}

#[test]
fn test_compio_form_body() {
    let addr = start_server_with_tokio(|req| async move {
        use http_body_util::BodyExt;
        let ct = req
            .headers()
            .get("content-type")
            .map(|v| v.to_str().unwrap_or("").to_owned())
            .unwrap_or_default();
        let body = req.collect().await.unwrap().to_bytes();
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(format!(
            "{}|{}",
            ct,
            String::from_utf8_lossy(&body)
        )))))
    });

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::new();
        let resp = client
            .post_local(&format!("http://{addr}/"))
            .unwrap()
            .form(&[("user", "alice"), ("pass", "secret")])
            .send()
            .await
            .unwrap();

        let body = resp.text().await.unwrap();
        assert!(body.starts_with("application/x-www-form-urlencoded|"));
        assert!(body.contains("user=alice"));
        assert!(body.contains("pass=secret"));
    });
}

#[test]
fn test_compio_basic_auth() {
    let addr = start_server_with_tokio(|req| async move {
        let auth = req
            .headers()
            .get("authorization")
            .map(|v| v.to_str().unwrap_or("").to_owned())
            .unwrap_or_default();
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(auth))))
    });

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::new();
        let resp = client
            .get_local(&format!("http://{addr}/"))
            .unwrap()
            .basic_auth("user", Some("pass"))
            .send()
            .await
            .unwrap();

        let body = resp.text().await.unwrap();
        assert!(body.starts_with("Basic "));
    });
}

#[test]
fn test_compio_try_clone() {
    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::new();
        let builder = client
            .get_local("http://example.com/")
            .unwrap()
            .header_str("x-test", "value")
            .unwrap()
            .body("payload");
        let cloned = builder.try_clone();
        assert!(cloned.is_some());
    });
}

#[test]
fn test_compio_headers_bulk() {
    let addr = start_server_with_tokio(|req| async move {
        let h1 = req
            .headers()
            .get("x-one")
            .map(|v| v.to_str().unwrap_or("").to_owned())
            .unwrap_or_default();
        let h2 = req
            .headers()
            .get("x-two")
            .map(|v| v.to_str().unwrap_or("").to_owned())
            .unwrap_or_default();
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(format!("{h1},{h2}")))))
    });

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::new();
        let mut headers = http::HeaderMap::new();
        headers.insert("x-one", "1".parse().unwrap());
        headers.insert("x-two", "2".parse().unwrap());
        let resp = client
            .get_local(&format!("http://{addr}/"))
            .unwrap()
            .headers(headers)
            .send()
            .await
            .unwrap();

        assert_eq!(resp.text().await.unwrap(), "1,2");
    });
}
