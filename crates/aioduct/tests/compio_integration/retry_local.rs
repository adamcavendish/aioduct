use super::*;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

#[derive(Clone)]
struct LocalDigestRetryRecorder {
    observer_attempts: Arc<std::sync::Mutex<Vec<(u32, u32)>>>,
}

impl aioduct::RequestObserver for LocalDigestRetryRecorder {
    fn on_event(&self, event: &aioduct::RequestEvent) {
        if let aioduct::RequestPhase::Retrying {
            attempt,
            max_retries,
            ..
        } = &event.phase
        {
            self.observer_attempts
                .lock()
                .unwrap()
                .push((*attempt, *max_retries));
        }
    }

    fn on_connection_event(&self, _event: &aioduct::ConnectionEvent) {}
}

#[test]
fn local_fresh_h2_publication_starts_reaper_before_first_response() {
    let (addr_tx, addr_rx) = std::sync::mpsc::sync_channel(1);
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
    let (done_tx, done_rx) = std::sync::mpsc::sync_channel(1);
    let server = std::thread::spawn(move || {
        tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(async move {
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                addr_tx.send(listener.local_addr().unwrap()).unwrap();
                let (stream, _) = listener.accept().await.unwrap();
                let mut connection = h2::server::handshake(stream).await.unwrap();
                let (_request, mut response) = connection
                    .accept()
                    .await
                    .expect("Local H2 connection ended before the first request")
                    .expect("first Local H2 request was invalid");
                response.send_reset(h2::Reason::CANCEL);
                tokio::pin!(shutdown_rx);
                tokio::select! {
                    _ = &mut shutdown_rx => {}
                    _ = connection.accept() => {}
                }
            });
        let _ = done_tx.send(());
    });
    let addr = addr_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    let idle_timeout = Duration::from_millis(50);

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .pool_idle_timeout(idle_timeout)
            .timeout(Duration::from_secs(2))
            .build_local()
            .unwrap();
        let error = client
            .get_local(&format!("http://{addr}/reset"))
            .unwrap()
            .h2c_prior_knowledge()
            .send()
            .await
            .unwrap_err();
        assert!(error.to_string().contains("http2 error"), "{error}");
        assert_eq!(
            client.pool_stats().idle_pool_entries,
            1,
            "the original fresh Local H2 handle should be published before the clone fails"
        );
        compio_runtime::time::sleep(idle_timeout + Duration::from_millis(150)).await;
        assert_eq!(client.pool_stats().idle_pool_entries, 0);
        assert!(client.pool_stats().idle_timeout_evictions >= 1);
    });

    let _ = shutdown_tx.send(());
    done_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("Local H2 reset server did not stop");
    server.join().unwrap();
}

#[test]
fn local_digest_response_drain_failure_does_not_commit_retry_state() {
    let (addr_tx, addr_rx) = std::sync::mpsc::sync_channel(1);
    let (done_tx, done_rx) = std::sync::mpsc::sync_channel(1);
    let server = std::thread::spawn(move || {
        tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(async move {
                use tokio::io::{AsyncReadExt, AsyncWriteExt};

                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                addr_tx.send(listener.local_addr().unwrap()).unwrap();
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                let mut buffer = [0_u8; 1024];
                while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                    let read = stream.read(&mut buffer).await.unwrap();
                    if read == 0 {
                        break;
                    }
                    request.extend_from_slice(&buffer[..read]);
                }
                stream
                    .write_all(
                        b"HTTP/1.1 401 Unauthorized\r\n\
                          WWW-Authenticate: Digest realm=\"test\", nonce=\"nonce\", qop=\"auth\"\r\n\
                          Content-Length: 4\r\n\
                          Connection: close\r\n\r\n\
                          x",
                    )
                    .await
                    .unwrap();
                stream.shutdown().await.unwrap();
            });
        let _ = done_tx.send(());
    });
    let addr = addr_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    let budget = aioduct::RetryBudget::new(1, 0);
    let recorder = LocalDigestRetryRecorder {
        observer_attempts: Arc::new(std::sync::Mutex::new(Vec::new())),
    };

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .digest_auth("user", "password")
            .request_observer(recorder.clone())
            .retry(
                aioduct::RetryConfig::default()
                    .max_retries(2)
                    .budget(budget.clone())
                    .classify(|_| aioduct::RetryDecision::DoNotRetry),
            )
            .build_local()
            .unwrap();
        let error = compio_runtime::time::timeout(
            Duration::from_secs(2),
            client
                .get_local(&format!("http://{addr}/digest"))
                .unwrap()
                .send(),
        )
        .await
        .expect("Local Digest response drain stalled")
        .unwrap_err();
        assert!(error.to_string().contains("body"), "{error}");
    });

    assert_eq!(budget.available(), 1);

    assert!(recorder.observer_attempts.lock().unwrap().is_empty());
    done_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("Local Digest server did not stop");
    server.join().unwrap();
}

