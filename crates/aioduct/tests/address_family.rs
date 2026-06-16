#![cfg(feature = "tokio")]

use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::pin::Pin;

use aioduct::HttpEngineSend;
use aioduct::address_family::AddressFamily;
use aioduct::runtime::Resolve;
use aioduct::runtime::TokioRuntime;
use aioduct::runtime::tokio_rt::TcpConnector;

/// A resolver that always returns a fixed mixed IPv6/IPv4 address list,
/// regardless of host, so address-family filtering can be tested directly.
struct MixedResolver;

impl Resolve for MixedResolver {
    fn resolve(
        &self,
        _host: &str,
        port: u16,
    ) -> Pin<Box<dyn Future<Output = io::Result<SocketAddr>> + Send>> {
        Box::pin(async move { Ok(format!("[::1]:{port}").parse().unwrap()) })
    }

    fn resolve_all(
        &self,
        _host: &str,
        port: u16,
    ) -> Pin<Box<dyn Future<Output = io::Result<Vec<SocketAddr>>> + Send>> {
        Box::pin(async move {
            Ok(vec![
                format!("[::1]:{port}").parse().unwrap(),
                format!("10.0.0.1:{port}").parse().unwrap(),
                format!("[::2]:{port}").parse().unwrap(),
                format!("10.0.0.2:{port}").parse().unwrap(),
            ])
        })
    }
}

fn client_with(family: AddressFamily) -> HttpEngineSend<TokioRuntime, TcpConnector> {
    HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .resolver(MixedResolver)
        .address_family(family)
        .build()
        .unwrap()
}

#[tokio::test]
async fn address_family_any_keeps_all() {
    let addrs = client_with(AddressFamily::Any)
        .resolve_all("example.com", 80)
        .await
        .unwrap();
    assert_eq!(addrs.len(), 4);
}

#[tokio::test]
async fn address_family_ipv4_only_filters_ipv6() {
    let addrs = client_with(AddressFamily::Ipv4Only)
        .resolve_all("example.com", 80)
        .await
        .unwrap();
    assert_eq!(addrs.len(), 2);
    assert!(addrs.iter().all(|a| a.is_ipv4()));
}

#[tokio::test]
async fn address_family_ipv6_only_filters_ipv4() {
    let addrs = client_with(AddressFamily::Ipv6Only)
        .resolve_all("example.com", 80)
        .await
        .unwrap();
    assert_eq!(addrs.len(), 2);
    assert!(addrs.iter().all(|a| a.is_ipv6()));
}

#[tokio::test]
async fn address_family_prefer_ipv4_orders_v4_first() {
    let addrs = client_with(AddressFamily::PreferIpv4)
        .resolve_all("example.com", 80)
        .await
        .unwrap();
    assert_eq!(addrs.len(), 4);
    assert!(addrs[0].is_ipv4());
    assert!(addrs[1].is_ipv4());
    assert!(addrs[2].is_ipv6());
    assert!(addrs[3].is_ipv6());
}

#[tokio::test]
async fn address_family_prefer_ipv6_orders_v6_first() {
    let addrs = client_with(AddressFamily::PreferIpv6)
        .resolve_all("example.com", 80)
        .await
        .unwrap();
    assert_eq!(addrs.len(), 4);
    assert!(addrs[0].is_ipv6());
    assert!(addrs[1].is_ipv6());
    assert!(addrs[2].is_ipv4());
    assert!(addrs[3].is_ipv4());
}

/// An IP-literal request host bypasses the family filter: requesting an IPv4
/// literal under Ipv6Only still resolves to that literal rather than being
/// dropped. (An explicit literal is the caller's deliberate choice.)
#[tokio::test]
async fn ip_literal_bypasses_address_family() {
    let addrs = client_with(AddressFamily::Ipv6Only)
        .resolve_all("10.0.0.9", 80)
        .await
        .unwrap();
    assert_eq!(addrs.len(), 1);
    assert!(addrs[0].is_ipv4());
}

/// When filtering removes every address, resolution fails with a clear error
/// rather than handing an empty list to the connector.
#[tokio::test]
async fn address_family_only_with_no_match_errors() {
    // MixedResolver returns both families, but force an impossible combination
    // via a resolver that returns only IPv6 and require IPv4.
    struct V6Only;
    impl Resolve for V6Only {
        fn resolve(
            &self,
            _host: &str,
            port: u16,
        ) -> Pin<Box<dyn Future<Output = io::Result<SocketAddr>> + Send>> {
            Box::pin(async move { Ok(format!("[::1]:{port}").parse().unwrap()) })
        }
        fn resolve_all(
            &self,
            _host: &str,
            port: u16,
        ) -> Pin<Box<dyn Future<Output = io::Result<Vec<SocketAddr>>> + Send>> {
            Box::pin(async move { Ok(vec![format!("[::1]:{port}").parse().unwrap()]) })
        }
    }

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .resolver(V6Only)
        .address_family(AddressFamily::Ipv4Only)
        .build()
        .unwrap();

    let result = client.resolve_all("example.com", 80).await;
    assert!(result.is_err(), "filtering to empty should error");
}
