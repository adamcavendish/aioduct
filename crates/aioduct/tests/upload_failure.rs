#![cfg(feature = "tokio")]

use std::convert::Infallible;
use std::time::Duration;

use bytes::Bytes;
use http_body_util::Full;
use hyper::Response;

use aioduct::HttpEngineSend;
use aioduct::runtime::TokioRuntime;
use aioduct::runtime::tokio_rt::TcpConnector;

use aioduct_test_server::h1::h1_server_with;

use http_body_util::BodyExt;

use tokio::io::AsyncReadExt;
use tokio::net::TcpListener;

/// Test that a streaming upload to a server that sends RST mid-body
/// returns an error. The server accepts the connection, reads the HTTP
/// headers, then drops the stream (sends RST, not graceful close).
/// The client must surface an error from `send().await` — a half-sent
/// body is not a success.
#[tokio::test]
async fn streaming_upload_server_rst_mid_body_error() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    // Spawn a server task that accepts, reads HTTP headers up through
    // the blank line, then forcefully drops the stream (RST via SO_LINGER=0).
    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut stream = stream;
        // Read headers until \r\n\r\n boundary.
        let mut buf = [0u8; 4096];
        let mut read = 0usize;
        loop {
            let n = stream.read(&mut buf[read..]).await.unwrap();
            if n == 0 {
                break;
            }
            read += n;
            if buf[..read].windows(4).any(|w| w == b"\r\n\r\n") {
                break;
            }
        }
        // Send RST by setting SO_LINGER to 0, then dropping.
        let raw = stream.into_std().unwrap();
        let sock = socket2::SockRef::from(&raw);
        let _ = sock.set_linger(Some(Duration::from_secs(0)));
        drop(raw);
    });

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap();

    // Build a large streaming body (many chunks totalling ~1MB) so the
    // upload is definitely in-flight when the RST arrives.
    let chunk_data = Bytes::from(vec![b'x'; 1024]);
    let chunks: Vec<Result<hyper::body::Frame<Bytes>, aioduct::Error>> = (0..1024)
        .map(|_| Ok(hyper::body::Frame::data(chunk_data.clone())))
        .collect();

    let stream = futures_util::stream::iter(chunks);
    let stream_body: aioduct::body::RequestBodySend =
        http_body_util::StreamBody::new(stream).boxed_unsync();

    let result = client
        .post(&format!("http://{addr}/"))
        .unwrap()
        .body_stream(stream_body)
        .send()
        .await;

    assert!(
        result.is_err(),
        "streaming upload to server that RST mid-body must return an error, \
         got: {:?}",
        result.ok().map(|r| r.status())
    );
}

/// Test that a 307 redirect with a buffered POST body preserves the method
/// and body, but strips the Authorization header on cross-origin redirect.
#[tokio::test]
async fn redirect_307_buffered_post_strips_auth_preserves_body() {
    // ── Target server: echoes method, body, and auth header presence ──
    let (target_addr, _) = h1_server_with(|req| async move {
        let method = req.method().to_string();
        let has_auth = req.headers().get("authorization").is_some();
        let body_bytes = req.into_body().collect().await.unwrap().to_bytes();
        let body_str = String::from_utf8_lossy(&body_bytes).to_string();
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(format!(
            "method={method} body={body_str} has_auth={has_auth}"
        )))))
    })
    .await;

    // ── Redirect server: 307 to target on a different port ──
    let (redirect_addr, _) = h1_server_with(move |_req| {
        let target = format!("http://127.0.0.1:{}/final", target_addr.port());
        async move {
            Ok::<_, Infallible>(
                Response::builder()
                    .status(307)
                    .header("location", target)
                    .body(Full::new(Bytes::new()))
                    .unwrap(),
            )
        }
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();

    let body_data = "my-post-body-content";
    let resp = client
        .post(&format!("http://{redirect_addr}/"))
        .unwrap()
        .body(body_data)
        .header(
            http::header::AUTHORIZATION,
            http::header::HeaderValue::from_static("Bearer secret-token"),
        )
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    let body = resp.text().await.unwrap();

    // Method preserved on 307
    assert!(
        body.contains("method=POST"),
        "307 redirect should preserve POST method, got: {body}"
    );
    // Body content preserved
    assert!(
        body.contains("body=my-post-body-content"),
        "body should be preserved on 307 redirect, got: {body}"
    );
    // Authorization stripped on cross-origin
    assert!(
        body.contains("has_auth=false"),
        "Authorization header should be stripped on cross-origin 307 redirect, got: {body}"
    );
}
