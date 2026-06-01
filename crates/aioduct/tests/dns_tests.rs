#![cfg(feature = "tokio")]

use aioduct::HttpEngineSend;
use aioduct::SystemResolver;
use aioduct::runtime::TokioRuntime;
use aioduct::runtime::tokio_rt::TcpConnector;

use aioduct_test_server::h1::h1_server;

#[tokio::test]
async fn test_force_addr_skips_dns() {
    let (addr, _counter) = h1_server().await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();
    let resp = client
        .get(&format!("http://127.0.0.1:{}/", addr.port()))
        .unwrap()
        .force_addr(addr)
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    let body = resp.text().await.unwrap();
    assert_eq!(body, "hello aioduct");
}

#[tokio::test]
async fn test_force_addr_with_resolve_all_workflow() {
    let (addr, _counter) = h1_server().await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();
    let addrs = client.resolve_all("127.0.0.1", addr.port()).await.unwrap();
    let chosen = addrs.into_iter().next().unwrap();

    let resp = client
        .get(&format!("http://127.0.0.1:{}/", addr.port()))
        .unwrap()
        .force_addr(chosen)
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    let body = resp.text().await.unwrap();
    assert_eq!(body, "hello aioduct");
}

#[tokio::test]
async fn test_system_resolver_resolves_localhost() {
    let (addr, _counter) = h1_server().await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .resolver(SystemResolver)
        .build()
        .unwrap();

    let resp = client
        .get(&format!("http://127.0.0.1:{}/", addr.port()))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    let body = resp.text().await.unwrap();
    assert_eq!(body, "hello aioduct");
}
