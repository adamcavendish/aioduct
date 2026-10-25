use super::*;

struct RecordingTransport {
    options: Arc<Mutex<Option<HostForwardOptions>>>,
    worker: Mutex<Option<::wasmtime_wasi::runtime::AbortOnDropJoinHandle<()>>>,
}

impl sealed::Sealed for RecordingTransport {}

impl WasiHostTransport for RecordingTransport {
    fn forward_wasi_http(
        &self,
        _request: http::Request<crate::body::RequestBodySend>,
        options: HostForwardOptions,
    ) -> BoxFuture<Result<HostResponse, crate::Error>> {
        *self.options.lock().expect("options lock") = Some(options);
        let worker = self.worker.lock().expect("worker lock").take();
        Box::pin(async move {
            Ok(HostResponse {
                response: http::Response::new(
                    Empty::<Bytes>::new()
                        .map_err(|never| match never {})
                        .boxed_unsync(),
                ),
                worker,
            })
        })
    }
}

#[tokio::test]
async fn hook_retains_default_and_explicit_guest_timeouts() {
    for (input, expected_connect) in [
        (None, Duration::from_secs(600)),
        (
            Some(RequestOptions {
                connect_timeout: Some(Duration::from_millis(17)),
                ..RequestOptions::default()
            }),
            Duration::from_millis(17),
        ),
    ] {
        let options = Arc::new(Mutex::new(None));
        let mut host = WasiHttpHost::builder()
            .transport(RecordingTransport {
                options: options.clone(),
                worker: Mutex::new(None),
            })
            .policy(ExactOriginPolicy::new("http://example.com").unwrap())
            .build()
            .unwrap();
        let (response, completion) = Pin::from(host.send_request(
            request("http://example.com/".into()),
            input,
            Box::new(async { Ok(()) }),
        ))
        .await
        .unwrap();
        assert_eq!(response.status(), http::StatusCode::OK);
        Pin::from(completion).await.unwrap();
        let options = options.lock().unwrap().take().unwrap();
        assert_eq!(options.connect_timeout, expected_connect);
        assert_eq!(options.first_byte_timeout, Duration::from_secs(600));
        assert_eq!(options.read_timeout, Duration::from_secs(600));
    }
}

#[tokio::test]
async fn hook_completion_retains_worker_and_cancels_it_on_drop() {
    struct NotifyDrop(Option<tokio::sync::oneshot::Sender<()>>);

    impl Drop for NotifyDrop {
        fn drop(&mut self) {
            if let Some(sender) = self.0.take() {
                let _ = sender.send(());
            }
        }
    }

    let (sender, mut dropped) = tokio::sync::oneshot::channel();
    let guard = NotifyDrop(Some(sender));
    let worker = ::wasmtime_wasi::runtime::spawn(async move {
        let _guard = guard;
        std::future::pending::<()>().await;
    });
    let mut host = WasiHttpHost::builder()
        .transport(RecordingTransport {
            options: Arc::new(Mutex::new(None)),
            worker: Mutex::new(Some(worker)),
        })
        .policy(ExactOriginPolicy::new("http://example.com").unwrap())
        .build()
        .unwrap();
    let (_response, completion) = Pin::from(host.send_request(
        request("http://example.com/".into()),
        None,
        Box::new(async { Ok(()) }),
    ))
    .await
    .unwrap();
    let mut completion = Pin::from(completion);
    assert!(futures_util::poll!(completion.as_mut()).is_pending());
    assert!(matches!(
        dropped.try_recv(),
        Err(tokio::sync::oneshot::error::TryRecvError::Empty)
    ));
    drop(completion);
    tokio::time::timeout(Duration::from_secs(1), dropped)
        .await
        .expect("dropping the completion must cancel its worker")
        .expect("worker must release its owned resources");
}
