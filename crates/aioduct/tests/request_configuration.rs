#![cfg(feature = "tokio")]

use aioduct::{Netrc, RetryConfig, TokioClient};
use aioduct_test_server::h1::h1_server_with;
use bytes::Bytes;
use http::{HeaderMap, HeaderName, HeaderValue, Response};
use http_body_util::Full;
use std::convert::Infallible;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[tokio::test]
async fn netrc_redirect_uses_destination_credentials_and_retry_preserves_them() {
    let attempts = Arc::new(AtomicU32::new(0));
    let seen = Arc::new(Mutex::new(Vec::new()));
    let count = attempts.clone();
    let capture = seen.clone();
    let (target, _) = h1_server_with(move |request| {
        let capture = capture.clone();
        let attempt = count.fetch_add(1, Ordering::SeqCst);
        async move {
            capture
                .lock()
                .unwrap()
                .push(request.headers()["authorization"].clone());
            Ok::<_, Infallible>(
                Response::builder()
                    .status(if attempt == 0 { 503 } else { 200 })
                    .body(Full::new(Bytes::new()))
                    .unwrap(),
            )
        }
    })
    .await;
    let (origin, _) = h1_server_with(move |request| async move {
        assert_eq!(request.headers()["authorization"], "Basic c291cmNlOnBhc3M=");
        Ok::<_, Infallible>(
            Response::builder()
                .status(302)
                .header(
                    "location",
                    format!("http://localhost:{}/final", target.port()),
                )
                .body(Full::new(Bytes::new()))
                .unwrap(),
        )
    })
    .await;
    let client = TokioClient::builder()
        .netrc(Netrc::parse("machine 127.0.0.1 login source password pass machine localhost login target password pass"))
        .build().unwrap();
    let response = client
        .get(&format!("http://{origin}/start"))
        .unwrap()
        .retry(
            RetryConfig::default()
                .max_retries(1)
                .initial_backoff(Duration::ZERO),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(
        *seen.lock().unwrap(),
        vec![HeaderValue::from_static("Basic dGFyZ2V0OnBhc3M="); 2]
    );
}

#[tokio::test]
async fn explicit_auth_takes_precedence_and_digest_replay_overrides_netrc() {
    let attempts = Arc::new(AtomicU32::new(0));
    let count = attempts.clone();
    let (addr, _) = h1_server_with(move |request| {
        let attempt = count.fetch_add(1, Ordering::SeqCst);
        async move {
            if request.uri().path() == "/explicit" {
                assert_eq!(request.headers()["authorization"], "Bearer explicit");
                return Ok::<_, Infallible>(Response::new(Full::new(Bytes::new())));
            }
            // The explicit request occupies attempt zero.
            if attempt == 1 {
                assert_eq!(request.headers()["authorization"], "Basic dXNlcjpwYXNz");
                Ok(Response::builder()
                    .status(401)
                    .header(
                        "www-authenticate",
                        "Digest realm=\"test\", nonce=\"nonce\", qop=\"auth\"",
                    )
                    .body(Full::new(Bytes::new()))
                    .unwrap())
            } else {
                assert!(
                    request.headers()["authorization"]
                        .to_str()
                        .unwrap()
                        .starts_with("Digest ")
                );
                Ok(Response::new(Full::new(Bytes::new())))
            }
        }
    })
    .await;
    let client = TokioClient::builder()
        .netrc(Netrc::parse("machine 127.0.0.1 login user password pass"))
        .digest_auth("user", "pass")
        .build()
        .unwrap();
    client
        .get(&format!("http://{addr}/explicit"))
        .unwrap()
        .header_str("authorization", "Bearer explicit")
        .unwrap()
        .send()
        .await
        .unwrap()
        .bytes()
        .await
        .unwrap();
    assert_eq!(
        client
            .get(&format!("http://{addr}/digest"))
            .unwrap()
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    assert_eq!(attempts.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn trace_headers_obey_sensitive_redirect_rules_without_reinjection() {
    for mode in 0..3 {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let original = Arc::new(Mutex::new(None));
        let back = original.clone();
        let capture = seen.clone();
        let (target, _) = h1_server_with(move |request| {
            let back = back.clone();
            let capture = capture.clone();
            async move {
                capture.lock().unwrap().push(request.headers().clone());
                Ok::<_, Infallible>(
                    Response::builder()
                        .status(302)
                        .header(
                            "location",
                            format!("http://{}/final", back.lock().unwrap().unwrap()),
                        )
                        .body(Full::new(Bytes::new()))
                        .unwrap(),
                )
            }
        })
        .await;
        let capture = seen.clone();
        let finals = Arc::new(AtomicU32::new(0));
        let count = finals.clone();
        let (origin, _) = h1_server_with(move |request| {
            let capture = capture.clone();
            let count = count.clone();
            async move {
                capture.lock().unwrap().push(request.headers().clone());
                let response = match request.uri().path() {
                    "/start" => Response::builder().status(302).header("location", "/same"),
                    "/same" => Response::builder()
                        .status(302)
                        .header("location", format!("http://{target}/back")),
                    "/final" => {
                        Response::builder().status(if count.fetch_add(1, Ordering::SeqCst) == 0 {
                            503
                        } else {
                            200
                        })
                    }
                    path => panic!("unexpected path {path}"),
                };
                Ok::<_, Infallible>(response.body(Full::new(Bytes::new())).unwrap())
            }
        })
        .await;
        *original.lock().unwrap() = Some(origin);
        let mut headers = HeaderMap::new();
        let mut builder = TokioClient::builder();
        for name in ["traceparent", "tracestate", "baggage"] {
            let mut value = HeaderValue::from_static("application-context");
            if mode == 1 {
                value.set_sensitive(true);
            }
            if mode == 2 {
                builder = builder.sensitive_header(HeaderName::from_static(name));
            }
            headers.insert(name, value);
        }
        // Defaults must not restore sensitive headers after the redirect either.
        let client = builder.default_headers(headers).build().unwrap();
        let response = client
            .get(&format!("http://{origin}/start"))
            .unwrap()
            .retry(
                RetryConfig::default()
                    .max_retries(1)
                    .initial_backoff(Duration::ZERO),
            )
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 5);
        for (index, headers) in seen.iter().enumerate() {
            for name in ["traceparent", "tracestate", "baggage"] {
                assert_eq!(
                    headers.contains_key(name),
                    mode == 0 || index < 2,
                    "mode={mode}, hop={index}, header={name}"
                );
            }
        }
    }
}

#[cfg(all(feature = "gzip", feature = "blocking"))]
#[tokio::test]
async fn blocking_netrc_and_no_decompression_keep_encoded_response() {
    use flate2::{Compression, write::GzEncoder};
    use std::io::Write;
    let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
    encoder.write_all(b"blocking payload").unwrap();
    let compressed = encoder.finish().unwrap();
    let expected = compressed.clone();
    let (addr, _) = h1_server_with(move |request| {
        let compressed = compressed.clone();
        async move {
            assert_eq!(request.headers()["authorization"], "Basic dXNlcjpwYXNz");
            assert!(!request.headers().contains_key("accept-encoding"));
            Ok::<_, Infallible>(
                Response::builder()
                    .header("content-encoding", "gzip")
                    .header("content-length", compressed.len())
                    .body(Full::new(Bytes::from(compressed)))
                    .unwrap(),
            )
        }
    })
    .await;
    let worker = std::thread::spawn(move || {
        let client = aioduct::BlockingTokioClient::new(
            TokioClient::builder()
                .netrc(Netrc::parse("machine 127.0.0.1 login user password pass"))
                .build()
                .unwrap(),
        )
        .unwrap();
        let response = client
            .get(&format!("http://{addr}/"))
            .unwrap()
            .no_decompression()
            .send()
            .unwrap();
        assert_eq!(response.headers()["content-encoding"], "gzip");
        assert_eq!(
            response.headers()["content-length"],
            expected.len().to_string()
        );
        assert_eq!(response.bytes().unwrap(), expected);
    });
    tokio::task::spawn_blocking(move || worker.join().unwrap())
        .await
        .unwrap();
}

#[cfg(feature = "gzip")]
#[tokio::test]
async fn no_decompression_preserves_encoded_cache_hits_revalidation_and_fallback() {
    use flate2::{Compression, write::GzEncoder};
    use std::io::Write;
    for mode in ["fresh", "revalidate", "fallback"] {
        let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
        encoder.write_all(b"encoded cache payload").unwrap();
        let compressed = encoder.finish().unwrap();
        let expected = compressed.clone();
        let attempts = Arc::new(AtomicU32::new(0));
        let count = attempts.clone();
        let (addr, _) = h1_server_with(move |request| {
            let compressed = compressed.clone();
            let attempt = count.fetch_add(1, Ordering::SeqCst);
            async move {
                assert!(!request.headers().contains_key("accept-encoding"));
                if attempt == 0 {
                    return Ok::<_, Infallible>(
                        Response::builder()
                            .header(
                                "cache-control",
                                if mode == "fresh" {
                                    "max-age=3600"
                                } else {
                                    "max-age=0, stale-if-error=86400"
                                },
                            )
                            .header("etag", "\"encoded\"")
                            .header("content-encoding", "gzip")
                            .header("content-length", compressed.len())
                            .body(Full::new(Bytes::from(compressed)))
                            .unwrap(),
                    );
                }
                assert_ne!(mode, "fresh");
                assert_eq!(request.headers()["if-none-match"], "\"encoded\"");
                Ok(Response::builder()
                    .status(if mode == "revalidate" { 304 } else { 503 })
                    .body(Full::new(Bytes::new()))
                    .unwrap())
            }
        })
        .await;
        let client = TokioClient::builder()
            .cache(aioduct::HttpCache::new())
            .build()
            .unwrap();
        for _ in 0..2 {
            let response = client
                .get(&format!("http://{addr}/"))
                .unwrap()
                .no_decompression()
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), 200);
            assert_eq!(response.headers()["content-encoding"], "gzip");
            assert_eq!(
                response.headers()["content-length"],
                expected.len().to_string()
            );
            assert_eq!(response.bytes().await.unwrap(), expected);
        }
        assert_eq!(
            attempts.load(Ordering::SeqCst),
            if mode == "fresh" { 1 } else { 2 }
        );
    }
}
