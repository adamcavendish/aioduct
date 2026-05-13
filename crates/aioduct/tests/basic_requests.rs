#![cfg(feature = "tokio")]

mod common;
use common::*;

use http_body_util::BodyExt;

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

#[tokio::test]
async fn auto_headers_no_accept_by_default() {
    let addr = start_server_with(|req| async move {
        assert_eq!(req.method(), "GET");
        let accept = req
            .headers()
            .get("accept")
            .map(|v| v.to_str().unwrap().to_owned());
        let body = match accept {
            Some(v) => format!("accept={v}"),
            None => "accept=none".to_string(),
        };
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(body))))
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
    assert_eq!(body, "accept=none");
}

#[tokio::test]
async fn donot_set_content_length_0_if_have_no_body() {
    let addr = start_server_with(|req| async move {
        let headers = req.headers();
        assert!(
            headers.get("content-length").is_none(),
            "GET should not set content-length"
        );
        assert!(
            headers.get("content-type").is_none(),
            "GET should not set content-type"
        );
        assert!(
            headers.get("transfer-encoding").is_none(),
            "GET should not set transfer-encoding"
        );
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::new())))
    })
    .await;

    let client = HttpEngine::<TokioRuntime, TcpConnector>::new(TcpConnector);
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), http::StatusCode::OK);
}

#[tokio::test]
async fn custom_user_agent_via_builder() {
    let addr = start_server_with(|req| async move {
        let ua = req
            .headers()
            .get("user-agent")
            .map(|v| v.to_str().unwrap().to_owned())
            .unwrap_or_default();
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(ua))))
    })
    .await;

    let client = HttpEngine::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .user_agent("aioduct-test-agent")
        .build();

    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    let body = resp.text().await.unwrap();
    assert_eq!(body, "aioduct-test-agent");
}

#[tokio::test]
async fn response_text_and_content_length() {
    let addr = start_server_with(|_req| async {
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("Hello"))))
    })
    .await;

    let client = HttpEngine::<TokioRuntime, TcpConnector>::new(TcpConnector);
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.content_length(), Some(5));
    let text = resp.text().await.unwrap();
    assert_eq!(text, "Hello");
}

#[tokio::test]
async fn response_bytes_and_content_length() {
    let addr = start_server_with(|_req| async {
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("Hello"))))
    })
    .await;

    let client = HttpEngine::<TokioRuntime, TcpConnector>::new(TcpConnector);
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.content_length(), Some(5));
    let bytes = resp.bytes().await.unwrap();
    assert_eq!(&bytes[..], b"Hello");
}

#[cfg(feature = "json")]
#[tokio::test]
async fn response_json_string() {
    let addr = start_server_with(|_req| async {
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("\"Hello\""))))
    })
    .await;

    let client = HttpEngine::<TokioRuntime, TcpConnector>::new(TcpConnector);
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    let text: String = resp.json().await.unwrap();
    assert_eq!(text, "Hello");
}

#[cfg(feature = "json")]
#[tokio::test]
async fn json_content_type_default() {
    let addr = start_server_with(|req| async move {
        let ct = req
            .headers()
            .get("content-type")
            .map(|v| v.to_str().unwrap().to_owned())
            .unwrap_or_default();
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(ct))))
    })
    .await;

    let client = HttpEngine::<TokioRuntime, TcpConnector>::new(TcpConnector);
    let resp = client
        .post(&format!("http://{addr}/"))
        .unwrap()
        .json(&serde_json::json!({"body": "json"}))
        .unwrap()
        .send()
        .await
        .unwrap();

    let body = resp.text().await.unwrap();
    assert_eq!(body, "application/json");
}

#[cfg(feature = "json")]
#[tokio::test]
async fn json_content_type_not_overridden_if_set() {
    let addr = start_server_with(|req| async move {
        let ct = req
            .headers()
            .get("content-type")
            .map(|v| v.to_str().unwrap().to_owned())
            .unwrap_or_default();
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(ct))))
    })
    .await;

    let client = HttpEngine::<TokioRuntime, TcpConnector>::new(TcpConnector);
    let resp = client
        .post(&format!("http://{addr}/"))
        .unwrap()
        .header(
            http::header::CONTENT_TYPE,
            http::HeaderValue::from_static("application/vnd.api+json"),
        )
        .json(&serde_json::json!({"body": "json"}))
        .unwrap()
        .send()
        .await
        .unwrap();

    let body = resp.text().await.unwrap();
    assert_eq!(body, "application/vnd.api+json");
}

