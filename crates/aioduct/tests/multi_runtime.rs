//! Multi-runtime tests: each test body is stamped out for tokio + smol.
//!
//! The server always runs on a background tokio thread, so `tokio` feature is
//! required. The macro additionally stamps out `_smol` variants when `smol` is
//! enabled.
#![cfg(feature = "tokio")]

#[macro_use]
mod common;

use common::multi_runtime::{spawn_server, spawn_server_with};

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
        let client = new_client_builder().default_headers(headers).build();
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
        let client = new_client_builder().default_headers(headers).build();
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

        let auth_seen = Arc::new(AtomicBool::new(false));
        let auth_seen_clone = auth_seen.clone();

        let proxy_addr = spawn_server_with(move |req| {
            let auth_seen = auth_seen_clone.clone();
            async move {
                if let Some(auth) = req.headers().get("proxy-authorization")
                    && auth.to_str().unwrap_or("") == "Basic dXNlcjpwYXNz"
                {
                    auth_seen.store(true, Ordering::SeqCst);
                }
                Ok::<_, std::convert::Infallible>(hyper::Response::new(
                    http_body_util::Full::new(bytes::Bytes::from("proxied")),
                ))
            }
        });

        let client = new_client_builder()
            .proxy(
                aioduct::ProxyConfig::http(&format!("http://{proxy_addr}"))
                    .unwrap()
                    .basic_auth("user", "pass"),
            )
            .build();

        let resp = client
            .get("http://example.com/test")
            .unwrap()
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), http::StatusCode::OK);
        assert!(
            auth_seen.load(Ordering::SeqCst),
            "proxy should receive Proxy-Authorization header for plain HTTP"
        );
    }

    async fn test_connection_refused() {
        let client = new_client();
        let result = client.get("http://127.0.0.1:1/").unwrap().send().await;
        assert!(result.is_err());
    }

    async fn test_https_only_rejects_http() {
        let client = new_client_builder().https_only(true).build();
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
            .build();
        let resp = client
            .get(&format!("http://{addr}/"))
            .unwrap()
            .send()
            .await
            .unwrap();
        assert_eq!(resp.text().await.unwrap(), "aioduct-multi-rt/1.0");
    }
}
