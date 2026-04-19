use aioduct::runtime::TokioRuntime;
use aioduct::{Client, RedirectAction, RedirectPolicy};

#[tokio::main]
async fn main() -> Result<(), aioduct::Error> {
    // Default: follow up to 10 redirects
    let client = Client::<TokioRuntime>::builder().build();

    let resp = client.get("https://httpbin.org/redirect/3")?.send().await?;

    println!("Final URL after redirects: {}", resp.url());
    println!("Status: {}", resp.status());

    // Limited redirects
    let client = Client::<TokioRuntime>::builder()
        .redirect_policy(RedirectPolicy::Limited(1))
        .build();

    let resp = client.get("https://httpbin.org/redirect/3")?.send().await?;

    // Only followed 1 redirect, then stopped
    println!("\nLimited (1): final URL = {}", resp.url());
    println!("Status: {}", resp.status());

    // No redirects
    let client = Client::<TokioRuntime>::builder()
        .redirect_policy(RedirectPolicy::None)
        .build();

    let resp = client.get("https://httpbin.org/redirect/1")?.send().await?;

    println!("\nNo redirect: status = {}", resp.status());
    println!("Location: {:?}", resp.headers().get("location"));

    // Custom redirect policy with closure
    let client = Client::<TokioRuntime>::builder()
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
}
