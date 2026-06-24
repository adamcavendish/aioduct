use super::*;

#[tokio::test]
async fn redirect_should_preserve_fragment_when_location_has_none() {
    let (addr, _) = h1_server_with(|req| async move {
        let path = req.uri().path().to_string();
        let query = req.uri().query().unwrap_or("");
        if path == "/page" {
            Ok::<_, Infallible>(
                Response::builder()
                    .status(302)
                    .header("Location", "/target")
                    .body(Full::new(Bytes::new()))
                    .unwrap(),
            )
        } else {
            Ok(Response::new(Full::new(Bytes::from(format!(
                "path={path} query={query}"
            )))))
        }
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();

    // Request with a fragment: the fragment #section1 should be inherited
    // by the redirect target since Location has no fragment of its own.
    let resp = client
        .get(&format!("http://{addr}/page#section1"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    // Fragments are client-side (not part of http::Uri), so verify via
    // Response::fragment() rather than url().to_string().
    assert_eq!(
        resp.fragment(),
        Some("section1"),
        "original fragment should be preserved across redirects"
    );
    assert!(
        resp.url().to_string().ends_with("/target"),
        "should redirect to /target"
    );
}

// RFC 7231 Section 7.1.2: when Location has its own fragment, that fragment
// takes priority over the original request's fragment.
#[tokio::test]
async fn redirect_location_fragment_overrides_original() {
    let (addr, _) = h1_server_with(|req| async move {
        let path = req.uri().path().to_string();
        if path == "/page" {
            Ok::<_, Infallible>(
                Response::builder()
                    .status(302)
                    .header("Location", "/target#newsection")
                    .body(Full::new(Bytes::new()))
                    .unwrap(),
            )
        } else {
            Ok(Response::new(Full::new(Bytes::from("ok"))))
        }
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();

    let resp = client
        .get(&format!("http://{addr}/page#oldsection"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    // Location fragment takes priority over original fragment.
    assert_eq!(
        resp.fragment(),
        Some("newsection"),
        "Location fragment should override original fragment"
    );
}
