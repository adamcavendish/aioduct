use std::convert::Infallible;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use aioduct::HttpEngineSend;
use aioduct::runtime::TokioRuntime;
use aioduct::runtime::tokio_rt::TcpConnector;
use aioduct_test_server::h1::spawn_h1_server_with;
use bytes::Bytes;
use http_body_util::Full;
use hyper::Response;

// ═══════════════════════════════════════════════════════════════════════════
// 2. redirect_cross_origin_strips_authorization
//    Cross-origin redirect (different port) must strip Authorization header.
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(feature = "tokio")]
#[tokio::test]
async fn redirect_cross_origin_strips_authorization_tokio() {
    let captured_auth = Arc::new(Mutex::new(None::<String>));
    let cap = captured_auth.clone();

    // Server B (target): captures Authorization header
    let target_addr = spawn_h1_server_with(move |req| {
        let cap = cap.clone();
        async move {
            let auth = req
                .headers()
                .get("authorization")
                .map(|v| v.to_str().unwrap_or("").to_string());
            *cap.lock().unwrap() = auth;
            Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("target"))))
        }
    });

    // Server A (origin): redirects to Server B
    let origin_addr = spawn_h1_server_with(move |_req| {
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
    });

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();

    let resp = client
        .get(&format!("http://{origin_addr}/start"))
        .unwrap()
        .bearer_auth("secret-token")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), http::StatusCode::OK);
    let _ = resp.text().await.unwrap();

    let auth = captured_auth.lock().unwrap().clone();
    assert!(
        auth.is_none(),
        "Authorization should be stripped on cross-origin redirect, got: {}",
        auth.unwrap_or_default()
    );
}

#[cfg(feature = "smol")]
#[test]
fn redirect_cross_origin_strips_authorization_smol() {
    smol::block_on(async {
        let captured_auth = Arc::new(Mutex::new(None::<String>));
        let cap = captured_auth.clone();

        let target_addr = spawn_h1_server_with(move |req| {
            let cap = cap.clone();
            async move {
                let auth = req
                    .headers()
                    .get("authorization")
                    .map(|v| v.to_str().unwrap_or("").to_string());
                *cap.lock().unwrap() = auth;
                Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("target"))))
            }
        });

        let origin_addr = spawn_h1_server_with(move |_req| {
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
        });

        let client: HttpEngineSend<
            aioduct::runtime::smol_rt::SmolRuntime,
            aioduct::runtime::smol_rt::TcpConnector,
        > = HttpEngineSend::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap();

        let resp = client
            .get(&format!("http://{origin_addr}/start"))
            .unwrap()
            .bearer_auth("secret-token")
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), http::StatusCode::OK);
        let _ = resp.text().await.unwrap();

        let auth = captured_auth.lock().unwrap().clone();
        assert!(
            auth.is_none(),
            "Authorization should be stripped on cross-origin redirect, got: {}",
            auth.unwrap_or_default()
        );
    });
}

#[cfg(feature = "compio")]
#[test]
fn redirect_cross_origin_strips_authorization_compio() {
    compio_runtime::Runtime::new().unwrap().block_on(async {
        let captured_auth = Arc::new(Mutex::new(None::<String>));
        let cap = captured_auth.clone();

        let target_addr = spawn_h1_server_with(move |req| {
            let cap = cap.clone();
            async move {
                let auth = req
                    .headers()
                    .get("authorization")
                    .map(|v| v.to_str().unwrap_or("").to_string());
                *cap.lock().unwrap() = auth;
                Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("target"))))
            }
        });

        let origin_addr = spawn_h1_server_with(move |_req| {
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
        });

        let client: aioduct::HttpEngineLocal<
            aioduct::runtime::compio_rt::CompioRuntime,
            aioduct::runtime::compio_rt::TcpConnector,
        > = aioduct::HttpEngineLocal::builder()
            .timeout(Duration::from_secs(5))
            .build_local()
            .unwrap();

        let resp = client
            .get_local(&format!("http://{origin_addr}/start"))
            .unwrap()
            .bearer_auth("secret-token")
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), http::StatusCode::OK);
        let _ = resp.text().await.unwrap();

        let auth = captured_auth.lock().unwrap().clone();
        assert!(
            auth.is_none(),
            "Authorization should be stripped on cross-origin redirect, got: {}",
            auth.unwrap_or_default()
        );
    });
}

