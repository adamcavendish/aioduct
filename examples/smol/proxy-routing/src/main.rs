use std::env;

use aioduct::{EnvCredentialResolver, NoProxy, ProxyConfig, ProxySettings, SmolClient};

fn parse_proxy(value: &str) -> Result<ProxyConfig, aioduct::Error> {
    ProxyConfig::detect_from_url(value)
        .ok_or_else(|| aioduct::Error::InvalidUrl("invalid proxy URL".into()))
}

fn main() -> Result<(), aioduct::Error> {
    smol::block_on(async {
        let args = env::args().skip(1).collect::<Vec<_>>();
        if !(3..=4).contains(&args.len()) {
            eprintln!(
                "Usage: proxy-routing <http-proxy-url> <https-proxy-url> <target-url> [no-proxy]"
            );
            return Ok(());
        }

        let no_proxy = args
            .get(3)
            .map(String::as_str)
            .unwrap_or("localhost,127.0.0.1");
        let settings = ProxySettings::default()
            .http(parse_proxy(&args[0])?)
            .https(parse_proxy(&args[1])?)
            .no_proxy(NoProxy::new(no_proxy))
            .proxy_credential_resolver(EnvCredentialResolver);
        let client = SmolClient::builder().proxy_settings(settings).build()?;
        let response = client.get(&args[2])?.send().await?;

        println!("{:?} {}", response.version(), response.status());
        Ok(())
    })
}