#[test]
fn client_default_retry_applies_to_local_requests() {
    let attempts = Arc::new(AtomicU32::new(0));
    let server_attempts = attempts.clone();
    let addr = start_server_with_tokio(move |_req| {
        let attempts = server_attempts.clone();
        async move {
            let attempt = attempts.fetch_add(1, Ordering::SeqCst);
            let status = if attempt == 0 {
                http::StatusCode::SERVICE_UNAVAILABLE
            } else {
                http::StatusCode::OK
            };
            Ok::<_, Infallible>(
                Response::builder()
                    .status(status)
                    .body(Full::new(Bytes::from_static(b"response")))
                    .unwrap(),
            )
        }
    });

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .retry(
                aioduct::RetryConfig::default()
                    .max_retries(1)
                    .initial_backoff(Duration::ZERO),
            )
            .build_local()
            .unwrap();
        let response = client
            .get_local(&format!("http://{addr}/"))
            .unwrap()
            .send()
            .await
            .unwrap();

        assert_eq!(response.status(), http::StatusCode::OK);
    });
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
}

#[test]
fn local_status_retry_refreshes_cookie_jar_headers() {
    let attempts = Arc::new(AtomicU32::new(0));
    let server_attempts = attempts.clone();
    let addr = start_server_with_tokio(move |request| {
        let attempts = server_attempts.clone();
        async move {
            let attempt = attempts.fetch_add(1, Ordering::SeqCst);
            if attempt == 0 {
                assert!(request.headers().get(http::header::COOKIE).is_none());
                Ok::<_, Infallible>(
                    Response::builder()
                        .status(http::StatusCode::SERVICE_UNAVAILABLE)
                        .header(http::header::SET_COOKIE, "session=fresh; Path=/")
                        .body(Full::new(Bytes::new()))
                        .unwrap(),
                )
            } else {
                assert_eq!(
                    request.headers().get(http::header::COOKIE).unwrap(),
                    "session=fresh"
                );
                Ok(Response::new(Full::new(Bytes::from_static(b"ok"))))
            }
        }
    });

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .cookie_jar(aioduct::CookieJar::new())
            .retry(
                aioduct::RetryConfig::default()
                    .max_retries(1)
                    .initial_backoff(Duration::ZERO),
            )
            .build_local()
            .unwrap();
        let response = client
            .get_local(&format!("http://{addr}/cookie"))
            .unwrap()
            .send()
            .await
            .unwrap();

        assert_eq!(response.text().await.unwrap(), "ok");
    });
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
}

#[test]
fn local_configured_retry_recovers_from_connection_error() {
    let live_addr = start_server_tokio();
    let dead_addr = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);
        addr
    };
    let resolutions = Arc::new(AtomicU32::new(0));
    let resolver_calls = resolutions.clone();
    let recorder = LocalDigestRetryRecorder {
        observer_attempts: Arc::new(std::sync::Mutex::new(Vec::new())),
    };

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .resolver(move |_host: &str, _port: u16| {
                let attempt = resolver_calls.fetch_add(1, Ordering::SeqCst);
                let addr = if attempt == 0 { dead_addr } else { live_addr };
                Box::pin(async move { Ok(addr) })
                    as std::pin::Pin<
                        Box<dyn std::future::Future<Output = std::io::Result<SocketAddr>> + Send>,
                    >
            })
            .request_observer(recorder.clone())
            .retry(
                aioduct::RetryConfig::default()
                    .max_retries(1)
                    .initial_backoff(Duration::ZERO),
            )
            .build_local()
            .unwrap();
        let response = client
            .get_local(&format!("http://retry-local.test:{}/", live_addr.port()))
            .unwrap()
            .send()
            .await
            .unwrap();

        assert_eq!(response.status(), http::StatusCode::OK);
        assert_eq!(response.text().await.unwrap(), "hello aioduct");
    });

    assert_eq!(resolutions.load(Ordering::SeqCst), 2);

    assert_eq!(*recorder.observer_attempts.lock().unwrap(), vec![(1, 1)]);
}

