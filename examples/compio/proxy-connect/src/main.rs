use std::env;

use aioduct::{CompioClient, EnvCredentialResolver, ProxyConfig, ProxySettings};

fn main() -> Result<(), aioduct::Error> {
    compio_runtime::Runtime::new().unwrap().block_on(async {
        let args = env::args().skip(1).collect::<Vec<_>>();
        if args.len() != 2 {
            eprintln!("Usage: proxy-connect <proxy-url> <target-url>");
            eprintln!("Proxy URL schemes: http, https, socks4, socks4a, socks5, socks5h");
            return Ok(());
        }

        let proxy = ProxyConfig::detect_from_url(&args[0])
            .ok_or_else(|| aioduct::Error::InvalidUrl("invalid proxy URL".into()))?;
        let settings = ProxySettings::all(proxy).proxy_credential_resolver(EnvCredentialResolver);
        let client = CompioClient::builder()
            .proxy_settings(settings)
            .build_local()?;
        let response = client.get_local(&args[1])?.send().await?;

        println!("{:?} {}", response.version(), response.status());
        Ok(())
    })
}
