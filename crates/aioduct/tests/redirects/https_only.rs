use std::convert::Infallible;
use std::time::Duration;

use aioduct::HttpEngineSend;
use aioduct::runtime::TokioRuntime;
use aioduct::runtime::tokio_rt::TcpConnector;
use aioduct_test_server::h1::spawn_h1_server_with;
use bytes::Bytes;
use http_body_util::Full;
use hyper::Response;

// ═══════════════════════════════════════════════════════════════════════════
// 1. https_only_blocks_http_redirect_target
//    Client with https_only(true) must reject HTTP requests, including
//    redirect targets that resolve to HTTP.
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(feature = "tokio")]
#[tokio::test]
async fn https_only_blocks_http_redirect_target_tokio() {
    let _addr = spawn_h1_server_with(|_req| async move {
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("ok"))))
    });

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .https_only(true)
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();

    let result = client
        .get(&format!("http://{_addr}/"))
        .unwrap()
        .send()
        .await;
    assert!(result.is_err(), "https_only should block HTTP requests");
    let err_str = format!("{}", result.unwrap_err());
    assert!(
        err_str.to_lowercase().contains("https"),
        "error should mention HTTPS requirement, got: {err_str}"
    );
}

#[cfg(feature = "smol")]
#[test]
fn https_only_blocks_http_redirect_target_smol() {
    smol::block_on(async {
        let _addr = spawn_h1_server_with(|_req| async move {
            Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("ok"))))
        });

        let client = HttpEngineSend::<
            aioduct::runtime::smol_rt::SmolRuntime,
            aioduct::runtime::smol_rt::TcpConnector,
        >::builder()
        .https_only(true)
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();

        let result = client
            .get(&format!("http://{_addr}/"))
            .unwrap()
            .send()
            .await;
        assert!(result.is_err(), "https_only should block HTTP requests");
        let err_str = format!("{}", result.unwrap_err());
        assert!(
            err_str.to_lowercase().contains("https"),
            "error should mention HTTPS requirement, got: {err_str}"
        );
    });
}

#[cfg(feature = "compio")]
#[test]
fn https_only_blocks_http_redirect_target_compio() {
    compio_runtime::Runtime::new().unwrap().block_on(async {
        let _addr = spawn_h1_server_with(|_req| async move {
            Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("ok"))))
        });

        let client: aioduct::HttpEngineLocal<
            aioduct::runtime::compio_rt::CompioRuntime,
            aioduct::runtime::compio_rt::TcpConnector,
        > = aioduct::HttpEngineLocal::builder()
            .https_only(true)
            .timeout(Duration::from_secs(5))
            .build_local()
            .unwrap();

        let result = client
            .get_local(&format!("http://{_addr}/"))
            .unwrap()
            .send()
            .await;
        assert!(result.is_err(), "https_only should block HTTP requests");
        let err_str = format!("{}", result.unwrap_err());
        assert!(
            err_str.to_lowercase().contains("https"),
            "error should mention HTTPS requirement, got: {err_str}"
        );
    });
}
