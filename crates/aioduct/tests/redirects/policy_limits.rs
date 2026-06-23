use super::*;

#[tokio::test]
async fn test_redirect_max_exceeded() {
    let (addr, _counter) = h1_server_with(|_req| async move {
        Ok::<_, Infallible>(
            Response::builder()
                .status(302)
                .header("location", "/loop")
                .body(Full::new(Bytes::new()))
                .unwrap(),
        )
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .max_redirects(3)
        .build()
        .unwrap();

    let result = client.get(&format!("http://{addr}/")).unwrap().send().await;

    assert!(result.is_err());
}

#[tokio::test]
async fn test_redirect_policy_none() {
    let (addr, _counter) = h1_server_with(|_req| async move {
        Ok::<_, Infallible>(
            Response::builder()
                .status(302)
                .header("location", "/target")
                .body(Full::new(Bytes::new()))
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

#[tokio::test]
async fn test_redirect_policy_custom() {
    let (final_addr, _counter) = h1_server().await;
    let (addr, _counter) = h1_server_with(move |req| {
        let target = format!("http://{final_addr}/");
        async move {
            if req.uri().path() == "/allowed" {
                Ok::<_, Infallible>(
                    Response::builder()
                        .status(302)
                        .header("location", target)
                        .body(Full::new(Bytes::new()))
                        .unwrap(),
                )
            } else {
                Ok::<_, Infallible>(
                    Response::builder()
                        .status(302)
                        .header("location", "/blocked-target")
                        .body(Full::new(Bytes::new()))
                        .unwrap(),
                )
            }
        }
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .redirect_policy(aioduct::RedirectPolicy::custom(
            |_current, next, _status, _method| {
                if next.host() == Some("127.0.0.1") {
                    aioduct::RedirectAction::Follow
                } else {
                    aioduct::RedirectAction::Stop
                }
            },
        ))
        .build()
        .unwrap();

    let resp = client
        .get(&format!("http://{addr}/allowed"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    let body = resp.text().await.unwrap();
    assert_eq!(body, "hello aioduct");
}

#[tokio::test]
async fn test_redirect_stop_returns_redirect_response() {
    let (addr, _counter) = h1_server_with(|_req| async move {
        Ok::<_, Infallible>(
            Response::builder()
                .status(301)
                .header("location", "/target")
                .body(Full::new(Bytes::from("moved")))
                .unwrap(),
        )
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .redirect_policy(aioduct::RedirectPolicy::custom(
            |_current, _next, _status, _method| aioduct::RedirectAction::Stop,
        ))
        .build()
        .unwrap();

    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::MOVED_PERMANENTLY);
}

#[tokio::test]
async fn redirect_max_redirects_error() {
    let (addr, _counter) = h1_server_with(|_req| async move {
        Ok::<_, Infallible>(
            Response::builder()
                .status(302)
                .header("location", "/loop")
                .body(Full::new(Bytes::new()))
                .unwrap(),
        )
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .max_redirects(5)
        .build()
        .unwrap();

    let result = client.get(&format!("http://{addr}/")).unwrap().send().await;
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.is_redirect(), "expected redirect error, got: {err:?}");
}
