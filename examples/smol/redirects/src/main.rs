use aioduct::runtime::smol_rt::TcpConnector;
use aioduct::{RedirectAction, RedirectPolicy, SmolClient};

fn main() -> Result<(), aioduct::Error> {
    smol::block_on(async {
        // Default: follow up to 10 redirects
        let client = SmolClient::builder(TcpConnector).build();

        let resp = client.get("https://httpbin.org/redirect/3")?.send().await?;

        println!("Final URL after redirects: {}", resp.url());
        println!("Status: {}", resp.status());

        // Limited redirects
        let client = SmolClient::builder(TcpConnector)
            .redirect_policy(RedirectPolicy::Limited(1))
            .build();

        let resp = client.get("https://httpbin.org/redirect/3")?.send().await?;

        // Only followed 1 redirect, then stopped
        println!("\nLimited (1): final URL = {}", resp.url());
        println!("Status: {}", resp.status());

        // No redirects
        let client = SmolClient::builder(TcpConnector)
            .redirect_policy(RedirectPolicy::None)
            .build();

        let resp = client.get("https://httpbin.org/redirect/1")?.send().await?;

        println!("\nNo redirect: status = {}", resp.status());
        println!("Location: {:?}", resp.headers().get("location"));

        // Custom redirect policy with closure
        let client = SmolClient::builder(TcpConnector)
            .redirect_policy(RedirectPolicy::custom(|_from, to, _status, _method| {
                // Only follow redirects to the same host
                if to.host() == Some("httpbin.org") {
                    RedirectAction::Follow
                } else {
                    RedirectAction::Stop
                }
            }))
            .build();

        let resp = client
            .get("https://httpbin.org/redirect-to?url=https%3A%2F%2Fexample.com")?
            .send()
            .await?;

        println!(
            "\nCustom policy (same-host only): final URL = {}",
            resp.url()
        );

        Ok(())
    })
}
