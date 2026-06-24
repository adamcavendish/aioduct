use super::*;

#[cfg(feature = "tokio")]
#[tokio::test]
async fn test_middleware_adds_request_header() {
    let (addr, _counter) = h1_server_with(|req| async move {
        let val = req
            .headers()
            .get("x-middleware")
            .map(|v| v.to_str().unwrap().to_string())
            .unwrap_or_default();
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(val))))
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .middleware(
            |req: &mut http::Request<aioduct::body::RequestBodySend>, _uri: &http::Uri| {
                req.headers_mut().insert(
                    http::header::HeaderName::from_static("x-middleware"),
                    http::header::HeaderValue::from_static("injected"),
                );
            },
        )
        .build()
        .unwrap();

    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    assert_eq!(resp.text().await.unwrap(), "injected");
}
#[cfg(feature = "tokio")]
#[tokio::test]
async fn test_middleware_modifies_response_header() {
    use std::sync::atomic::{AtomicBool, Ordering};

    let (addr, _counter) = h1_server().await;

    struct ResponseTagger {
        called: Arc<AtomicBool>,
    }

    impl aioduct::Middleware for ResponseTagger {
        fn on_response(
            &self,
            response: &mut http::Response<aioduct::body::RequestBodySend>,
            _uri: &http::Uri,
        ) {
            self.called.store(true, Ordering::SeqCst);
            response.headers_mut().insert(
                http::header::HeaderName::from_static("x-from-middleware"),
                http::header::HeaderValue::from_static("yes"),
            );
        }
    }

    let called = Arc::new(AtomicBool::new(false));
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .middleware(ResponseTagger {
            called: called.clone(),
        })
        .build()
        .unwrap();

    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert!(called.load(Ordering::SeqCst));
    assert_eq!(
        resp.headers()
            .get("x-from-middleware")
            .unwrap()
            .to_str()
            .unwrap(),
        "yes"
    );
    assert_eq!(resp.text().await.unwrap(), "hello aioduct");
}
#[cfg(feature = "tokio")]
#[tokio::test]
async fn test_multiple_middleware_ordering() {
    let (addr, _counter) = h1_server_with(|req| async move {
        let val = req
            .headers()
            .get("x-order")
            .map(|v| v.to_str().unwrap().to_string())
            .unwrap_or_default();
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(val))))
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .middleware(
            |req: &mut http::Request<aioduct::body::RequestBodySend>, _uri: &http::Uri| {
                req.headers_mut().insert(
                    http::header::HeaderName::from_static("x-order"),
                    http::header::HeaderValue::from_static("first"),
                );
            },
        )
        .middleware(
            |req: &mut http::Request<aioduct::body::RequestBodySend>, _uri: &http::Uri| {
                req.headers_mut().insert(
                    http::header::HeaderName::from_static("x-order"),
                    http::header::HeaderValue::from_static("second"),
                );
            },
        )
        .build()
        .unwrap();

    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    assert_eq!(resp.text().await.unwrap(), "second");
}
