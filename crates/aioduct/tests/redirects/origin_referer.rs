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
// 6. redirect_same_origin_different_port_preserves_auth
//    Same host, different port is cross-origin (port is part of origin per
//    RFC 6454). This test verifies that when redirecting to the SAME host
//    and port, Authorization IS preserved because the same-origin check
//    correctly includes port.
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(feature = "tokio")]
#[tokio::test]
async fn redirect_same_origin_different_port_preserves_auth_tokio() {
    use std::sync::atomic::{AtomicBool, Ordering};

    let auth_preserved = Arc::new(AtomicBool::new(false));
    let ap = auth_preserved.clone();

    // Single server that redirects to itself (same host:port) but a
    // different path. Auth should be preserved because it's same-origin.
    let addr = spawn_h1_server_with(move |req| {
        let ap = ap.clone();
        async move {
            if req.uri().path() == "/step1" {
                // Redirect to same server, explicit host:port in Location
                let host = req
                    .headers()
                    .get("host")
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .to_string();
                Ok::<_, Infallible>(
                    Response::builder()
                        .status(302)
                        .header("location", format!("http://{host}/step2"))
                        .body(Full::new(Bytes::new()))
                        .unwrap(),
                )
            } else {
                let has_auth = req.headers().get("authorization").is_some();
                ap.store(has_auth, Ordering::SeqCst);
                Ok(Response::new(Full::new(Bytes::from("final"))))
            }
        }
    });

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();

    let resp = client
        .get(&format!("http://{addr}/step1"))
        .unwrap()
        .bearer_auth("my-token")
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    assert!(
        auth_preserved.load(Ordering::SeqCst),
        "Same-origin redirect (same host:port) should preserve Authorization"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// 7. redirect_referer_set_on_http_to_http_cross_origin
//    With referer(true), Referer IS sent on a same-scheme HTTP→HTTP
//    cross-origin redirect (matching curl's default). The HTTPS→HTTP
//    downgrade suppression required by RFC 7231 §5.5.2 is covered by the
//    real TLS-backed test `referer_not_leaked_on_https_to_http_downgrade`
//    in tests/tls.rs.
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(feature = "tokio")]
#[tokio::test]
async fn redirect_referer_set_on_http_to_http_cross_origin() {
    let captured_referer = Arc::new(Mutex::new(None::<String>));
    let cap = captured_referer.clone();

    // Server B (target): captures Referer header
    let target_addr = spawn_h1_server_with(move |req| {
        let cap = cap.clone();
        async move {
            let referer = req
                .headers()
                .get("referer")
                .map(|v| v.to_str().unwrap_or("").to_string());
            *cap.lock().unwrap() = referer;
            Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("target"))))
        }
    });

    // Server A (origin): redirects to Server B on a different port
    let origin_addr = spawn_h1_server_with(move |_req| {
        let target = format!("http://127.0.0.1:{}/final", target_addr.port());
        async move {
            Ok::<_, Infallible>(
                Response::builder()
                    .status(302)
                    .header("location", target)
                    .body(Full::new(Bytes::new()))
                    .unwrap(),
            )
        }
    });

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .referer(true)
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();

    // HTTP→HTTP cross-origin redirect: Referer IS set (curl default behavior).
    let resp = client
        .get(&format!("http://{origin_addr}/secret-path"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), http::StatusCode::OK);
    let _ = resp.text().await.unwrap();

    let referer = captured_referer.lock().unwrap().clone();
    assert!(
        referer.is_some(),
        "referer(true) should set Referer on a same-scheme HTTP→HTTP redirect"
    );
}
