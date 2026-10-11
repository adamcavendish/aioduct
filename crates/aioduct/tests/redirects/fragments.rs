use super::*;

#[derive(Clone, Default)]
struct RetryTargetObserver {
    targets: std::sync::Arc<std::sync::Mutex<Vec<http::Uri>>>,
}

struct RetryTargetObservation {
    targets: std::sync::Arc<std::sync::Mutex<Vec<http::Uri>>>,
    current: std::sync::Mutex<Option<http::Uri>>,
}
impl aioduct::Observer for RetryTargetObserver {
    type Context = ();
    type Observation = RetryTargetObservation;
    type ConnectionObservation = aioduct::NoopConnectionObservation;
    fn begin(
        &self,
        _: aioduct::RequestInfo<'_>,
        _: Option<()>,
    ) -> (aioduct::Interest, Self::Observation) {
        (
            aioduct::Interest::ATTEMPT,
            RetryTargetObservation {
                targets: self.targets.clone(),
                current: std::sync::Mutex::new(None),
            },
        )
    }
    fn begin_connection(&self) -> (aioduct::Interest, Self::ConnectionObservation) {
        (aioduct::Interest::EMPTY, aioduct::NoopConnectionObservation)
    }
}
impl aioduct::RequestObservation for RetryTargetObservation {
    fn on_attempt(&self, event: &aioduct::AttemptEvent<'_>) {
        match event {
            aioduct::AttemptEvent::Started { uri, .. } => {
                *self.current.lock().unwrap() = Some((*uri).clone())
            }
            aioduct::AttemptEvent::RetryScheduled { .. } => {
                if let Some(uri) = self.current.lock().unwrap().as_ref() {
                    self.targets.lock().unwrap().push(uri.clone());
                }
            }
            _ => {}
        }
    }
}

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

#[tokio::test]
async fn configured_retry_preserves_fragment_selected_by_redirect() {
    let target_attempts = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
    let server_attempts = target_attempts.clone();
    let (addr, _) = h1_server_with(move |req| {
        let attempts = server_attempts.clone();
        async move {
            if req.uri().path() == "/start" {
                return Ok::<_, Infallible>(
                    Response::builder()
                        .status(302)
                        .header("location", "/target#redirect-fragment")
                        .body(Full::new(Bytes::new()))
                        .unwrap(),
                );
            }

            assert_eq!(req.uri().path(), "/target");
            let attempt = attempts.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(Response::builder()
                .status(if attempt == 0 { 503 } else { 200 })
                .body(Full::new(Bytes::from_static(b"done")))
                .unwrap())
        }
    })
    .await;

    let observer = RetryTargetObserver::default();
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .observer(observer.clone())
        .build()
        .unwrap();
    let response = client
        .get(&format!("http://{addr}/start#original-fragment"))
        .unwrap()
        .retry(
            aioduct::RetryConfig::default()
                .max_retries(1)
                .initial_backoff(Duration::ZERO),
        )
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), http::StatusCode::OK);
    assert_eq!(response.fragment(), Some("redirect-fragment"));
    assert_eq!(target_attempts.load(std::sync::atomic::Ordering::SeqCst), 2);
    let targets = observer.targets.lock().unwrap();
    assert_eq!(targets.len(), 1);
    assert_eq!(targets[0].path(), "/target");
}