#[test]
fn local_configured_headers_preserves_buffered_body_replay() {
    let attempts = Arc::new(AtomicU32::new(0));
    let server_attempts = attempts.clone();
    let addr = start_server_with_tokio(move |request| {
        let attempts = server_attempts.clone();
        async move {
            let attempt = attempts.fetch_add(1, Ordering::SeqCst);
            assert_eq!(request.headers()["x-local-request"], "applied");
            assert_eq!(
                request.into_body().collect().await.unwrap().to_bytes(),
                "local buffered payload"
            );
            Ok::<_, Infallible>(
                Response::builder()
                    .status(if attempt == 0 { 503 } else { 200 })
                    .body(Full::new(Bytes::new()))
                    .unwrap(),
            )
        }
    });

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .default_headers(http::HeaderMap::from_iter([(
                http::header::HeaderName::from_static("x-local-request"),
                http::HeaderValue::from_static("applied"),
            )]))
            .retry(
                aioduct::RetryConfig::default()
                    .max_retries(1)
                    .initial_backoff(Duration::ZERO)
                    .classify(|_| aioduct::RetryDecision::Retry),
            )
            .build_local()
            .unwrap();
        let response = client
            .post_local(&format!("http://{addr}/"))
            .unwrap()
            .body("local buffered payload")
            .send()
            .await
            .unwrap();

        assert_eq!(response.status(), http::StatusCode::OK);
    });
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
}

#[test]
fn local_configured_retry_preserves_configured_request() {
    let attempts = Arc::new(AtomicU32::new(0));
    let server_attempts = attempts.clone();
    let addr = start_server_with_tokio(move |request| {
        let attempts = server_attempts.clone();
        async move {
            let attempt = attempts.fetch_add(1, Ordering::SeqCst);
            assert_eq!(request.method(), http::Method::PUT);
            assert_eq!(request.headers()["x-finalized-request"], "first");
            assert!(
                request
                    .into_body()
                    .collect()
                    .await
                    .unwrap()
                    .to_bytes()
                    .is_empty()
            );
            Ok::<_, Infallible>(if attempt == 0 {
                Response::builder()
                    .status(http::StatusCode::SERVICE_UNAVAILABLE)
                    .body(Full::new(Bytes::new()))
                    .unwrap()
            } else {
                Response::new(Full::new(Bytes::from_static(b"ok")))
            })
        }
    });

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .default_headers(http::HeaderMap::from_iter([(
                http::header::HeaderName::from_static("x-finalized-request"),
                http::HeaderValue::from_static("first"),
            )]))
            .retry(
                aioduct::RetryConfig::default()
                    .max_retries(1)
                    .initial_backoff(Duration::ZERO),
            )
            .build_local()
            .unwrap();
        let response = client
            .request_local(http::Method::PUT, &format!("http://{addr}/"))
            .unwrap()
            .send()
            .await
            .unwrap();

        assert_eq!(response.status(), http::StatusCode::OK);
    });
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
}

#[test]
fn local_digest_retry_preserves_configured_request() {
    let attempts = Arc::new(AtomicU32::new(0));
    let server_attempts = attempts.clone();
    let addr = start_server_with_tokio(move |request| {
        let attempts = server_attempts.clone();
        async move {
            let attempt = attempts.fetch_add(1, Ordering::SeqCst);
            assert_eq!(request.method(), http::Method::POST);
            assert_eq!(request.headers()["x-finalized-request"], "first");
            if attempt > 0 {
                assert!(request.headers().contains_key(http::header::AUTHORIZATION));
            }
            assert!(
                request
                    .into_body()
                    .collect()
                    .await
                    .unwrap()
                    .to_bytes()
                    .is_empty()
            );
            Ok::<_, Infallible>(if attempt == 0 {
                Response::builder()
                    .status(http::StatusCode::UNAUTHORIZED)
                    .header(
                        http::header::WWW_AUTHENTICATE,
                        r#"Digest realm="local", nonce="nonce", qop="auth""#,
                    )
                    .body(Full::new(Bytes::new()))
                    .unwrap()
            } else {
                Response::new(Full::new(Bytes::from_static(b"ok")))
            })
        }
    });

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .digest_auth("user", "pass")
            .default_headers(http::HeaderMap::from_iter([(
                http::header::HeaderName::from_static("x-finalized-request"),
                http::HeaderValue::from_static("first"),
            )]))
            .build_local()
            .unwrap();
        let response = client
            .post_local(&format!("http://{addr}/"))
            .unwrap()
            .send()
            .await
            .unwrap();

        assert_eq!(response.status(), http::StatusCode::OK);
    });
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
}

