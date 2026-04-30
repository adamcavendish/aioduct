#![cfg(feature = "tokio")]

mod common;
use common::*;

#[cfg(feature = "json")]
#[tokio::test]
async fn test_json_request_and_response() {
    use http_body_util::BodyExt;
    use serde::{Deserialize, Serialize};

    #[derive(Serialize, Deserialize, Debug, PartialEq)]
    struct Payload {
        name: String,
        value: u32,
    }

    let addr = start_server_with(|req| async move {
        let content_type = req
            .headers()
            .get("content-type")
            .map(|v| v.to_str().unwrap_or("missing").to_owned())
            .unwrap_or_else(|| "missing".to_owned());

        let body = req.into_body().collect().await.unwrap().to_bytes();
        let payload: Payload = serde_json::from_slice(&body).unwrap();

        let resp_body = serde_json::to_string(&Payload {
            name: payload.name.to_uppercase(),
            value: payload.value + 1,
        })
        .unwrap();

        Ok::<_, Infallible>(
            Response::builder()
                .header("content-type", content_type)
                .body(Full::new(Bytes::from(resp_body)))
                .unwrap(),
        )
    })
    .await;

    let client = Client::<TokioRuntime>::new();
    let input = Payload {
        name: "test".into(),
        value: 42,
    };

    let resp = client
        .post(&format!("http://{addr}/"))
        .unwrap()
        .json(&input)
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    assert_eq!(
        resp.headers().get("content-type").unwrap(),
        "application/json"
    );
    let output: Payload = resp.json().await.unwrap();
    assert_eq!(
        output,
        Payload {
            name: "TEST".into(),
            value: 43
        }
    );
}
#[tokio::test]
async fn test_form_data() {
    use http_body_util::BodyExt;

    let addr = start_server_with(|req| async move {
        let ct = req
            .headers()
            .get("content-type")
            .map(|v| v.to_str().unwrap_or("").to_owned())
            .unwrap_or_default();
        let body = req.into_body().collect().await.unwrap().to_bytes();
        let body_str = String::from_utf8_lossy(&body).to_string();
        let resp_body = format!("ct={ct}\nbody={body_str}");
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(resp_body))))
    })
    .await;

    let client = Client::<TokioRuntime>::new();
    let resp = client
        .post(&format!("http://{addr}/"))
        .unwrap()
        .form(&[("username", "admin"), ("password", "s3cr&t=val")])
        .send()
        .await
        .unwrap();

    let body = resp.text().await.unwrap();
    assert!(
        body.contains("ct=application/x-www-form-urlencoded"),
        "expected form content-type, got: {body}"
    );
    assert!(
        body.contains("username=admin"),
        "expected username param, got: {body}"
    );
    assert!(
        body.contains("password=s3cr%26t%3Dval"),
        "expected encoded password, got: {body}"
    );
}
#[tokio::test]
async fn test_sse_stream() {
    let addr = start_server_with(|_req| async move {
        let sse_body =
            "event: greeting\ndata: hello\n\ndata: world\n\nevent: done\ndata: bye\nid: 3\n\n";
        Ok::<_, Infallible>(
            Response::builder()
                .header("content-type", "text/event-stream")
                .body(Full::new(Bytes::from(sse_body)))
                .unwrap(),
        )
    })
    .await;

    let client = Client::<TokioRuntime>::new();
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    let mut sse = resp.into_sse_stream();
    let mut events = Vec::new();
    while let Some(event) = sse.next().await {
        events.push(event.unwrap());
    }

    assert_eq!(events.len(), 3);
    assert_eq!(events[0].event.as_deref(), Some("greeting"));
    assert_eq!(events[0].data, "hello");
    assert_eq!(events[1].event, None);
    assert_eq!(events[1].data, "world");
    assert_eq!(events[2].event.as_deref(), Some("done"));
    assert_eq!(events[2].data, "bye");
    assert_eq!(events[2].id.as_deref(), Some("3"));
}
#[tokio::test]
async fn test_sse_multiline_data() {
    let addr = start_server_with(|_req| async move {
        let sse_body = "data: line1\ndata: line2\ndata: line3\n\n";
        Ok::<_, Infallible>(
            Response::builder()
                .header("content-type", "text/event-stream")
                .body(Full::new(Bytes::from(sse_body)))
                .unwrap(),
        )
    })
    .await;

    let client = Client::<TokioRuntime>::new();
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    let mut sse = resp.into_sse_stream();
    let event = sse.next().await.unwrap().unwrap();
    assert_eq!(event.data, "line1\nline2\nline3");
    assert!(sse.next().await.is_none());
}
#[tokio::test]
async fn test_sse_comments_and_retry() {
    let addr = start_server_with(|_req| async move {
        let sse_body = ": this is a comment\nretry: 5000\ndata: after comment\n\n";
        Ok::<_, Infallible>(
            Response::builder()
                .header("content-type", "text/event-stream")
                .body(Full::new(Bytes::from(sse_body)))
                .unwrap(),
        )
    })
    .await;

    let client = Client::<TokioRuntime>::new();
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    let mut sse = resp.into_sse_stream();
    let event = sse.next().await.unwrap().unwrap();
    assert_eq!(event.data, "after comment");
    assert_eq!(event.retry, Some(5000));
    assert!(sse.next().await.is_none());
}
#[tokio::test]
async fn test_multipart_text_fields() {
    use http_body_util::BodyExt;

    let addr = start_server_with(|req| async move {
        let ct = req
            .headers()
            .get("content-type")
            .map(|v| v.to_str().unwrap_or("").to_owned())
            .unwrap_or_default();
        let body = req.into_body().collect().await.unwrap().to_bytes();
        let body_str = String::from_utf8_lossy(&body).to_string();
        let resp_body = format!("ct={ct}\nbody={body_str}");
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(resp_body))))
    })
    .await;

    let client = Client::<TokioRuntime>::new();
    let form = aioduct::Multipart::new()
        .text("field1", "value1")
        .text("field2", "value2");

    let resp = client
        .post(&format!("http://{addr}/"))
        .unwrap()
        .multipart(form)
        .send()
        .await
        .unwrap();

    let body = resp.text().await.unwrap();
    assert!(
        body.contains("multipart/form-data; boundary="),
        "expected multipart content-type, got: {body}"
    );
    assert!(
        body.contains("name=\"field1\""),
        "expected field1, got: {body}"
    );
    assert!(body.contains("value1"), "expected value1, got: {body}");
    assert!(
        body.contains("name=\"field2\""),
        "expected field2, got: {body}"
    );
    assert!(body.contains("value2"), "expected value2, got: {body}");
}
#[tokio::test]
async fn test_multipart_file_upload() {
    use http_body_util::BodyExt;

    let addr = start_server_with(|req| async move {
        let body = req.into_body().collect().await.unwrap().to_bytes();
        let body_str = String::from_utf8_lossy(&body).to_string();
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(body_str))))
    })
    .await;

    let client = Client::<TokioRuntime>::new();
    let form = aioduct::Multipart::new()
        .text("description", "test upload")
        .file("file", "hello.txt", "text/plain", "file contents here");

    let resp = client
        .post(&format!("http://{addr}/"))
        .unwrap()
        .multipart(form)
        .send()
        .await
        .unwrap();

    let body = resp.text().await.unwrap();
    assert!(
        body.contains("filename=\"hello.txt\""),
        "expected filename, got: {body}"
    );
    assert!(
        body.contains("Content-Type: text/plain"),
        "expected file content-type, got: {body}"
    );
    assert!(
        body.contains("file contents here"),
        "expected file data, got: {body}"
    );
    assert!(
        body.contains("name=\"description\""),
        "expected description field, got: {body}"
    );
}
#[tokio::test]
async fn test_bytes_stream() {
    let addr = start_server_with(|_req| async move {
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("chunk1chunk2chunk3"))))
    })
    .await;

    let client = Client::<TokioRuntime>::new();
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    let mut stream = resp.into_bytes_stream();
    let mut collected = Vec::new();
    while let Some(chunk) = stream.next().await {
        collected.extend_from_slice(&chunk.unwrap());
    }

    assert_eq!(String::from_utf8(collected).unwrap(), "chunk1chunk2chunk3");
}
#[tokio::test]
async fn test_bytes_stream_empty() {
    let addr = start_server_with(|_req| async move {
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::new())))
    })
    .await;

    let client = Client::<TokioRuntime>::new();
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    let mut stream = resp.into_bytes_stream();
    assert!(stream.next().await.is_none());
}
#[tokio::test]
async fn test_streaming_body_upload() {
    use http_body_util::BodyExt;

    let addr = start_server_with(|req| async move {
        let body = req.into_body().collect().await.unwrap().to_bytes();
        Ok::<_, Infallible>(Response::new(Full::new(body)))
    })
    .await;

    let chunks: Vec<Result<hyper::body::Frame<Bytes>, aioduct::Error>> = vec![
        Ok(hyper::body::Frame::data(Bytes::from("hello "))),
        Ok(hyper::body::Frame::data(Bytes::from("streaming "))),
        Ok(hyper::body::Frame::data(Bytes::from("world"))),
    ];

    let stream = futures_util::stream::iter(chunks);
    let stream_body: aioduct::AioductBody = http_body_util::StreamBody::new(stream).boxed();

    let client = Client::<TokioRuntime>::new();
    let resp = client
        .post(&format!("http://{addr}/"))
        .unwrap()
        .body_stream(stream_body)
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    let body = resp.text().await.unwrap();
    assert_eq!(body, "hello streaming world");
}
#[tokio::test]
async fn test_streaming_body_from_request_body() {
    use http_body_util::BodyExt;

    let addr = start_server_with(|req| async move {
        let body = req.into_body().collect().await.unwrap().to_bytes();
        Ok::<_, Infallible>(Response::new(Full::new(body)))
    })
    .await;

    let data = "buffered body content";
    let client = Client::<TokioRuntime>::new();
    let resp = client
        .post(&format!("http://{addr}/"))
        .unwrap()
        .body(data)
        .send()
        .await
        .unwrap();

    assert_eq!(resp.text().await.unwrap(), "buffered body content");
}
#[tokio::test]
async fn test_chunk_download() {
    let data = "abcdefghijklmnopqrstuvwxyz0123456789";

    let addr = start_server_with(move |req| async move {
        let total = data.len();
        if req.method() == http::Method::HEAD {
            return Ok::<_, Infallible>(
                Response::builder()
                    .header("content-length", total.to_string())
                    .header("accept-ranges", "bytes")
                    .body(Full::new(Bytes::new()))
                    .unwrap(),
            );
        }

        if let Some(range) = req.headers().get("range") {
            let range_str = range.to_str().unwrap();
            let range_str = range_str.trim_start_matches("bytes=");
            let parts: Vec<&str> = range_str.split('-').collect();
            let start: usize = parts[0].parse().unwrap();
            let end: usize = parts[1].parse().unwrap();
            let slice = &data[start..=end];
            return Ok(Response::builder()
                .status(206)
                .header("content-range", format!("bytes {start}-{end}/{total}"))
                .body(Full::new(Bytes::from(slice.to_owned())))
                .unwrap());
        }

        Ok(Response::new(Full::new(Bytes::from(data))))
    })
    .await;

    let client = Client::<TokioRuntime>::new();
    let result = client
        .chunk_download(&format!("http://{addr}/"))
        .chunks(4)
        .download()
        .await
        .unwrap();

    assert_eq!(result.total_size, 36);
    assert_eq!(
        String::from_utf8(result.data.to_vec()).unwrap(),
        "abcdefghijklmnopqrstuvwxyz0123456789"
    );
}
#[tokio::test]
async fn test_chunk_download_fallback_no_range() {
    let addr = start_server_with(|_req| async move {
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("no range support"))))
    })
    .await;

    let client = Client::<TokioRuntime>::new();
    let result = client
        .chunk_download(&format!("http://{addr}/"))
        .chunks(4)
        .download()
        .await
        .unwrap();

    assert_eq!(
        String::from_utf8(result.data.to_vec()).unwrap(),
        "no range support"
    );
}
#[tokio::test]
async fn test_large_body() {
    let data = "x".repeat(100_000);
    let data_clone = data.clone();

    let addr = start_server_with(move |_req| {
        let data = data_clone.clone();
        async move { Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(data)))) }
    })
    .await;

    let client = Client::<TokioRuntime>::new();
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    let body = resp.text().await.unwrap();
    assert_eq!(body.len(), 100_000);
}
#[tokio::test]
async fn test_empty_body_response() {
    let addr = start_server_with(|_req| async move {
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::new())))
    })
    .await;

    let client = Client::<TokioRuntime>::new();
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    let body = resp.text().await.unwrap();
    assert_eq!(body, "");
}
#[tokio::test]
async fn test_chunk_download_head_fails() {
    let addr = start_server_with(|req| async move {
        if req.method() == http::Method::HEAD {
            Ok::<_, Infallible>(
                Response::builder()
                    .status(404)
                    .body(Full::new(Bytes::from("not found")))
                    .unwrap(),
            )
        } else {
            Ok(Response::new(Full::new(Bytes::from("full body data"))))
        }
    })
    .await;

    let client = Client::<TokioRuntime>::new();
    let result = client
        .chunk_download(&format!("http://{addr}/file"))
        .download()
        .await;
    assert!(result.is_err());
}
#[tokio::test]
async fn test_chunk_download_fallback_to_get() {
    let addr = start_server_with(|req| async move {
        if req.method() == http::Method::HEAD {
            Ok::<_, Infallible>(
                Response::builder()
                    .header("content-length", "100")
                    .body(Full::new(Bytes::new()))
                    .unwrap(),
            )
        } else {
            Ok(Response::new(Full::new(Bytes::from("fallback data"))))
        }
    })
    .await;

    let client = Client::<TokioRuntime>::new();
    let result = client
        .chunk_download(&format!("http://{addr}/file"))
        .download()
        .await
        .unwrap();
    assert_eq!(result.data, Bytes::from("fallback data"));
}
#[tokio::test]
async fn test_chunk_download_debug() {
    let client = Client::<TokioRuntime>::new();
    let dl = client.chunk_download("http://example.com/file.bin");
    let dbg = format!("{dl:?}");
    assert!(dbg.contains("ChunkDownload"));
    assert!(dbg.contains("example.com"));
}
#[tokio::test]
async fn test_chunk_download_with_custom_chunks() {
    let data = "x".repeat(10000);
    let data_clone = data.clone();

    let addr = start_server_with(move |req| {
        let data = data_clone.clone();
        async move {
            if req.method() == http::Method::HEAD {
                Ok::<_, Infallible>(
                    Response::builder()
                        .header("accept-ranges", "bytes")
                        .header("content-length", data.len().to_string())
                        .body(Full::new(Bytes::new()))
                        .unwrap(),
                )
            } else if let Some(range) = req.headers().get("range") {
                let range_str = range.to_str().unwrap();
                let range_str = range_str.strip_prefix("bytes=").unwrap();
                let parts: Vec<&str> = range_str.split('-').collect();
                let start: usize = parts[0].parse().unwrap();
                let end: usize = parts[1].parse().unwrap();
                let chunk = &data.as_bytes()[start..=end];
                Ok(Response::builder()
                    .status(206)
                    .body(Full::new(Bytes::copy_from_slice(chunk)))
                    .unwrap())
            } else {
                Ok(Response::new(Full::new(Bytes::from(data.clone()))))
            }
        }
    })
    .await;

    let client = Client::<TokioRuntime>::new();
    let result = client
        .chunk_download(&format!("http://{addr}/file"))
        .chunks(2)
        .download()
        .await
        .unwrap();

    assert_eq!(result.total_size, 10000);
    assert_eq!(result.data.len(), 10000);
    assert_eq!(result.data, Bytes::from(data));
}
#[tokio::test]
async fn test_chunk_download_range_request_fails() {
    let addr = start_server_with(move |req| async move {
        if req.method() == http::Method::HEAD {
            Ok::<_, Infallible>(
                Response::builder()
                    .header("accept-ranges", "bytes")
                    .header("content-length", "100")
                    .body(Full::new(Bytes::new()))
                    .unwrap(),
            )
        } else {
            Ok(Response::builder()
                .status(500)
                .body(Full::new(Bytes::from("server error")))
                .unwrap())
        }
    })
    .await;

    let client = Client::<TokioRuntime>::new();
    let result = client
        .chunk_download(&format!("http://{addr}/file"))
        .chunks(2)
        .download()
        .await;

    assert!(result.is_err());
}
