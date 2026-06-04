#![cfg(all(feature = "tokio", feature = "rustls"))]
//! H2 stream cancellation tests: RST_STREAM mid-body, selective RST among
//! concurrent streams, and GOAWAY during concurrent requests.
//!
//! Run: cargo test -p aioduct --features "tokio,rustls,rustls-ring" --test h2_cancellation

use std::convert::Infallible;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::task::{Context, Poll};
use std::time::Duration;

use bytes::Bytes;
use http::{Request, Response};
use http_body::{Body, Frame, SizeHint};
use hyper::service::service_fn;
use rustls::pki_types::CertificateDer;
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;

use aioduct::HttpEngineSend;
use aioduct::runtime::TokioRuntime;
use aioduct::runtime::tokio_rt::TcpConnector;
use aioduct_test_server::TokioExec;
use aioduct_test_server::TokioIo;

// ── Helpers ───────────────────────────────────────────────────────────────

fn install_provider() {
    aioduct_test_server::tls::install_crypto_provider();
}

/// Plaintext H2-prior-knowledge client (used by the GOAWAY test).
fn h2_client() -> HttpEngineSend<TokioRuntime, TcpConnector> {
    HttpEngineSend::builder()
        .pool_idle_timeout(Duration::from_secs(60))
        .http2_prior_knowledge()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap()
}

/// Build a TLS-enabled client that trusts the given self-signed certificate.
fn tls_h2_client_with_cert(
    cert_der: &CertificateDer<'static>,
) -> HttpEngineSend<TokioRuntime, TcpConnector> {
    let client_config = aioduct_test_server::tls::make_client_config(cert_der);
    let connector = aioduct::tls::RustlsConnector::new(client_config);
    HttpEngineSend::builder()
        .tls(connector)
        .pool_idle_timeout(Duration::from_secs(60))
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap()
}

// ── Custom error body ─────────────────────────────────────────────────────

/// A response body that sends one data frame then either errors (simulating
/// an RST_STREAM from the server) or ends the stream normally.
struct ErrorAfterBody {
    /// Data to send before the stream terminates.
    data: Option<Bytes>,
    /// If true, the second `poll_frame` returns an error (RST_STREAM).
    /// If false, the second `poll_frame` returns `None` (graceful end).
    error_after: bool,
}

impl ErrorAfterBody {
    fn new_ok(data: impl Into<Bytes>) -> Self {
        Self {
            data: Some(data.into()),
            error_after: false,
        }
    }

    fn new_error(data: impl Into<Bytes>) -> Self {
        Self {
            data: Some(data.into()),
            error_after: true,
        }
    }
}

impl Body for ErrorAfterBody {
    type Data = Bytes;
    type Error = Box<dyn std::error::Error + Send + Sync>;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        let this = self.as_mut().get_mut();
        if let Some(data) = this.data.take() {
            // Send the data frame (no END_STREAM – more frames follow).
            Poll::Ready(Some(Ok(Frame::data(data))))
        } else if this.error_after {
            // Error simulates an RST_STREAM (INTERNAL_ERROR).
            Poll::Ready(Some(Err("RST_STREAM: INTERNAL_ERROR".into())))
        } else {
            // Normal end of stream.
            Poll::Ready(None)
        }
    }

    fn is_end_stream(&self) -> bool {
        false
    }

    fn size_hint(&self) -> SizeHint {
        let mut hint = SizeHint::default();
        if let Some(ref data) = self.data {
            hint.set_lower(data.len() as u64);
            hint.set_upper(data.len() as u64);
        }
        hint
    }
}

// ── TLS H2 server with custom body ────────────────────────────────────────

