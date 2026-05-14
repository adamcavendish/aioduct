use std::time::Duration;

use aioduct::runtime::compio_rt::TcpConnector;
use aioduct::{CompioClient, RedirectAction, RedirectPolicy};

fn main() -> Result<(), aioduct::Error> {
    compio_runtime::Runtime::new().unwrap().block_on(async {
        // Default: follow up to 10 redirects
        let client = CompioClient::builder_local(TcpConnector)
            .timeout(Duration::from_secs(10))
            .build_local();

        let resp = client
            .get_local("https://httpbin.org/redirect/3")?
            .send()
            .await?;

        println!("Final URL after redirects: {}", resp.url());
        println!("Status: {}", resp.status());

        // Limited redirects — requesting 3 redirects but only allowing 1
        let client = CompioClient::builder_local(TcpConnector)
            .redirect_policy(RedirectPolicy::Limited(1))
            .timeout(Duration::from_secs(10))
            .build_local();

        let req = client.get_local("https://httpbin.org/redirect/3")?;
        match req.send().await {
            Ok(resp) => {
                println!("\nLimited (1): final URL = {}", resp.url());
                println!("Status: {}", resp.status());
            }
            Err(e) if e.is_redirect() => {
                // Expected: TooManyRedirects because we allow only 1 but need 3
                println!("\nLimited (1): got expected redirect error: {e}");
            }
            Err(e) => return Err(e),
        }

        // No redirects
        let client = CompioClient::builder_local(TcpConnector)
            .redirect_policy(RedirectPolicy::None)
            .timeout(Duration::from_secs(10))
            .build_local();

        let resp = client
            .get_local("https://httpbin.org/redirect/1")?
            .send()
            .await?;

        println!("\nNo redirect: status = {}", resp.status());
        println!("Location: {:?}", resp.headers().get("location"));

        // Custom redirect policy with closure
        let client = CompioClient::builder_local(TcpConnector)
            .redirect_policy(RedirectPolicy::custom(|_from, to, _status, _method| {
                // Only follow redirects to the same host
                if to.host() == Some("httpbin.org") {
                    RedirectAction::Follow
                } else {
                    RedirectAction::Stop
                }
            }))
            .timeout(Duration::from_secs(10))
            .build_local();

        let resp = client
            .get_local("https://httpbin.org/redirect-to?url=https%3A%2F%2Fexample.com")?
            .send()
            .await?;

        println!(
            "\nCustom policy (same-host only): final URL = {}",
            resp.url()
        );

        Ok(())
    })
}