#[cfg(feature = "tokio")]
#[tokio::test]
async fn redirect_cross_origin_strips_header_value_marked_sensitive_tokio() {
    let captured_secret = Arc::new(Mutex::new(None::<String>));
    let cap = captured_secret.clone();

    let target_addr = spawn_h1_server_with(move |req| {
        let cap = cap.clone();
        async move {
            let secret = req
                .headers()
                .get("x-secret")
                .map(|v| v.to_str().unwrap_or("").to_string());
            *cap.lock().unwrap() = secret;
            Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("target"))))
        }
    });

    let origin_addr = spawn_h1_server_with(move |_req| {
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
    });

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let mut secret = http::HeaderValue::from_static("do-not-forward");
    secret.set_sensitive(true);

    let resp = client
        .get(&format!("http://{origin_addr}/start"))
        .unwrap()
        .header(http::HeaderName::from_static("x-secret"), secret)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), http::StatusCode::OK);
    let _ = resp.text().await.unwrap();

    let secret = captured_secret.lock().unwrap().clone();
    assert!(
        secret.is_none(),
        "sensitive HeaderValue should be stripped on cross-origin redirect, got: {}",
        secret.unwrap_or_default()
    );
}

#[cfg(feature = "compio")]
#[test]
fn redirect_cross_origin_strips_header_value_marked_sensitive_compio() {
    compio_runtime::Runtime::new().unwrap().block_on(async {
        let captured_secret = Arc::new(Mutex::new(None::<String>));
        let cap = captured_secret.clone();

        let target_addr = spawn_h1_server_with(move |req| {
            let cap = cap.clone();
            async move {
                let secret = req
                    .headers()
                    .get("x-secret")
                    .map(|v| v.to_str().unwrap_or("").to_string());
                *cap.lock().unwrap() = secret;
                Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("target"))))
            }
        });

        let origin_addr = spawn_h1_server_with(move |_req| {
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
        });

        let client: aioduct::HttpEngineLocal<
            aioduct::runtime::compio_rt::CompioRuntime,
            aioduct::runtime::compio_rt::TcpConnector,
        > = aioduct::HttpEngineLocal::builder()
            .timeout(Duration::from_secs(5))
            .build_local()
            .unwrap();
        let mut secret = http::HeaderValue::from_static("do-not-forward");
        secret.set_sensitive(true);

        let resp = client
            .get_local(&format!("http://{origin_addr}/start"))
            .unwrap()
            .header(http::HeaderName::from_static("x-secret"), secret)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), http::StatusCode::OK);
        let _ = resp.text().await.unwrap();

        let secret = captured_secret.lock().unwrap().clone();
        assert!(
            secret.is_none(),
            "sensitive HeaderValue should be stripped on cross-origin redirect, got: {}",
            secret.unwrap_or_default()
        );
    });
}

// ═══════════════════════════════════════════════════════════════════════════
// 3. redirect_cross_origin_strips_cookie
//    Cross-origin redirect must strip Cookie header.
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(feature = "tokio")]
#[tokio::test]
async fn redirect_cross_origin_strips_cookie_tokio() {
    let captured_cookie = Arc::new(Mutex::new(None::<String>));
    let cap = captured_cookie.clone();

    let target_addr = spawn_h1_server_with(move |req| {
        let cap = cap.clone();
        async move {
            let cookie = req
                .headers()
                .get("cookie")
                .map(|v| v.to_str().unwrap_or("").to_string());
            *cap.lock().unwrap() = cookie;
            Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("target"))))
        }
    });

    let origin_addr = spawn_h1_server_with(move |_req| {
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
    });

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();

    let resp = client
        .get(&format!("http://{origin_addr}/start"))
        .unwrap()
        .header(
            http::header::COOKIE,
            http::header::HeaderValue::from_static("session=abc"),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), http::StatusCode::OK);
    let _ = resp.text().await.unwrap();

    let cookie = captured_cookie.lock().unwrap().clone();
    assert!(
        cookie.is_none(),
        "Cookie should be stripped on cross-origin redirect, got: {}",
        cookie.unwrap_or_default()
    );
}

