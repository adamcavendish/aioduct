use std::convert::Infallible;
use std::time::Duration;

use aioduct::HttpEngineSend;
use aioduct::runtime::TokioRuntime;
use aioduct::runtime::tokio_rt::TcpConnector;
use aioduct_test_server::h1::h1_server_with;
use bytes::Bytes;
use http_body_util::Full;
use hyper::Response;

// Redirect Set-Cookie headers should be stored and applied to the redirected request.
#[tokio::test]
async fn redirect_stores_cookies_from_redirect_response() {
    let (addr, _) = h1_server_with(|req| async move {
        let path = req.uri().path().to_string();
        if path == "/login" {
            let resp = Response::builder()
                .status(302)
                .header("Location", "/dashboard")
                .header("Set-Cookie", "session=xyz789; Path=/")
                .body(Full::new(Bytes::new()))
                .unwrap();
            Ok::<_, Infallible>(resp)
        } else {
            let cookie = req
                .headers()
                .get("cookie")
                .map(|v| v.to_str().unwrap_or("").to_string())
                .unwrap_or_default();
            Ok(Response::new(Full::new(Bytes::from(format!(
                "cookie={cookie}"
            )))))
        }
    })
    .await;

    let jar = aioduct::cookie::CookieJar::new();
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .cookie_jar(jar)
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();

    let resp = client
        .get(&format!("http://{addr}/login"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let body = resp.text().await.unwrap();
    assert!(
        body.contains("session=xyz789"),
        "redirect response Set-Cookie should be stored and applied to the \
         redirected request, got: {body}"
    );
}