/// Spin up a TLS H2 server whose handler returns responses with an
/// `ErrorAfterBody`.  Returns the bound address and the certificate (DER) so
/// the client can trust it.
async fn tls_h2_error_body_server<F, Fut>(
    handler: F,
) -> (std::net::SocketAddr, CertificateDer<'static>)
where
    F: Fn(Request<hyper::body::Incoming>) -> Fut + Send + Clone + 'static,
    Fut: Future<Output = Result<Response<ErrorAfterBody>, Infallible>> + Send + 'static,
{
    let cert = aioduct_test_server::tls::generate_self_signed(&["localhost"]);
    let cert_der = cert.cert_der.clone();

    let server_config =
        rustls::ServerConfig::builder_with_provider(aioduct_test_server::tls::crypto_provider())
            .with_safe_default_protocol_versions()
            .expect("configured rustls provider does not support the default TLS versions")
            .with_no_client_auth()
            .with_single_cert(vec![cert.cert_der], cert.key_der.clone_key())
            .unwrap();

    let mut server_config = Arc::new(server_config);
    // ALPN: offer h2 so the client can negotiate HTTP/2.
    Arc::make_mut(&mut server_config).alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];

    let acceptor = TlsAcceptor::from(server_config);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let cert_der_ret = cert_der.clone();

    tokio::spawn(async move {
        loop {
            let (stream, _) = match listener.accept().await {
                Ok(v) => v,
                Err(_) => continue,
            };
            let acceptor = acceptor.clone();
            let handler = handler.clone();
            tokio::spawn(async move {
                let tls_stream = match acceptor.accept(stream).await {
                    Ok(s) => s,
                    Err(_) => return,
                };
                let io = TokioIo::new(tls_stream);
                let _ = hyper::server::conn::http2::Builder::new(TokioExec)
                    .serve_connection(
                        io,
                        service_fn(move |req| {
                            let handler = handler.clone();
                            async move { handler(req).await }
                        }),
                    )
                    .await;
            });
        }
    });

    (addr, cert_der_ret)
}

