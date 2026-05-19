use aioduct::{NoProxy, ProxyConfig, ProxySettings, TokioClient};

#[tokio::main]
async fn main() -> Result<(), aioduct::Error> {
    // HTTP proxy
    println!("=== Proxy Configuration Examples ===\n");

    // Single HTTP proxy for all traffic
    let _client = TokioClient::builder()
        .proxy_settings(ProxySettings::all(
            ProxyConfig::http("http://proxy.example.com:8080").unwrap(),
        ))
        .build()
        .unwrap();

    println!("1. HTTP proxy: http://proxy.example.com:8080");

    // SOCKS5 proxy (e.g., via SSH tunnel or Tor)
    let _client = TokioClient::builder()
        .proxy_settings(ProxySettings::all(
            ProxyConfig::socks5("socks5://127.0.0.1:1080").unwrap(),
        ))
        .build()
        .unwrap();

    println!("2. SOCKS5 proxy: socks5://127.0.0.1:1080");

    // Separate proxies for HTTP and HTTPS
    let _client = TokioClient::builder()
        .proxy_settings(
            ProxySettings::default()
                .http(ProxyConfig::http("http://http-proxy:3128").unwrap())
                .https(ProxyConfig::http("http://https-proxy:3129").unwrap()),
        )
        .build()
        .unwrap();

    println!("3. Split HTTP/HTTPS proxies");

    // Proxy with authentication
    let _client = TokioClient::builder()
        .proxy_settings(ProxySettings::all(
            ProxyConfig::http("http://user:password@proxy.example.com:8080").unwrap(),
        ))
        .build()
        .unwrap();

    println!("4. Authenticated proxy");

    // No-proxy list: bypass proxy for certain hosts
    let _client = TokioClient::builder()
        .proxy_settings(
            ProxySettings::all(ProxyConfig::http("http://proxy.example.com:8080").unwrap())
                .no_proxy(NoProxy::new("localhost,127.0.0.1,.internal.corp")),
        )
        .build()
        .unwrap();

    println!("5. Proxy with no-proxy bypass list");

    println!("\n(No actual requests made — these examples show configuration only)");
    println!("Set up a real proxy to test connectivity.");

    Ok(())
}
