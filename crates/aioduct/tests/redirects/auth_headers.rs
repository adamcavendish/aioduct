use super::*;

#[tokio::test]
async fn redirect_cross_origin_strips_auth() {
    let (final_addr, _counter) = h1_server_with(|req| async move {
        let auth = req
            .headers()
            .get("authorization")
            .map(|v| v.to_str().unwrap().to_owned());
        let body = match auth {
            Some(v) => format!("auth={v}"),
            None => "auth=none".to_string(),
        };
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(body))))
    })
    .await;

    let (redirect_addr, _counter) = h1_server_with(move |_req| {
        let target = format!("http://{final_addr}/final");
        async move {
            Ok::<_, Infallible>(
                Response::builder()
                    .status(302)
                    .header("location", target)
                    .body(Full::new(Bytes::new()))
                    .unwrap(),
            )
        }
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();
    let resp = client
        .get(&format!("http://{redirect_addr}/start"))
        .unwrap()
        .bearer_auth("secret-token")
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    let body = resp.text().await.unwrap();
    assert_eq!(
        body, "auth=none",
        "authorization header should be stripped on cross-origin redirect"
    );
}

#[tokio::test]
async fn redirect_same_origin_preserves_auth() {
    let (addr, _counter) = h1_server_with(|req| async move {
        if req.uri().path() == "/start" {
            Ok::<_, Infallible>(
                Response::builder()
                    .status(302)
                    .header("location", "/final")
                    .body(Full::new(Bytes::new()))
                    .unwrap(),
            )
        } else {
            let auth = req
                .headers()
                .get("authorization")
                .map(|v| v.to_str().unwrap().to_owned())
                .unwrap_or_else(|| "none".to_owned());
            Ok(Response::new(Full::new(Bytes::from(auth))))
        }
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();
    let resp = client
        .get(&format!("http://{addr}/start"))
        .unwrap()
        .bearer_auth("secret-token")
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    let body = resp.text().await.unwrap();
    assert!(
        body.contains("secret-token"),
        "same-origin redirect should preserve auth, got: {body}"
    );
}

#[tokio::test]
async fn redirect_chain_url_reflects_final() {
    let (final_addr, _counter) = h1_server().await;
    let (mid_addr, _counter) = h1_server_with(move |_req| {
        let target = format!("http://{final_addr}/final");
        async move {
            Ok::<_, Infallible>(
                Response::builder()
                    .status(302)
                    .header("location", target)
                    .body(Full::new(Bytes::new()))
                    .unwrap(),
            )
        }
    })
    .await;

    let (redirect_addr, _counter) = h1_server_with(move |_req| {
        let target = format!("http://{mid_addr}/mid");
        async move {
            Ok::<_, Infallible>(
                Response::builder()
                    .status(301)
                    .header("location", target)
                    .body(Full::new(Bytes::new()))
                    .unwrap(),
            )
        }
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();
    let resp = client
        .get(&format!("http://{redirect_addr}/start"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    let url = resp.url().to_string();
    assert!(
        url.contains("/final"),
        "URL should reflect final destination, got: {url}"
    );
}

#[tokio::test]
async fn redirect_to_invalid_scheme_returns_error() {
    let (addr, _counter) = h1_server_with(|_req| async move {
        Ok::<_, Infallible>(
            Response::builder()
                .status(302)
                .header("location", "ftp://invalid.example.com/")
                .body(Full::new(Bytes::new()))
                .unwrap(),
        )
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();
    let result = client.get(&format!("http://{addr}/")).unwrap().send().await;

    assert!(
        result.is_err(),
        "redirect to ftp:// should produce an error"
    );
}

#[tokio::test]
async fn redirect_stop_policy_allows_invalid_location() {
    let (addr, _counter) = h1_server_with(|_req| async move {
        Ok::<_, Infallible>(
            Response::builder()
                .status(302)
                .header("location", "htt://invalid/")
                .body(Full::new(Bytes::from("redirect body")))
                .unwrap(),
        )
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .redirect_policy(aioduct::RedirectPolicy::none())
        .build()
        .unwrap();

    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::FOUND);
}

// Same-origin redirect with explicit port in Location should preserve auth.
#[tokio::test]
async fn redirect_same_host_different_port_representation_strips_auth() {
    let (addr, _) = h1_server_with(|req| async move {
        let path = req.uri().path().to_string();
        if path == "/step1" {
            let host = req.headers().get("host").unwrap().to_str().unwrap();
            let resp = Response::builder()
                .status(302)
                .header("Location", format!("http://{host}/step2"))
                .body(Full::new(Bytes::new()))
                .unwrap();
            Ok::<_, Infallible>(resp)
        } else {
            let auth = req
                .headers()
                .get("authorization")
                .map(|v| v.to_str().unwrap_or("").to_string())
                .unwrap_or_default();
            Ok(Response::new(Full::new(Bytes::from(format!(
                "auth={auth}"
            )))))
        }
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();

    let resp = client
        .get(&format!("http://{addr}/step1"))
        .unwrap()
        .bearer_auth("my-secret-token")
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let body = resp.text().await.unwrap();
    assert!(
        body.contains("Bearer my-secret-token"),
        "same-origin redirect (same host:port) should preserve auth, got: {body}"
    );
}

// Cross-origin redirect strips both Authorization AND Cookie headers.
#[tokio::test]
async fn redirect_cross_origin_strips_auth_and_cookie() {
    let (target_addr, _) = h1_server_with(|req| async move {
        let auth = req
            .headers()
            .get("authorization")
            .map(|v| v.to_str().unwrap_or("").to_string())
            .unwrap_or_default();
        let cookie = req
            .headers()
            .get("cookie")
            .map(|v| v.to_str().unwrap_or("").to_string())
            .unwrap_or_default();
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(format!(
            "auth={auth} cookie={cookie}"
        )))))
    })
    .await;

    let (origin_addr, _) = h1_server_with(move |req| {
        let target_addr = target_addr;
        async move {
            let path = req.uri().path();
            if path == "/redirect" {
                let resp = Response::builder()
                    .status(302)
                    .header("Location", format!("http://{target_addr}/final"))
                    .body(Full::new(Bytes::new()))
                    .unwrap();
                Ok::<_, Infallible>(resp)
            } else {
                Ok(Response::new(Full::new(Bytes::from("origin"))))
            }
        }
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();

    let resp = client
        .get(&format!("http://{origin_addr}/redirect"))
        .unwrap()
        .bearer_auth("secret-token")
        .header(
            http::header::COOKIE,
            http::header::HeaderValue::from_static("session=abc"),
        )
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let body = resp.text().await.unwrap();
    assert!(
        !body.contains("secret-token"),
        "cross-origin redirect should strip Authorization, got: {body}"
    );
    assert!(
        !body.contains("session=abc"),
        "cross-origin redirect should strip Cookie, got: {body}"
    );
}

// 302 redirect clears body AND Content-Length/Content-Type headers.
