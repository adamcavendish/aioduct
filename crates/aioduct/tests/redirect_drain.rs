#![cfg(feature = "tokio")]

use std::convert::Infallible;
use std::time::Duration;

use bytes::Bytes;
use http_body_util::Full;
use hyper::Response;

use aioduct::HttpEngineSend;
use aioduct::runtime::TokioRuntime;
use aioduct::runtime::tokio_rt::TcpConnector;

use aioduct_test_server::h1::h1_server_with;

/// A 302 redirect response has a 10MB body. The client must drain
/// (discard) the body during redirect processing without OOM or
/// panic. The redirect target returns "done".
#[tokio::test]
async fn redirect_302_with_large_body_drained_and_followed() {
    // Final target server: returns "done"
    let (final_addr, _counter) = h1_server_with(|_req| async move {
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("done"))))
    })
    .await;

    // Redirect server: returns 302 with 10MB body pointing to the target
    let (redirect_addr, _counter) = h1_server_with(move |_req| {
        let target = format!("http://{final_addr}/");
        async move {
            Ok::<_, Infallible>(
                Response::builder()
                    .status(302)
                    .header("location", target)
                    .body(Full::new(Bytes::from(vec![0u8; 10 * 1024 * 1024])))
                    .unwrap(),
            )
        }
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .unwrap();

    let resp = client
        .get(&format!("http://{redirect_addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    let body = resp.text().await.unwrap();
    assert_eq!(body, "done");
}

/// A 3-hop redirect chain where each redirect response has a 1MB
/// body. All bodies must be drained, and the final destination is
/// reached. Verifies the redirect counter works correctly (3
/// redirects followed, max 10 by default).
#[tokio::test]
async fn redirect_chain_each_with_body_all_drained() {
    let (addr, _counter) = h1_server_with(|req| async move {
        let path = req.uri().path().to_string();
        let mb = Bytes::from(vec![0u8; 1024 * 1024]); // 1MB
        if path == "/start" {
            Ok::<_, Infallible>(
                Response::builder()
                    .status(302)
                    .header("Location", "/hop1")
                    .body(Full::new(mb))
                    .unwrap(),
            )
        } else if path == "/hop1" {
            Ok(Response::builder()
                .status(302)
                .header("Location", "/hop2")
                .body(Full::new(mb))
                .unwrap())
        } else if path == "/hop2" {
            Ok(Response::builder()
                .status(302)
                .header("Location", "/final")
                .body(Full::new(mb))
                .unwrap())
        } else {
            Ok(Response::new(Full::new(Bytes::from("final"))))
        }
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .unwrap();

    let resp = client
        .get(&format!("http://{addr}/start"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    let body = resp.text().await.unwrap();
    assert_eq!(body, "final");
}
