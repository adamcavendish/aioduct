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
5. Cookies with `Max-Age=0` or a past `Expires` date are removed from the jar
6. The `Path` attribute is respected — cookies are only sent for matching request paths
7. Domain matching supports subdomains — a cookie for `example.com` is sent to `sub.example.com`

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

## Inspecting Cookies

The `CookieJar` and `Cookie` types are public, allowing inspection of stored cookies:

```rust
# use aioduct::CookieJar;
let jar = CookieJar::new();
// ... use jar with client ...

for cookie in jar.cookies() {
    println!("{} = {}", cookie.name(), cookie.value());
    if let Some(domain) = cookie.domain() {
        println!("  domain: {domain}");
    }
    if let Some(path) = cookie.path() {
        println!("  path: {path}");
    }
    println!("  secure: {}", cookie.secure());
    println!("  http_only: {}", cookie.http_only());
}
```

### Cookie Accessors

| Method | Return Type | Description |
|--------|-------------|-------------|
| `name()` | `&str` | Cookie name |
| `value()` | `&str` | Cookie value |
| `domain()` | `Option<&str>` | Domain attribute (defaults to request domain) |
| `path()` | `Option<&str>` | Path attribute |
| `secure()` | `bool` | Whether the cookie requires HTTPS |
| `http_only()` | `bool` | Whether the cookie is HTTP-only |
| `same_site()` | `Option<&SameSite>` | SameSite attribute (Strict, Lax, or None) |

## SameSite Cookies

aioduct parses the `SameSite` attribute from `Set-Cookie` headers per the RFC 6265bis draft:

- **`Strict`** — cookie is only sent in first-party context (same-site requests)
- **`Lax`** — cookie is sent on top-level navigations and same-site requests (browser default)
- **`None`** — cookie is sent in all contexts (requires `Secure` flag)

```rust
# use aioduct::cookie::SameSite;
# use aioduct::CookieJar;
let jar = CookieJar::new();
// ... use jar with client ...
for cookie in jar.cookies() {
    match cookie.same_site() {
        Some(SameSite::Strict) => println!("{}: strict", cookie.name()),
        Some(SameSite::Lax) => println!("{}: lax", cookie.name()),
        Some(SameSite::None) => println!("{}: none", cookie.name()),
        None => println!("{}: not set", cookie.name()),
    }
}
```

## Cookie Prefixes

aioduct enforces cookie prefix validation per RFC 6265bis:

- **`__Host-`** — requires `Secure`, exact domain match (no `Domain` attribute pointing elsewhere), and `Path=/`
- **`__Secure-`** — requires `Secure` flag

Cookies that fail prefix validation are silently rejected.

## Cookie Attributes

### Domain Matching

Cookies use RFC-compliant domain matching with subdomain support:

- A cookie stored for `example.com` matches requests to `example.com` and `sub.example.com`
- A cookie stored for `sub.example.com` does **not** match `example.com` or `other.example.com`
- Leading dots in the `Domain` attribute are stripped (`Domain=.example.com` becomes `example.com`)

### Path Scoping

When a `Set-Cookie` header includes a `Path` attribute, the cookie is only sent for requests whose path starts with the cookie's path:

```text
Set-Cookie: token=abc; Path=/api
```

- `/api` — cookie sent
- `/api/users` — cookie sent
- `/` — cookie **not** sent
- `/other` — cookie **not** sent

### Expiration

Cookies are expired and removed from the jar when:

- `Max-Age=0` or a negative value is received
- An `Expires` date in the past is received (RFC 7231 date format: `Wed, 21 Oct 2015 07:28:00 GMT`)

Expired cookies are never stored; setting `Max-Age=0` on an existing cookie removes it.
