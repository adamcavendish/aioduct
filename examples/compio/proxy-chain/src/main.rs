use std::env;

use aioduct::{CompioClient, ProxyChain, ProxyConfig};

fn parse_proxy(value: &str) -> Result<ProxyConfig, aioduct::Error> {
    ProxyConfig::detect_from_url(value)
        .ok_or_else(|| aioduct::Error::InvalidUrl("invalid proxy URL".into()))
}

fn main() -> Result<(), aioduct::Error> {
    compio_runtime::Runtime::new().unwrap().block_on(async {
        let args = env::args().skip(1).collect::<Vec<_>>();
        if args.len() != 3 {
            eprintln!("Usage: proxy-chain <first-proxy-url> <second-proxy-url> <target-url>");
            return Ok(());
        }

        let chain = ProxyChain::new(vec![parse_proxy(&args[0])?, parse_proxy(&args[1])?]);
        let client = CompioClient::builder().proxy_chain(chain).build_local()?;
        let response = client.get_local(&args[2])?.send().await?;

        println!("{:?} {}", response.version(), response.status());
        Ok(())
    })
}