#[cfg(feature = "smol")]
#[test]
fn redirect_cross_origin_strips_cookie_smol() {
    smol::block_on(async {
        let captured_cookie = Arc::new(Mutex::new(None::<String>));
        let cap = captured_cookie.clone();

        let target_addr = spawn_h1_server_with(move |req| {
            let cap = cap.clone();
            async move {
                let cookie = req
                    .headers()
                    .get("cookie")
                    .map(|v| v.to_str().unwrap_or("").to_string());
                *cap.lock().unwrap() = cookie;
                Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("target"))))
            }
        });

        let origin_addr = spawn_h1_server_with(move |_req| {
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
        });

        let client: HttpEngineSend<
            aioduct::runtime::smol_rt::SmolRuntime,
            aioduct::runtime::smol_rt::TcpConnector,
        > = HttpEngineSend::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap();

        let resp = client
            .get(&format!("http://{origin_addr}/start"))
            .unwrap()
            .header(
                http::header::COOKIE,
                http::header::HeaderValue::from_static("session=abc"),
            )
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), http::StatusCode::OK);
        let _ = resp.text().await.unwrap();

        let cookie = captured_cookie.lock().unwrap().clone();
        assert!(
            cookie.is_none(),
            "Cookie should be stripped on cross-origin redirect, got: {}",
            cookie.unwrap_or_default()
        );
    });
}

#[cfg(feature = "compio")]
#[test]
fn redirect_cross_origin_strips_cookie_compio() {
    compio_runtime::Runtime::new().unwrap().block_on(async {
        let captured_cookie = Arc::new(Mutex::new(None::<String>));
        let cap = captured_cookie.clone();

        let target_addr = spawn_h1_server_with(move |req| {
            let cap = cap.clone();
            async move {
                let cookie = req
                    .headers()
                    .get("cookie")
                    .map(|v| v.to_str().unwrap_or("").to_string());
                *cap.lock().unwrap() = cookie;
                Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("target"))))
            }
        });

        let origin_addr = spawn_h1_server_with(move |_req| {
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
        });

        let client: aioduct::HttpEngineLocal<
            aioduct::runtime::compio_rt::CompioRuntime,
            aioduct::runtime::compio_rt::TcpConnector,
        > = aioduct::HttpEngineLocal::builder()
            .timeout(Duration::from_secs(5))
            .build_local()
            .unwrap();

        let resp = client
            .get_local(&format!("http://{origin_addr}/start"))
            .unwrap()
            .header(
                http::header::COOKIE,
                http::header::HeaderValue::from_static("session=abc"),
            )
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), http::StatusCode::OK);
        let _ = resp.text().await.unwrap();

        let cookie = captured_cookie.lock().unwrap().clone();
        assert!(
            cookie.is_none(),
            "Cookie should be stripped on cross-origin redirect, got: {}",
            cookie.unwrap_or_default()
        );
    });
}

// ═══════════════════════════════════════════════════════════════════════════
// 4. redirect_cross_origin_strips_proxy_authorization
//    Cross-origin redirect must strip Proxy-Authorization header.
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(feature = "tokio")]
#[tokio::test]
async fn redirect_cross_origin_strips_proxy_authorization_tokio() {
    let captured_proxy_auth = Arc::new(Mutex::new(None::<String>));
    let cap = captured_proxy_auth.clone();

    let target_addr = spawn_h1_server_with(move |req| {
        let cap = cap.clone();
        async move {
            let pa = req
                .headers()
                .get("proxy-authorization")
                .map(|v| v.to_str().unwrap_or("").to_string());
            *cap.lock().unwrap() = pa;
            Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("target"))))
        }
    });

    let origin_addr = spawn_h1_server_with(move |_req| {
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
    });

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();

    let resp = client
        .get(&format!("http://{origin_addr}/start"))
        .unwrap()
        .header(
            http::header::PROXY_AUTHORIZATION,
            http::header::HeaderValue::from_static("Basic dXNlcjpwYXNz"),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), http::StatusCode::OK);
    let _ = resp.text().await.unwrap();

    let pa = captured_proxy_auth.lock().unwrap().clone();
    assert!(
        pa.is_none(),
        "Proxy-Authorization should be stripped on cross-origin redirect, got: {}",
        pa.unwrap_or_default()
    );
}