#[tokio::test]
async fn body_pipe_response_to_post() {
    let addr = start_server_with(|req| async move {
        if req.uri().path() == "/get" {
            Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("pipe me"))))
        } else {
            assert_eq!(req.uri().path(), "/pipe");
            let full = req.into_body().collect().await.unwrap().to_bytes();
            assert_eq!(&full[..], b"pipe me");
            Ok(Response::new(Full::new(Bytes::from("piped"))))
        }
    })
    .await;

    let client = HttpEngine::<TokioRuntime, TcpConnector>::new(TcpConnector);

    let res1 = client
        .get(&format!("http://{addr}/get"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(res1.status(), http::StatusCode::OK);
    assert_eq!(res1.content_length(), Some(7));

    let body_bytes = res1.bytes().await.unwrap();

    let res2 = client
        .post(&format!("http://{addr}/pipe"))
        .unwrap()
        .body(body_bytes.to_vec())
        .send()
        .await
        .unwrap();

    assert_eq!(res2.status(), http::StatusCode::OK);
    assert_eq!(res2.text().await.unwrap(), "piped");
}

#[tokio::test]
async fn raw_server_custom_response() {
    let addr = start_raw_server(|_req| async {
        b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\n\r\nraw".to_vec()
    })
    .await;

    let client = HttpEngine::<TokioRuntime, TcpConnector>::new(TcpConnector);
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    assert_eq!(resp.text().await.unwrap(), "raw");
}

#[tokio::test]
async fn text_part() {
    let form = aioduct::Multipart::new().text("foo", "bar");
    let expected_body = format!(
        "--{0}\r\nContent-Disposition: form-data; name=\"foo\"\r\n\r\nbar\r\n--{0}--\r\n",
        form.boundary()
    );
    let ct = form.content_type();

    let addr = start_server_with(move |req| {
        let ct = ct.clone();
        let expected_body = expected_body.clone();
        async move {
            assert_eq!(req.method(), "POST");
            assert_eq!(req.headers()["content-type"], ct);
            assert_eq!(
                req.headers()["content-length"],
                expected_body.len().to_string()
            );
            let full = req.into_body().collect().await.unwrap().to_bytes();
            assert_eq!(full, expected_body.as_bytes());
            Ok::<_, Infallible>(Response::new(Full::new(Bytes::new())))
        }
    })
    .await;

    let client = HttpEngine::<TokioRuntime, TcpConnector>::new(TcpConnector);
    let resp = client
        .post(&format!("http://{addr}/multipart/1"))
        .unwrap()
        .multipart(form)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), http::StatusCode::OK);
}

#[tokio::test]
async fn stream_part() {
    let stream_data = "part1 part2";
    let stream_body: aioduct::body::RequestBoxBody =
        http_body_util::Full::new(Bytes::from(stream_data))
            .map_err(|never| match never {})
            .boxed_unsync();

    let form = aioduct::Multipart::new()
        .text("foo", "bar")
        .part(aioduct::multipart::Part::stream("part_stream", stream_body));

    let expected_body = format!(
        "--{0}\r\nContent-Disposition: form-data; name=\"foo\"\r\n\r\nbar\r\n--{0}\r\nContent-Disposition: form-data; name=\"part_stream\"\r\n\r\n{1}\r\n--{0}--\r\n",
        form.boundary(),
        stream_data,
    );
    let ct = form.content_type();

    let addr = start_server_with(move |req| {
        let ct = ct.clone();
        let expected_body = expected_body.clone();
        async move {
            assert_eq!(req.method(), "POST");
            assert_eq!(req.headers()["content-type"], ct);
            assert_eq!(req.headers()["transfer-encoding"], "chunked");
            let full = req.into_body().collect().await.unwrap().to_bytes();
            assert_eq!(full, expected_body.as_bytes());
            Ok::<_, Infallible>(Response::new(Full::new(Bytes::new())))
        }
    })
    .await;

    let client = HttpEngine::<TokioRuntime, TcpConnector>::new(TcpConnector);
    let resp = client
        .post(&format!("http://{addr}/multipart/stream"))
        .unwrap()
        .multipart(form)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), http::StatusCode::OK);
}

#[tokio::test]
async fn file_part() {
    let file_contents = "file contents here";
    let form = aioduct::Multipart::new().file(
        "upload",
        "test.txt",
        "application/octet-stream",
        file_contents.as_bytes().to_vec(),
    );

    let expected_body = format!(
        "--{0}\r\nContent-Disposition: form-data; name=\"upload\"; filename=\"test.txt\"\r\nContent-Type: application/octet-stream\r\n\r\n{1}\r\n--{0}--\r\n",
        form.boundary(),
        file_contents,
    );
    let ct = form.content_type();

    let addr = start_server_with(move |req| {
        let ct = ct.clone();
        let expected_body = expected_body.clone();
        async move {
            assert_eq!(req.method(), "POST");
            assert_eq!(req.headers()["content-type"], ct);
            assert_eq!(
                req.headers()["content-length"],
                expected_body.len().to_string()
            );
            let full = req.into_body().collect().await.unwrap().to_bytes();
            assert_eq!(full, expected_body.as_bytes());
            Ok::<_, Infallible>(Response::new(Full::new(Bytes::new())))
        }
    })
    .await;

    let client = HttpEngine::<TokioRuntime, TcpConnector>::new(TcpConnector);
    let resp = client
        .post(&format!("http://{addr}/multipart/file"))
        .unwrap()
        .multipart(form)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), http::StatusCode::OK);
}

#[tokio::test]
async fn raw_server_chunked_response() {
    let addr = start_raw_server(|_req| async {
        b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n6\r\n world\r\n0\r\n\r\n"
            .to_vec()
    })
    .await;

    let client = HttpEngine::<TokioRuntime, TcpConnector>::new(TcpConnector);
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    assert_eq!(resp.text().await.unwrap(), "hello world");
}
