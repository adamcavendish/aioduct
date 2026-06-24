use super::*;

#[tokio::test]
async fn test_proxy_settings_no_proxy_bypass() {
    let (target_addr, _counter) = h1_server().await;
    let (proxy_addr, _conns) = connect_proxy().await;

    // Second target also accessible through the proxy.
    let (other_addr, _counter) = h1_server_with(|_req| async move {
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("proxied-ok"))))
    })
    .await;

    let settings = aioduct::ProxySettings::all(
        aioduct::ProxyConfig::http(&format!("http://{proxy_addr}")).unwrap(),
    )
    .no_proxy(aioduct::NoProxy::new(&format!("{}", target_addr.ip())));

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .proxy_settings(settings)
        .build()
        .unwrap();

    // Request to bypassed host goes direct.
    let resp = client
        .get(&format!("http://{target_addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.text().await.unwrap(), "hello aioduct");

    // Request to non-bypassed host goes through proxy.
    let resp = client
        .get(&format!("http://{other_addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.text().await.unwrap(), "proxied-ok");
}
#[tokio::test]
async fn test_no_proxy_wildcard_bypasses_all() {
    let (target_addr, _counter) = h1_server_with(|_req| async move {
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("direct"))))
    })
    .await;

    let settings =
        aioduct::ProxySettings::all(aioduct::ProxyConfig::http("http://127.0.0.1:9999").unwrap())
            .no_proxy(aioduct::NoProxy::new("*"));

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .proxy_settings(settings)
        .build()
        .unwrap();

    let resp = client
        .get(&format!("http://{target_addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.text().await.unwrap(), "direct");
}
#[tokio::test]
async fn test_no_proxy_domain_suffix_matching() {
    let no_proxy = aioduct::NoProxy::new(".example.com, localhost");

    // Direct matches
    assert!(!no_proxy.matches("example.com")); // no leading dot, exact doesn't match
    assert!(no_proxy.matches("foo.example.com"));
    assert!(no_proxy.matches("bar.baz.example.com"));
    assert!(no_proxy.matches("localhost"));

    // Non-matches
    assert!(!no_proxy.matches("notexample.com"));
    assert!(!no_proxy.matches("other.com"));
}
#[tokio::test]
async fn test_no_proxy_bare_domain_matches_subdomains() {
    let no_proxy = aioduct::NoProxy::new("example.com");

    assert!(no_proxy.matches("example.com"));
    assert!(no_proxy.matches("foo.example.com"));
    assert!(!no_proxy.matches("notexample.com"));
}

#[tokio::test]
async fn test_no_proxy_ip_cidr_and_port_matching() {
    let no_proxy =
        aioduct::NoProxy::new("127.0.0.1:8080, 10.0.0.0/8, 2001:db8::/32, [2001:db9::5]:8443");

    assert!(no_proxy.matches("127.0.0.1:8080"));
    assert!(!no_proxy.matches("127.0.0.1:8081"));
    assert!(no_proxy.matches("10.20.30.40"));
    assert!(!no_proxy.matches("192.0.2.1"));
    assert!(no_proxy.matches("2001:db8::1234"));
    assert!(!no_proxy.matches("2001:db9::1234"));
    assert!(no_proxy.matches("[2001:db9::5]:8443"));
    assert!(!no_proxy.matches("[2001:db9::5]:443"));
}
