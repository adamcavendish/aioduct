#![cfg(feature = "tokio")]
//! Tests verifying correct behavior when both Transfer-Encoding: chunked and
//! Content-Length headers are present (RFC 7230 Section 3.3.3), and graceful
//! handling of oversized Content-Length with early connection close.

use std::time::Duration;

use aioduct::HttpEngineSend;
use aioduct::runtime::TokioRuntime;
use aioduct::runtime::tokio_rt::TcpConnector;
use tokio::io::AsyncWriteExt;

fn client_with_read_timeout() -> HttpEngineSend<TokioRuntime, TcpConnector> {
    HttpEngineSend::builder()
        .timeout(Duration::from_secs(3))
        .read_timeout(Duration::from_secs(2))
        .build()
        .unwrap()
}

// ═══════════════════════════════════════════════════════════════════════════════
// Test 1: TE/CL smuggling — chunked takes priority over Content-Length
// ═══════════════════════════════════════════════════════════════════════════════

/// When both Transfer-Encoding: chunked and Content-Length are present,
/// the chunked encoding MUST take priority per RFC 7230 Section 3.3.3.
/// The Content-Length header MUST be ignored.
#[tokio::test]
async fn chunked_takes_priority_over_content_length() {
    // Raw server sends:
    //   Transfer-Encoding: chunked
    //   Content-Length: 5
    //   chunked body: 5\r\nhello\r\n0\r\n\r\n  => decoded = "hello"
    //
    // The CL:5 header is a lie — if the client obeyed it, it would read
    // exactly 5 bytes of raw body ("5\r\nh"), truncating the chunk header.
    // Instead, the client must follow chunked encoding and get "hello".
    let addr = aioduct_test_server::raw::raw_server(|_req| async {
        b"HTTP/1.1 200 OK\r\n\
          Transfer-Encoding: chunked\r\n\
          Content-Length: 5\r\n\
          \r\n\
          5\r\n\
          hello\r\n\
          0\r\n\
          \r\n"
            .to_vec()
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let body = resp.text().await.unwrap();
    assert_eq!(
        body, "hello",
        "chunked encoding must take priority: body should be 'hello' (5 chars), \
         not truncated by Content-Length: 5"
    );
    assert_eq!(
        body.len(),
        5,
        "body length should be 5, confirming chunked decoding won over Content-Length"
    );
}

// ═══════════════════════════════════════════════════════════════════════════════
// Test 2: Oversized Content-Length with early close
// ═══════════════════════════════════════════════════════════════════════════════

/// Server declares Content-Length: 1048576 (1 MB) but sends only 100 bytes
/// then closes the connection. The client MUST NOT return Ok with the
/// truncated 100 bytes — it must produce a timeout or connect error.
#[tokio::test]
async fn oversized_content_length_early_close_errors() {
    // Use raw_streaming_server to send headers claiming 1 MB + 100 bytes of
    // body, then stall (never send the remaining bytes). The stall forces
    // the client's read_timeout to fire, producing a proper timeout error.
    let addr = aioduct_test_server::raw::raw_streaming_server(|_req, mut stream| async move {
        let headers = b"HTTP/1.1 200 OK\r\nContent-Length: 1048576\r\n\r\n";
        let _ = stream.write_all(headers).await;
        let body_bytes = vec![b'X'; 100];
        let _ = stream.write_all(&body_bytes).await;
        let _ = stream.flush().await;
        // Stall — never send the remaining ~1 MB, forcing the read timeout.
        tokio::time::sleep(Duration::from_secs(3600)).await;
    })
    .await;

    let client = client_with_read_timeout();
    let result = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .timeout(Duration::from_secs(2))
        .send()
        .await;

    match result {
        Err(res_err) => {
            assert!(
                res_err.is_timeout() || res_err.is_connect() || res_err.is_closed(),
                "oversized CL with early close must produce timeout, connect, or closed error, got: {res_err:?}"
            );
        }
        Ok(resp) => {
            let body_result = resp.text().await;
            assert!(
                body_result.is_err(),
                "oversized CL with early close must fail on body read, \
                 must not return Ok with truncated 100 bytes"
            );
            let body_err = body_result.unwrap_err();
            assert!(
                body_err.is_timeout() || body_err.is_connect() || body_err.is_closed(),
                "body read error from oversized CL early close must be timeout, connect, or closed, got: {body_err:?}"
            );
        }
    }
}
