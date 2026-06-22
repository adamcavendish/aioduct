use super::*;

#[test]
fn test_compio_sse_stream() {
    let addr = start_server_with_tokio(|_req| async move {
        let body = "data: event1\n\ndata: event2\n\n";
        Ok::<_, Infallible>(
            Response::builder()
                .header("content-type", "text/event-stream")
                .body(Full::new(Bytes::from(body)))
                .unwrap(),
        )
    });

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::new();
        let resp = client
            .get_local(&format!("http://{addr}/"))
            .unwrap()
            .send()
            .await
            .unwrap();

        let mut stream = resp.into_sse_stream();
        let e1 = stream.next().await.unwrap().unwrap();
        let e2 = stream.next().await.unwrap().unwrap();
        assert!(stream.next().await.is_none());

        match (&e1, &e2) {
            (aioduct::sse::SseEvent::Message(m1), aioduct::sse::SseEvent::Message(m2)) => {
                assert_eq!(m1.data, "event1");
                assert_eq!(m2.data, "event2");
            }
            _ => panic!("expected two messages"),
        }
    });
}
