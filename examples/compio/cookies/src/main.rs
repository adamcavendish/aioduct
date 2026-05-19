use aioduct::{CompioClient, CookieJar};

fn main() -> Result<(), aioduct::Error> {
    compio_runtime::Runtime::new().unwrap().block_on(async {
        let jar = CookieJar::new();

        let client = CompioClient::builder()
            .cookie_jar(jar.clone())
            .build_local()
            .unwrap();

        // First request — server sets cookies
        let resp = client
            .get_local("https://httpbin.org/cookies/set/session_id/abc123")?
            .send()
            .await?;

        println!("After set: status = {}", resp.status());

        // Inspect cookies in the jar
        for cookie in jar.cookies() {
            println!("  Cookie: {}={}", cookie.name(), cookie.value());
        }

        // Second request — cookies are automatically sent
        let resp = client
            .get_local("https://httpbin.org/cookies")?
            .send()
            .await?;

        println!("\nCookies echoed back:\n{}", resp.text().await?);

        // Manual cookie on a per-request basis
        let resp = client
            .get_local("https://httpbin.org/cookies")?
            .header(http::header::COOKIE, "manual_cookie=hello".parse().unwrap())
            .send()
            .await?;

        println!("\nWith manual cookie:\n{}", resp.text().await?);

        Ok(())
    })
}
