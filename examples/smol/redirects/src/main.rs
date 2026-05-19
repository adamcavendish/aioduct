use std::time::Duration;

use aioduct::{RedirectAction, RedirectPolicy, SmolClient};

fn main() -> Result<(), aioduct::Error> {
    smol::block_on(async {
        // Default: follow up to 10 redirects
        let client = SmolClient::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .unwrap();

        let resp = client.get("https://httpbin.org/redirect/3")?.send().await?;

        println!("Final URL after redirects: {}", resp.url());
        println!("Status: {}", resp.status());

        // Limited redirects — requesting 3 redirects but only allowing 1
        let client = SmolClient::builder()
            .redirect_policy(RedirectPolicy::Limited(1))
            .timeout(Duration::from_secs(10))
            .build()
            .unwrap();

        let req = client.get("https://httpbin.org/redirect/3")?;
        match req.send().await {
            Ok(resp) => {
                println!("\nLimited (1): final URL = {}", resp.url());
                println!("Status: {}", resp.status());
            }
            Err(e) if e.is_redirect() => {
                // Expected: TooManyRedirects because we allow only 1 but need 3
                println!("\nLimited (1): got expected redirect error: {e}");
            }
            Err(e) => return Err(e.into()),
        }

        // No redirects
        let client = SmolClient::builder()
            .redirect_policy(RedirectPolicy::None)
            .timeout(Duration::from_secs(10))
            .build()
            .unwrap();

        let resp = client.get("https://httpbin.org/redirect/1")?.send().await?;

        println!("\nNo redirect: status = {}", resp.status());
        println!("Location: {:?}", resp.headers().get("location"));

        // Custom redirect policy with closure
        let client = SmolClient::builder()
            .redirect_policy(RedirectPolicy::custom(|_from, to, _status, _method| {
                // Only follow redirects to the same host
                if to.host() == Some("httpbin.org") {
                    RedirectAction::Follow
                } else {
                    RedirectAction::Stop
                }
            }))
            .timeout(Duration::from_secs(10))
            .build()
            .unwrap();

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