#[cfg(feature = "smol")]
#[test]
fn redirect_cross_origin_strips_proxy_authorization_smol() {
    smol::block_on(async {
        let captured_proxy_auth = Arc::new(Mutex::new(None::<String>));
        let cap = captured_proxy_auth.clone();

        let target_addr = spawn_h1_server_with(move |req| {
            let cap = cap.clone();
            async move {
                let pa = req
                    .headers()
                    .get("proxy-authorization")
                    .map(|v| v.to_str().unwrap_or("").to_string());
                *cap.lock().unwrap() = pa;
                Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("target"))))
            }
        });

        let origin_addr = spawn_h1_server_with(move |_req| {
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
        });

        let client: HttpEngineSend<
            aioduct::runtime::smol_rt::SmolRuntime,
            aioduct::runtime::smol_rt::TcpConnector,
        > = HttpEngineSend::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap();

        let resp = client
            .get(&format!("http://{origin_addr}/start"))
            .unwrap()
            .header(
                http::header::PROXY_AUTHORIZATION,
                http::header::HeaderValue::from_static("Basic dXNlcjpwYXNz"),
            )
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), http::StatusCode::OK);
        let _ = resp.text().await.unwrap();

        let pa = captured_proxy_auth.lock().unwrap().clone();
        assert!(
            pa.is_none(),
            "Proxy-Authorization should be stripped on cross-origin redirect, got: {}",
            pa.unwrap_or_default()
        );
    });
}

#[cfg(feature = "compio")]
#[test]
fn redirect_cross_origin_strips_proxy_authorization_compio() {
    compio_runtime::Runtime::new().unwrap().block_on(async {
        let captured_proxy_auth = Arc::new(Mutex::new(None::<String>));
        let cap = captured_proxy_auth.clone();

        let target_addr = spawn_h1_server_with(move |req| {
            let cap = cap.clone();
            async move {
                let pa = req
                    .headers()
                    .get("proxy-authorization")
                    .map(|v| v.to_str().unwrap_or("").to_string());
                *cap.lock().unwrap() = pa;
                Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("target"))))
            }
        });

        let origin_addr = spawn_h1_server_with(move |_req| {
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
        });

        let client: aioduct::HttpEngineLocal<
            aioduct::runtime::compio_rt::CompioRuntime,
            aioduct::runtime::compio_rt::TcpConnector,
        > = aioduct::HttpEngineLocal::builder()
            .timeout(Duration::from_secs(5))
            .build_local()
            .unwrap();

        let resp = client
            .get_local(&format!("http://{origin_addr}/start"))
            .unwrap()
            .header(
                http::header::PROXY_AUTHORIZATION,
                http::header::HeaderValue::from_static("Basic dXNlcjpwYXNz"),
            )
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), http::StatusCode::OK);
        let _ = resp.text().await.unwrap();

        let pa = captured_proxy_auth.lock().unwrap().clone();
        assert!(
            pa.is_none(),
            "Proxy-Authorization should be stripped on cross-origin redirect, got: {}",
            pa.unwrap_or_default()
        );
    });
}

// ═══════════════════════════════════════════════════════════════════════════
// 5. redirect_cross_origin_preserves_custom_header_when_not_registered
//    Custom headers not registered as sensitive should survive cross-origin
//    redirects. Only Authorization, Cookie, Proxy-Authorization, and
//    explicitly registered sensitive headers are stripped.
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(feature = "tokio")]
#[tokio::test]
async fn redirect_cross_origin_preserves_custom_header_when_not_registered_tokio() {
    let captured_custom = Arc::new(Mutex::new(None::<String>));
    let cap = captured_custom.clone();

    let target_addr = spawn_h1_server_with(move |req| {
        let cap = cap.clone();
        async move {
            let custom = req
                .headers()
                .get("x-custom-header")
                .map(|v| v.to_str().unwrap_or("").to_string());
            *cap.lock().unwrap() = custom;
            Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("target"))))
        }
    });

    let origin_addr = spawn_h1_server_with(move |_req| {
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
    });

    // NOT registering x-custom-header as sensitive — it should be forwarded
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();

    let resp = client
        .get(&format!("http://{origin_addr}/start"))
        .unwrap()
        .header(
            http::header::HeaderName::from_static("x-custom-header"),
            http::header::HeaderValue::from_static("custom-value"),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), http::StatusCode::OK);
    let _ = resp.text().await.unwrap();

    let custom = captured_custom.lock().unwrap().clone();
    assert_eq!(
        custom.as_deref(),
        Some("custom-value"),
        "Custom headers (not registered as sensitive) should be forwarded \
         across cross-origin redirects"
    );
}
