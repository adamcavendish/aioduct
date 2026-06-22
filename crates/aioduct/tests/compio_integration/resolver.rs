use super::*;

#[test]
fn test_compio_force_addr_skips_dns() {
    let addr = start_server_tokio();
    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::new();
        let resp = client
            .get_local(&format!("http://127.0.0.1:{}/", addr.port()))
            .unwrap()
            .force_addr(addr)
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), http::StatusCode::OK);
        let body = resp.text().await.unwrap();
        assert_eq!(body, "hello aioduct");
    });
}

#[test]
fn test_compio_force_addr_with_resolve_all_workflow() {
    let addr = start_server_tokio();
    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::new();
        let addrs = client.resolve_all("127.0.0.1", addr.port()).await.unwrap();
        let chosen = addrs.into_iter().next().unwrap();

        let resp = client
            .get_local(&format!("http://127.0.0.1:{}/", addr.port()))
            .unwrap()
            .force_addr(chosen)
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), http::StatusCode::OK);
        let body = resp.text().await.unwrap();
        assert_eq!(body, "hello aioduct");
    });
}

#[test]
fn test_compio_system_resolver_resolves_localhost() {
    let addr = start_server_tokio();
    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .resolver(aioduct::SystemResolver)
            .build_local()
            .unwrap();

        let resp = client
            .get_local(&format!("http://127.0.0.1:{}/", addr.port()))
            .unwrap()
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), http::StatusCode::OK);
        let body = resp.text().await.unwrap();
        assert_eq!(body, "hello aioduct");
    });
}