#[test]
fn local_unsupported_digest_challenge_preserves_finalized_retry_state() {
    use std::sync::Mutex;

    let attempts = Arc::new(AtomicU32::new(0));
    let server_attempts = attempts.clone();
    let addr = start_server_with_tokio(move |_req| {
        let attempts = server_attempts.clone();
        async move {
            attempts.fetch_add(1, Ordering::SeqCst);
            Ok::<_, Infallible>(
                Response::builder()
                    .status(http::StatusCode::UNAUTHORIZED)
                    .header(http::header::WWW_AUTHENTICATE, r#"Basic realm="fallback""#)
                    .body(Full::new(Bytes::new()))
                    .unwrap(),
            )
        }
    });

    let classified_method = Arc::new(Mutex::new(None));
    let observed_method = classified_method.clone();

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .digest_auth("user", "pass")
            .retry(
                aioduct::RetryConfig::default()
                    .max_retries(1)
                    .initial_backoff(Duration::ZERO)
                    .classify(move |context| {
                        *observed_method.lock().unwrap() = Some(context.method().clone());
                        aioduct::RetryDecision::DoNotRetry
                    }),
            )
            .build_local()
            .unwrap();
        let response = client
            .get_local(&format!("http://{addr}/"))
            .unwrap()
            .send()
            .await
            .unwrap();

        assert_eq!(response.status(), http::StatusCode::UNAUTHORIZED);
    });
    assert_eq!(attempts.load(Ordering::SeqCst), 1);

    assert_eq!(*classified_method.lock().unwrap(), Some(http::Method::GET));
}

#[test]
fn local_digest_retry_respects_configured_retry_count() {
    let attempts = Arc::new(AtomicU32::new(0));
    let server_attempts = attempts.clone();
    let addr = start_server_with_tokio(move |_req| {
        let attempts = server_attempts.clone();
        async move {
            attempts.fetch_add(1, Ordering::SeqCst);
            Ok::<_, Infallible>(
                Response::builder()
                    .status(http::StatusCode::UNAUTHORIZED)
                    .header(
                        http::header::WWW_AUTHENTICATE,
                        r#"Digest realm="count", nonce="nonce", qop="auth""#,
                    )
                    .body(Full::new(Bytes::new()))
                    .unwrap(),
            )
        }
    });

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .digest_auth("user", "pass")
            .retry(aioduct::RetryConfig::default().max_retries(0))
            .build_local()
            .unwrap();
        let response = client
            .get_local(&format!("http://{addr}/"))
            .unwrap()
            .send()
            .await
            .unwrap();

        assert_eq!(response.status(), http::StatusCode::UNAUTHORIZED);
    });
    assert_eq!(attempts.load(Ordering::SeqCst), 1);
}

#[test]
fn local_digest_retry_respects_configured_retry_budget() {
    let attempts = Arc::new(AtomicU32::new(0));
    let server_attempts = attempts.clone();
    let addr = start_server_with_tokio(move |_req| {
        let attempts = server_attempts.clone();
        async move {
            attempts.fetch_add(1, Ordering::SeqCst);
            Ok::<_, Infallible>(
                Response::builder()
                    .status(http::StatusCode::UNAUTHORIZED)
                    .header(
                        http::header::WWW_AUTHENTICATE,
                        r#"Digest realm="budget", nonce="nonce", qop="auth""#,
                    )
                    .body(Full::new(Bytes::new()))
                    .unwrap(),
            )
        }
    });
    let budget = aioduct::RetryBudget::new(0, 0);

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .digest_auth("user", "pass")
            .retry(
                aioduct::RetryConfig::default()
                    .max_retries(3)
                    .budget(budget.clone()),
            )
            .build_local()
            .unwrap();
        let response = client
            .get_local(&format!("http://{addr}/"))
            .unwrap()
            .send()
            .await
            .unwrap();

        assert_eq!(response.status(), http::StatusCode::UNAUTHORIZED);
    });
    assert_eq!(attempts.load(Ordering::SeqCst), 1);
    assert_eq!(budget.available(), 0);
}

