use std::env;

use aioduct::{EnvCredentialResolver, ProxyConfig, ProxySettings, TokioClient};

#[tokio::main]
async fn main() -> Result<(), aioduct::Error> {
    let args = env::args().skip(1).collect::<Vec<_>>();
    if args.len() != 2 {
        eprintln!("Usage: proxy-connect <proxy-url> <target-url>");
        eprintln!("Proxy URL schemes: http, https, socks4, socks4a, socks5, socks5h");
        return Ok(());
    }

    let proxy = ProxyConfig::detect_from_url(&args[0])
        .ok_or_else(|| aioduct::Error::InvalidUrl("invalid proxy URL".into()))?;
    let settings = ProxySettings::all(proxy).proxy_credential_resolver(EnvCredentialResolver);
    let client = TokioClient::builder().proxy_settings(settings).build()?;
    let response = client.get(&args[1])?.send().await?;

    println!("{:?} {}", response.version(), response.status());
    Ok(())
}
