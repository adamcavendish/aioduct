//! Multi-runtime tests: each test body is stamped out for tokio + smol.
//!
//! The server always runs on a background tokio thread, so `tokio` feature is
//! required. The macro additionally stamps out `_smol` variants when `smol` is
//! enabled.
#![cfg(feature = "tokio")]

use aioduct_test_server::h1::{spawn_h1_server, spawn_h1_server_with};

/// Stamps out a test function for each supported runtime.
///
/// Inside the test body:
/// - `new_client()` creates a default `HttpEngineSend` for the current runtime
/// - `new_client_builder()` returns an `HttpEngineBuilder` for the current runtime
/// - `spawn_server()` / `spawn_server_with(handler)` start a test server
///   (these are runtime-agnostic, from the `aioduct_test_server` crate)
macro_rules! runtime_test {
    (
        $(
            $(#[$meta:meta])*
            async fn $name:ident() $body:block
        )*
    ) => {
        $(
            #[cfg(feature = "tokio")]
            paste::paste! {
                #[tokio::test]
                $(#[$meta])*
                async fn [<$name _tokio>]() {
                    #[allow(unused)]
                    fn new_client() -> aioduct::HttpEngineSend<
                        aioduct::runtime::TokioRuntime,
                        aioduct::runtime::tokio_rt::TcpConnector,
                    > {
                        aioduct::HttpEngineSend::new()
                    }

                    #[allow(unused)]
                    fn new_client_builder() -> aioduct::HttpEngineBuilder<
                        aioduct::runtime::TokioRuntime,
                        aioduct::runtime::tokio_rt::TcpConnector,
                    > {
                        aioduct::HttpEngineSend::builder()
                    }

                    #[allow(unused)]
                    fn spawn_server() -> std::net::SocketAddr {
                        spawn_h1_server()
                    }

                    #[allow(unused)]
                    fn spawn_server_with<F, Fut>(handler: F) -> std::net::SocketAddr
                    where
                        F: Fn(hyper::Request<hyper::body::Incoming>) -> Fut + Send + Clone + 'static,
                        Fut: std::future::Future<
                                Output = Result<
                                    hyper::Response<http_body_util::Full<bytes::Bytes>>,
                                    std::convert::Infallible,
                                >,
                            > + Send,
                    {
                        spawn_h1_server_with(handler)
                    }

                    $body
                }
            }

            #[cfg(feature = "smol")]
            paste::paste! {
                #[test]
                $(#[$meta])*
                fn [<$name _smol>]() {
                    smol::block_on(async {
                        #[allow(unused)]
                        fn new_client() -> aioduct::HttpEngineSend<
                            aioduct::runtime::smol_rt::SmolRuntime,
                            aioduct::runtime::smol_rt::TcpConnector,
                        > {
                            aioduct::HttpEngineSend::new()
                        }

                        #[allow(unused)]
                        fn new_client_builder() -> aioduct::HttpEngineBuilder<
                            aioduct::runtime::smol_rt::SmolRuntime,
                            aioduct::runtime::smol_rt::TcpConnector,
                        > {
                            aioduct::HttpEngineSend::builder()
                        }

                        #[allow(unused)]
                        fn spawn_server() -> std::net::SocketAddr {
                            spawn_h1_server()
                        }

                        #[allow(unused)]
                        fn spawn_server_with<F, Fut>(handler: F) -> std::net::SocketAddr
                        where
                            F: Fn(hyper::Request<hyper::body::Incoming>) -> Fut + Send + Clone + 'static,
                            Fut: std::future::Future<
                                    Output = Result<
                                        hyper::Response<http_body_util::Full<bytes::Bytes>>,
                                        std::convert::Infallible,
                                    >,
                                > + Send,
                        {
                            spawn_h1_server_with(handler)
                        }

                        $body
                    });
                }
            }
        )*
    };
}

runtime_test! {
    async fn test_basic_get() {
        let addr = spawn_server();
        let client = new_client();
        let resp = client
            .get(&format!("http://{addr}/"))
            .unwrap()
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), http::StatusCode::OK);
        assert_eq!(resp.text().await.unwrap(), "hello aioduct");
    }

    async fn test_post_echo() {
        let addr = spawn_server_with(|req| async move {
            use http_body_util::BodyExt;
            let body = req.collect().await.unwrap().to_bytes();
            Ok::<_, std::convert::Infallible>(hyper::Response::new(
                http_body_util::Full::new(body),
            ))
        });
        let client = new_client();
        let resp = client
            .post(&format!("http://{addr}/"))
            .unwrap()
            .body("echo me")
            .send()
            .await
            .unwrap();
        assert_eq!(resp.text().await.unwrap(), "echo me");
    }

    async fn test_default_headers() {
        let addr = spawn_server_with(|req| async move {
            let val = req
                .headers()
                .get("x-custom")
                .map(|v| v.to_str().unwrap().to_owned())
                .unwrap_or_default();
            Ok::<_, std::convert::Infallible>(hyper::Response::new(
                http_body_util::Full::new(bytes::Bytes::from(val)),
            ))
        });

        let mut headers = http::HeaderMap::new();
        headers.insert(
            http::header::HeaderName::from_static("x-custom"),
            http::header::HeaderValue::from_static("default-val"),
        );
        let client = new_client_builder().default_headers(headers).build().unwrap();
        let resp = client
            .get(&format!("http://{addr}/"))
            .unwrap()
            .send()
            .await
            .unwrap();
        assert_eq!(resp.text().await.unwrap(), "default-val");
    }

    async fn test_request_header_overrides_default() {
        let addr = spawn_server_with(|req| async move {
            let val = req
                .headers()
                .get("authorization")
                .map(|v| v.to_str().unwrap().to_owned())
                .unwrap_or_default();
            Ok::<_, std::convert::Infallible>(hyper::Response::new(
                http_body_util::Full::new(bytes::Bytes::from(val)),
            ))
        });

        let mut headers = http::HeaderMap::new();
        headers.insert(
            http::header::AUTHORIZATION,
            http::header::HeaderValue::from_static("default-token"),
        );
        let client = new_client_builder().default_headers(headers).build().unwrap();
        let resp = client
            .get(&format!("http://{addr}/"))
            .unwrap()
            .header(
                http::header::AUTHORIZATION,
                http::header::HeaderValue::from_static("override-token"),
            )
            .send()
            .await
            .unwrap();
        assert_eq!(resp.text().await.unwrap(), "override-token");
    }

    async fn test_redirect_follows() {
        let final_addr = spawn_server();
        let redirect_addr = spawn_server_with(move |_req| {
            let target = format!("http://{final_addr}/");
            async move {
                Ok::<_, std::convert::Infallible>(
                    hyper::Response::builder()
                        .status(302)
                        .header("location", target)
                        .body(http_body_util::Full::new(bytes::Bytes::new()))
                        .unwrap(),
                )
            }
        });

        let client = new_client();
        let resp = client
            .get(&format!("http://{redirect_addr}/start"))
            .unwrap()
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), http::StatusCode::OK);
        assert_eq!(resp.text().await.unwrap(), "hello aioduct");
    }

    async fn test_proxy_auth_injected_for_plain_http() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::mpsc;

        let auth_seen = Arc::new(AtomicBool::new(false));
        let auth_seen_clone = auth_seen.clone();

        // Start a real HTTP target server (runs on background tokio thread via spawn_h1_server)
        let target_addr = spawn_h1_server();

        // Build a CONNECT proxy on a background tokio thread
        let (tx, rx) = mpsc::channel();
        let auth = auth_seen_clone;
        std::thread::spawn(move || {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async move {
                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                tx.send(listener.local_addr().unwrap()).unwrap();
                loop {
                    let (mut client, _) = listener.accept().await.unwrap();
                    let auth = auth.clone();
                    tokio::spawn(async move {
                        let mut buf = [0u8; 4096];
                        let n = client.read(&mut buf).await.unwrap();
                        let req_str = String::from_utf8_lossy(&buf[..n]);
                        if !req_str.starts_with("CONNECT") {
                            return;
                        }
                        if req_str.contains("proxy-authorization:")
                            || req_str.contains("Proxy-Authorization:")
                        {
                            auth.store(true, Ordering::SeqCst);
                        }
                        let target = req_str.split_whitespace().nth(1).unwrap_or("");
                        let _ = client
                            .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                            .await;
                        let mut upstream = match tokio::net::TcpStream::connect(target).await {
                            Ok(s) => s,
                            Err(_) => return,
                        };
                        let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
                    });
                }
            });
        });
        let proxy_addr = rx.recv().unwrap();

        let client = new_client_builder()
            .proxy(
                aioduct::ProxyConfig::http(&format!("http://{proxy_addr}"))
                    .unwrap()
                    .basic_auth("user", "pass"),
            )
            .build().unwrap();

        let resp = client
            .get(&format!("http://{target_addr}/test"))
            .unwrap()
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), http::StatusCode::OK);
        assert!(
            auth_seen.load(Ordering::SeqCst),
            "CONNECT request should include Proxy-Authorization header"
        );
    }

    async fn test_connection_refused() {
        let client = new_client();
        let result = client.get("http://127.0.0.1:1/").unwrap().send().await;
        assert!(result.is_err());
    }

    async fn test_https_only_rejects_http() {
        let client = new_client_builder().https_only(true).build().unwrap();
        let result = client.get("http://example.com/").unwrap().send().await;
        assert!(result.is_err());
    }

    async fn test_error_for_status() {
        let addr = spawn_server_with(|_req| async move {
            Ok::<_, std::convert::Infallible>(
                hyper::Response::builder()
                    .status(404)
                    .body(http_body_util::Full::new(bytes::Bytes::from("not found")))
                    .unwrap(),
            )
        });

        let client = new_client();
        let resp = client
            .get(&format!("http://{addr}/missing"))
            .unwrap()
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), http::StatusCode::NOT_FOUND);
        let result = resp.error_for_status();
        assert!(result.is_err());
    }

    async fn test_head_request() {
        let addr = spawn_server();
        let client = new_client();
        let resp = client
            .head(&format!("http://{addr}/"))
            .unwrap()
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), http::StatusCode::OK);
    }

    async fn test_remote_addr() {
        let addr = spawn_server();
        let client = new_client();
        let resp = client
            .get(&format!("http://{addr}/"))
            .unwrap()
            .send()
            .await
            .unwrap();
        let remote = resp.remote_addr();
        assert!(remote.is_some());
        assert_eq!(remote.unwrap().port(), addr.port());
    }

    async fn test_content_length() {
        let addr = spawn_server_with(|_req| async move {
            Ok::<_, std::convert::Infallible>(hyper::Response::new(
                http_body_util::Full::new(bytes::Bytes::from("x".repeat(42))),
            ))
        });

        let client = new_client();
        let resp = client
            .get(&format!("http://{addr}/"))
            .unwrap()
            .send()
            .await
            .unwrap();
        assert_eq!(resp.content_length(), Some(42));
    }

    async fn test_user_agent_builder() {
        let addr = spawn_server_with(|req| async move {
            let ua = req
                .headers()
                .get("user-agent")
                .map(|v| v.to_str().unwrap().to_owned())
                .unwrap_or_default();
            Ok::<_, std::convert::Infallible>(hyper::Response::new(
                http_body_util::Full::new(bytes::Bytes::from(ua)),
            ))
        });

        let client = new_client_builder()
            .user_agent("aioduct-multi-rt/1.0")
            .build().unwrap();
        let resp = client
            .get(&format!("http://{addr}/"))
            .unwrap()
            .send()
            .await
            .unwrap();
        assert_eq!(resp.text().await.unwrap(), "aioduct-multi-rt/1.0");
    }
}