#[test]
fn local_digest_and_configured_retries_share_attempts_and_callbacks() {
    let attempts = Arc::new(AtomicU32::new(0));
    let server_attempts = attempts.clone();
    let authorizations = Arc::new(std::sync::Mutex::new(Vec::new()));
    let server_authorizations = authorizations.clone();
    let addr = start_server_with_tokio(move |request| {
        let attempts = server_attempts.clone();
        let authorizations = server_authorizations.clone();
        async move {
            let attempt = attempts.fetch_add(1, Ordering::SeqCst);
            if let Some(value) = request.headers().get(http::header::AUTHORIZATION) {
                authorizations
                    .lock()
                    .unwrap()
                    .push(value.to_str().unwrap().to_owned());
            }
            Ok::<_, Infallible>(match attempt {
                0 => Response::builder()
                    .status(401)
                    .header(
                        http::header::WWW_AUTHENTICATE,
                        r#"Digest realm="local-events", nonce="local-nonce", qop="auth""#,
                    )
                    .body(Full::new(Bytes::new()))
                    .unwrap(),
                1 => {
                    assert!(request.headers().contains_key(http::header::AUTHORIZATION));
                    Response::builder()
                        .status(503)
                        .body(Full::new(Bytes::new()))
                        .unwrap()
                }
                _ => {
                    assert!(request.headers().contains_key(http::header::AUTHORIZATION));
                    Response::new(Full::new(Bytes::from_static(b"ok")))
                }
            })
        }
    });
    let recorder = LocalDigestRetryRecorder {
        observer_attempts: Arc::new(std::sync::Mutex::new(Vec::new())),
    };

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .digest_auth("user", "pass")
            .request_observer(recorder.clone())
            .retry(
                aioduct::RetryConfig::default()
                    .max_retries(2)
                    .initial_backoff(Duration::ZERO),
            )
            .build_local()
            .unwrap();
        let response = client
            .get_local(&format!("http://{addr}/digest-events"))
            .unwrap()
            .send()
            .await
            .unwrap();

        assert_eq!(response.status(), http::StatusCode::OK);
    });

    assert_eq!(attempts.load(Ordering::SeqCst), 3);

    assert_eq!(
        *recorder.observer_attempts.lock().unwrap(),
        vec![(1, 2), (2, 2)]
    );
    let authorizations = authorizations.lock().unwrap();
    assert_eq!(authorizations.len(), 2);
    assert!(authorizations[0].contains("nc=00000001"));
    assert!(authorizations[1].contains("nc=00000002"));
    assert_ne!(authorizations[0], authorizations[1]);
}

#[cfg(feature = "gzip")]
#[test]
fn local_netrc_and_no_decompression_survive_redirects() {
    use flate2::{Compression, write::GzEncoder};
    use std::io::Write;
    let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
    encoder.write_all(b"local decoded payload").unwrap();
    let compressed = encoder.finish().unwrap();
    let expected = compressed.clone();
    let accepts = Arc::new(std::sync::Mutex::new(Vec::new()));
    let capture = accepts.clone();
    let addr = start_server_with_tokio(move |request| {
        let compressed = compressed.clone();
        let capture = capture.clone();
        async move {
            assert_eq!(request.headers()["authorization"], "Basic dXNlcjpwYXNz");
            capture
                .lock()
                .unwrap()
                .push(request.headers().get("accept-encoding").cloned());
            Ok::<_, Infallible>(if request.uri().path() == "/start" {
                Response::builder()
                    .status(307)
                    .header("location", "/final")
                    .body(Full::new(Bytes::new()))
                    .unwrap()
            } else {
                Response::builder()
                    .header("content-encoding", "gzip")
                    .header("content-length", compressed.len())
                    .body(Full::new(Bytes::from(compressed)))
                    .unwrap()
            })
        }
    });
    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .netrc(aioduct::Netrc::parse(
                "machine 127.0.0.1 login user password pass",
            ))
            .build_local()
            .unwrap();
        let url = format!("http://{addr}/start");
        let raw = client
            .clone()
            .get_local(&url)
            .unwrap()
            .no_decompression()
            .send()
            .await
            .unwrap();
        assert_eq!(raw.headers()["content-encoding"], "gzip");
        assert_eq!(raw.headers()["content-length"], expected.len().to_string());
        assert_eq!(raw.bytes().await.unwrap(), expected);
        let decoded = client.get_local(&url).unwrap().send().await.unwrap();
        assert!(!decoded.headers().contains_key("content-encoding"));
        assert_eq!(decoded.text().await.unwrap(), "local decoded payload");
        let raw = client
            .get_local(&url)
            .unwrap()
            .header_str("accept-encoding", "gzip")
            .unwrap()
            .no_decompression()
            .send()
            .await
            .unwrap();
        assert_eq!(raw.bytes().await.unwrap(), expected);
    });
    let accepts = accepts.lock().unwrap();
    assert_eq!(accepts.len(), 6);
    assert!(accepts[..2].iter().all(Option::is_none));
    // Default negotiation may advertise every codec enabled by this build.
    assert!(accepts[2..4].iter().all(|value| {
        value
            .as_ref()
            .unwrap()
            .to_str()
            .unwrap()
            .split(',')
            .any(|coding| coding.trim() == "gzip")
    }));
    // Explicit negotiation remains unchanged even with decompression disabled.
    assert!(
        accepts[4..]
            .iter()
            .all(|value| value.as_ref().unwrap() == "gzip")
    );
}
