# Cookie Jar

aioduct supports automatic cookie management through a `CookieJar`. When enabled, cookies from `Set-Cookie` response headers are stored and automatically sent in subsequent requests to the same domain.

## Enabling Cookies

Create a `CookieJar` and pass it to the client builder:

```rust,no_run
use aioduct::{Client, CookieJar};
use aioduct::runtime::TokioRuntime;

let jar = CookieJar::new();
let client = Client::<TokioRuntime>::builder()
    .cookie_jar(jar)
    .build();
```

## How It Works

1. When a response contains `Set-Cookie` headers, the jar stores each cookie keyed by domain
2. On subsequent requests, matching cookies are sent in the `Cookie` header
3. Cookies with the `Secure` flag are only sent over HTTPS
4. If a response sets a cookie with the same name, it replaces the existing one

## Example: Session-Based API

```rust,no_run
use aioduct::{Client, CookieJar};
use aioduct::runtime::TokioRuntime;

#[tokio::main]
async fn main() -> Result<(), aioduct::Error> {
    let client = Client::<TokioRuntime>::builder()
        .cookie_jar(CookieJar::new())
        .build();

    // Login — server sets session cookie
    client
        .post("http://example.com/login")?
        .form(&[("user", "alice"), ("pass", "secret")])
        .send()
        .await?;

    // Subsequent requests automatically include the session cookie
    let resp = client
        .get("http://example.com/dashboard")?
        .send()
        .await?;

    println!("{}", resp.text().await?);
    Ok(())
}
```

## Clearing Cookies

```rust
# use aioduct::CookieJar;
let jar = CookieJar::new();
// ... use jar with client ...
jar.clear(); // remove all stored cookies
```

## Without Cookie Jar

By default, no cookie jar is configured. Responses with `Set-Cookie` headers are ignored, and no `Cookie` header is sent automatically. You can still manage cookies manually via `header_str("cookie", "...")`.
