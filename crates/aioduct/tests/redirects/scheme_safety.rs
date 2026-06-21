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
// 8. redirect_rejects_data_scheme_location
//    Redirect to data: URIs must be rejected as a security measure.
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(feature = "tokio")]
#[tokio::test]
async fn redirect_rejects_data_scheme_location_tokio() {
    let addr = spawn_h1_server_with(|_req| async move {
        Ok::<_, Infallible>(
            Response::builder()
                .status(302)
                .header("location", "data:text/html,<h1>pwned</h1>")
                .body(Full::new(Bytes::new()))
                .unwrap(),
        )
    });

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();

    let result = client.get(&format!("http://{addr}/")).unwrap().send().await;
    assert!(
        result.is_err(),
        "redirect to data: scheme should be rejected as a security measure"
    );
}

#[cfg(feature = "smol")]
#[test]
fn redirect_rejects_data_scheme_location_smol() {
    smol::block_on(async {
        let addr = spawn_h1_server_with(|_req| async move {
            Ok::<_, Infallible>(
                Response::builder()
                    .status(302)
                    .header("location", "data:text/html,<h1>pwned</h1>")
                    .body(Full::new(Bytes::new()))
                    .unwrap(),
            )
        });

        let client: HttpEngineSend<
            aioduct::runtime::smol_rt::SmolRuntime,
            aioduct::runtime::smol_rt::TcpConnector,
        > = HttpEngineSend::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap();

        let result = client.get(&format!("http://{addr}/")).unwrap().send().await;
        assert!(
            result.is_err(),
            "redirect to data: scheme should be rejected as a security measure"
        );
    });
}

#[cfg(feature = "compio")]
#[test]
fn redirect_rejects_data_scheme_location_compio() {
    compio_runtime::Runtime::new().unwrap().block_on(async {
        let addr = spawn_h1_server_with(|_req| async move {
            Ok::<_, Infallible>(
                Response::builder()
                    .status(302)
                    .header("location", "data:text/html,<h1>pwned</h1>")
                    .body(Full::new(Bytes::new()))
                    .unwrap(),
            )
        });

        let client: aioduct::HttpEngineLocal<
            aioduct::runtime::compio_rt::CompioRuntime,
            aioduct::runtime::compio_rt::TcpConnector,
        > = aioduct::HttpEngineLocal::builder()
            .timeout(Duration::from_secs(5))
            .build_local()
            .unwrap();

        let result = client
            .get_local(&format!("http://{addr}/"))
            .unwrap()
            .send()
            .await;
        assert!(
            result.is_err(),
            "redirect to data: scheme should be rejected as a security measure"
        );
    });
}

// ═══════════════════════════════════════════════════════════════════════════
// 9. redirect_rejects_javascript_scheme_location
//    Redirect to javascript: URIs must be rejected as a security measure.
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(feature = "tokio")]
#[tokio::test]
async fn redirect_rejects_javascript_scheme_location_tokio() {
    let addr = spawn_h1_server_with(|_req| async move {
        Ok::<_, Infallible>(
            Response::builder()
                .status(302)
                .header("location", "javascript:alert(1)")
                .body(Full::new(Bytes::new()))
                .unwrap(),
        )
    });

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();

    let result = client.get(&format!("http://{addr}/")).unwrap().send().await;
    assert!(
        result.is_err(),
        "redirect to javascript: scheme should be rejected as a security measure"
    );
}

#[cfg(feature = "smol")]
#[test]
fn redirect_rejects_javascript_scheme_location_smol() {
    smol::block_on(async {
        let addr = spawn_h1_server_with(|_req| async move {
            Ok::<_, Infallible>(
                Response::builder()
                    .status(302)
                    .header("location", "javascript:alert(1)")
                    .body(Full::new(Bytes::new()))
                    .unwrap(),
            )
        });

        let client: HttpEngineSend<
            aioduct::runtime::smol_rt::SmolRuntime,
            aioduct::runtime::smol_rt::TcpConnector,
        > = HttpEngineSend::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap();

        let result = client.get(&format!("http://{addr}/")).unwrap().send().await;
        assert!(
            result.is_err(),
            "redirect to javascript: scheme should be rejected as a security measure"
        );
    });
}

#[cfg(feature = "compio")]
#[test]
fn redirect_rejects_javascript_scheme_location_compio() {
    compio_runtime::Runtime::new().unwrap().block_on(async {
        let addr = spawn_h1_server_with(|_req| async move {
            Ok::<_, Infallible>(
                Response::builder()
                    .status(302)
                    .header("location", "javascript:alert(1)")
                    .body(Full::new(Bytes::new()))
                    .unwrap(),
            )
        });

        let client: aioduct::HttpEngineLocal<
            aioduct::runtime::compio_rt::CompioRuntime,
            aioduct::runtime::compio_rt::TcpConnector,
        > = aioduct::HttpEngineLocal::builder()
            .timeout(Duration::from_secs(5))
            .build_local()
            .unwrap();

        let result = client
            .get_local(&format!("http://{addr}/"))
            .unwrap()
            .send()
            .await;
        assert!(
            result.is_err(),
            "redirect to javascript: scheme should be rejected as a security measure"
        );
    });
}

// ═══════════════════════════════════════════════════════════════════════════
// 10. redirect_rejects_non_http_scheme_location
//     A redirect to a non-http(s) absolute target (e.g. ftp://) must be
//     rejected, not dispatched as cleartext HTTP to port 80.
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(feature = "tokio")]
#[tokio::test]
async fn redirect_rejects_ftp_scheme_location_tokio() {
    let addr = spawn_h1_server_with(|_req| async move {
        Ok::<_, Infallible>(
            Response::builder()
                .status(302)
                .header("location", "ftp://evil.example.com/path")
                .body(Full::new(Bytes::new()))
                .unwrap(),
        )
    });

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();

    let result = client.get(&format!("http://{addr}/")).unwrap().send().await;
    let err = result.expect_err("redirect to ftp: scheme should be rejected");
    assert!(
        err.is_redirect(),
        "expected a redirect error for the ftp:// target, got: {err:?}"
    );
}

#[cfg(feature = "compio")]
#[test]
fn redirect_rejects_ftp_scheme_location_compio() {
    compio_runtime::Runtime::new().unwrap().block_on(async {
        let addr = spawn_h1_server_with(|_req| async move {
            Ok::<_, Infallible>(
                Response::builder()
                    .status(302)
                    .header("location", "ftp://evil.example.com/path")
                    .body(Full::new(Bytes::new()))
                    .unwrap(),
            )
        });

        let client: aioduct::HttpEngineLocal<
            aioduct::runtime::compio_rt::CompioRuntime,
            aioduct::runtime::compio_rt::TcpConnector,
        > = aioduct::HttpEngineLocal::builder()
            .timeout(Duration::from_secs(5))
            .build_local()
            .unwrap();

        let result = client
            .get_local(&format!("http://{addr}/"))
            .unwrap()
            .send()
            .await;
        let err = result.expect_err("redirect to ftp: scheme should be rejected");
        assert!(
            err.is_redirect(),
            "expected a redirect error for the ftp:// target, got: {err:?}"
        );
    });
}
