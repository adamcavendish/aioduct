use super::super::*;
use super::multipart::multipart_body;
use super::support::connected_budget_client;

use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt, Full};

async fn read_http_request(
    tls: &mut rustls::ServerConnection,
    stream: &mut TokioIo<tokio::io::DuplexStream>,
) -> Vec<u8> {
    tokio::time::timeout(Duration::from_secs(5), async {
        let mut request = Vec::new();
        loop {
            if let Some(header_end) = request.windows(4).position(|window| window == b"\r\n\r\n") {
                let body_start = header_end + 4;
                let headers = std::str::from_utf8(&request[..header_end]).unwrap();
                let content_length = headers
                    .lines()
                    .filter_map(|line| line.split_once(':'))
                    .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                    .map(|(_, value)| value.trim().parse::<usize>().unwrap())
                    .expect("request should include Content-Length");
                let request_len = body_start + content_length;
                assert!(
                    request.len() <= request_len,
                    "received plaintext after the declared HTTP request body"
                );
                if request.len() == request_len {
                    return Ok::<_, io::Error>(request);
                }
            }

            let mut buf = [0u8; 4096];
            let n = server_read(tls, stream, &mut buf).await?;
            if n == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "HTTP request ended before its body was complete",
                ));
            }
            request.extend_from_slice(&buf[..n]);
        }
    })
    .await
    .expect("HTTP request read should not hang")
    .expect("HTTP request read should succeed")
}

fn assert_no_buffered_plaintext(tls: &mut rustls::ServerConnection) {
    let mut extra = [0u8; 1];
    match tls.reader().read(&mut extra) {
        Ok(0) => {}
        Ok(n) => panic!("received {n} trailing plaintext bytes after the HTTP request"),
        Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
        Err(e) => panic!("failed while checking for trailing plaintext: {e}"),
    }
}

#[tokio::test]
async fn hyper_h1_multipart_upload_resumes_after_tls_backpressure() {
    let (mut client_tls, mut server_stream, mut srv_conn, control) =
        connected_budget_client().await;
    client_tls.tls.set_buffer_limit(Some(1024));
    control.set_write_budget(Some(1));

    let boundary = "AioductBackpressureBoundary";
    let file: Vec<u8> = (0..4096).map(|i| (i % 251) as u8).collect();
    let multipart = multipart_body(boundary, &file);
    let expected_multipart = multipart.clone();

    let server_task = tokio::spawn(async move {
        let request = read_http_request(&mut srv_conn, &mut server_stream).await;
        let header_end = request
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .unwrap();
        let headers = std::str::from_utf8(&request[..header_end]).unwrap();
        assert!(headers.starts_with("POST /api/v2/ocr/jobs HTTP/1.1\r\n"));
        assert!(headers.lines().any(|line| {
            line.eq_ignore_ascii_case(&format!(
                "content-type: multipart/form-data; boundary={boundary}"
            ))
        }));
        assert_eq!(&request[header_end + 4..], expected_multipart);

        server_write(
            &mut srv_conn,
            &mut server_stream,
            b"HTTP/1.1 200 OK\r\nContent-Length: 9\r\nConnection: close\r\n\r\nupload ok",
        )
        .await
        .unwrap();
        srv_conn.send_close_notify();
        while srv_conn.wants_write() {
            std::future::poll_fn(|cx| srv_write_tls(&mut srv_conn, &mut server_stream, cx))
                .await
                .unwrap();
        }
        std::future::poll_fn(|cx| Pin::new(&mut server_stream).poll_flush(cx))
            .await
            .unwrap();

        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                assert_no_buffered_plaintext(&mut srv_conn);
                let n =
                    std::future::poll_fn(|cx| srv_read_tls(&mut srv_conn, &mut server_stream, cx))
                        .await
                        .unwrap();
                if n == 0 {
                    break;
                }
                let state = srv_conn.process_new_packets().unwrap();
                assert_no_buffered_plaintext(&mut srv_conn);
                if state.peer_has_closed() {
                    break;
                }
            }
        })
        .await
        .expect("client should answer the TLS close notification");
    });

    let release_control = control.clone();
    let release_task = tokio::spawn(async move {
        tokio::time::timeout(
            Duration::from_secs(2),
            release_control.wait_for_blocked_writes(2),
        )
        .await
        .expect("TLS transport should become backpressured twice");
        release_control.set_write_budget(None);
    });

    let (mut sender, connection) = hyper::client::conn::http1::handshake(client_tls)
        .await
        .unwrap();
    let connection_task = tokio::spawn(connection);
    let request = http::Request::builder()
        .method(http::Method::POST)
        .uri("/api/v2/ocr/jobs")
        .header(
            http::header::CONTENT_TYPE,
            format!("multipart/form-data; boundary={boundary}"),
        )
        .header(http::header::CONTENT_LENGTH, multipart.len())
        .body(Full::new(Bytes::from(multipart)))
        .unwrap();

    let response = tokio::time::timeout(Duration::from_secs(5), sender.send_request(request))
        .await
        .expect("Hyper request should not hang")
        .expect("Hyper request should survive TLS backpressure");
    assert_eq!(response.status(), http::StatusCode::OK);
    assert_eq!(
        response.into_body().collect().await.unwrap().to_bytes(),
        Bytes::from_static(b"upload ok")
    );

    drop(sender);
    release_task.await.unwrap();
    server_task.await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), connection_task)
        .await
        .expect("Hyper connection should finish")
        .expect("Hyper connection task should not panic")
        .expect("Hyper connection should close cleanly");
}