// ═══════════════════════════════════════════════════════════════════════════
// Test 1 — RST_STREAM mid-body causes an error (not partial data, not hang)
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn h2_rst_stream_mid_body_errors() {
    install_provider();

    let (addr, cert_der) = tls_h2_error_body_server(|_req| async {
        Ok::<_, Infallible>(Response::new(ErrorAfterBody::new_error("partial")))
    })
    .await;

    let client = tls_h2_client_with_cert(&cert_der);
    let url = format!("https://localhost:{}/", addr.port());

    let send_result = client.get(&url).unwrap().send().await;

    // The error may surface during send() (hyper propagates the RST_STREAM to
    // the request future) or during text() (body read fails).  Either way the
    // client must error, not return partial data and not hang.
    match send_result {
        Ok(resp) => {
            assert_eq!(resp.status(), 200);
            assert_eq!(resp.version(), http::Version::HTTP_2);
            let body_result = resp.text().await;
            assert!(
                body_result.is_err(),
                "text() should error after RST_STREAM, not return partial data"
            );
        }
        Err(e) => {
            // RST_STREAM surfacing at send() is valid behavior — the client
            // detected the stream reset before the response future resolved.
            let msg = format!("{e}");
            assert!(
                !msg.contains("timeout"),
                "error should not be a timeout hang, got: {e}"
            );
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Test 2 — Concurrent H2 streams: one gets RST, the others succeed
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn h2_concurrent_streams_one_rst_others_ok() {
    install_provider();

    let request_count = Arc::new(AtomicU32::new(0));
    let rst_target = 2u32; // 0-based → third request gets RST

    let (addr, cert_der) = {
        let req_cnt = request_count.clone();
        tls_h2_error_body_server(move |_req| {
            let n = req_cnt.fetch_add(1, Ordering::SeqCst);
            let body = if n == rst_target {
                ErrorAfterBody::new_error("partial-error")
            } else {
                ErrorAfterBody::new_ok("ok")
            };
            async move { Ok::<_, Infallible>(Response::new(body)) }
        })
        .await
    };

    let client = tls_h2_client_with_cert(&cert_der);
    let url = format!("https://localhost:{}/", addr.port());

    // Send 5 concurrent requests via `tokio::join!` so they all multiplex
    // on the same H2 connection.
    let c1 = client.clone();
    let c2 = client.clone();
    let c3 = client.clone();
    let c4 = client.clone();
    let c5 = client.clone();
    let u1 = url.clone();
    let u2 = url.clone();
    let u3 = url.clone();
    let u4 = url.clone();
    let u5 = url.clone();

    let (r1, r2, r3, r4, r5) = tokio::join!(
        async { c1.get(&u1).unwrap().send().await },
        async { c2.get(&u2).unwrap().send().await },
        async { c3.get(&u3).unwrap().send().await },
        async { c4.get(&u4).unwrap().send().await },
        async { c5.get(&u5).unwrap().send().await },
    );

    // Read bodies and count successes.
    let mut success = 0u32;
    let mut failure = 0u32;

    for result in [r1, r2, r3, r4, r5] {
        match result {
            Ok(resp) => {
                assert_eq!(resp.status(), 200);
                match resp.text().await {
                    Ok(body) if body == "ok" => success += 1,
                    Ok(_partial) => {
                        // Partial data from the RST stream is acceptable;
                        // treat as a failure for counting purposes.
                        failure += 1;
                    }
                    Err(_) => failure += 1,
                }
            }
            Err(_) => failure += 1,
        }
    }

    assert_eq!(
        success, 4,
        "expected 4 successful streams (200 + body 'ok'), got {success} successes and {failure} failures"
    );
    assert_eq!(
        failure, 1,
        "expected exactly 1 failed stream (the RST one), got {failure} failures"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// Test 3 — GOAWAY during concurrent requests: most should still complete
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn h2_goaway_during_concurrent_requests() {
    install_provider();

    // h2_goaway_after(2) sends GOAWAY after 2 requests on a connection.
    let (addr, counter) = aioduct_test_server::h2::h2_goaway_after(2).await;
    let client = h2_client();
    let url = format!("http://{addr}/");

    // Warm the connection so it is pooled.
    let warm = client.get(&url).unwrap().send().await.unwrap();
    assert_eq!(warm.status(), 200);
    let _ = warm.text().await.unwrap();

    // Let GOAWAY arrive before we fire concurrent requests.
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Fire 5 concurrent requests — at least 4 should succeed.
    let c1 = client.clone();
    let c2 = client.clone();
    let c3 = client.clone();
    let c4 = client.clone();
    let c5 = client.clone();
    let u1 = url.clone();
    let u2 = url.clone();
    let u3 = url.clone();
    let u4 = url.clone();
    let u5 = url.clone();

    let (r1, r2, r3, r4, r5) = tokio::join!(
        async { c1.get(&u1).unwrap().send().await },
        async { c2.get(&u2).unwrap().send().await },
        async { c3.get(&u3).unwrap().send().await },
        async { c4.get(&u4).unwrap().send().await },
        async { c5.get(&u5).unwrap().send().await },
    );

    let mut success = 0u32;
    let mut failure = 0u32;

    for result in [r1, r2, r3, r4, r5] {
        match result {
            Ok(resp) => {
                assert_eq!(resp.status(), 200, "successful response must be 200");
                match resp.text().await {
                    Ok(body) if body == "ok" => success += 1,
                    Ok(_) => success += 1,
                    Err(_) => failure += 1,
                }
            }
            Err(_) => failure += 1,
        }
    }

    assert!(
        success >= 4,
        "GOAWAY after 2: expected at least 4 of 5 concurrent requests to succeed \
         (hyper retries or opens new connection for remaining), got {success} successes, {failure} failures"
    );

    // All requests sent should be counted by the server. GOAWAY may reject
    // a stream before service_fn runs, so allow >= 5 (not strict == 6).
    assert!(
        counter.requests() >= 5,
        "expected at least 5 server requests, got {}",
        counter.requests()
    );
}
