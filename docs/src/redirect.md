# Redirect Policy

aioduct follows HTTP redirects automatically by default (up to 10 hops). You can customize this behavior with `RedirectPolicy`.

## Policies

| Policy | Behavior |
|--------|----------|
| `RedirectPolicy::default()` | Follow up to 10 redirects |
| `RedirectPolicy::none()` | Never follow redirects — return the 3xx response as-is |
| `RedirectPolicy::limited(n)` | Follow up to `n` redirects |
| `RedirectPolicy::custom(fn)` | User callback decides per-redirect |

## Method Handling

Regardless of policy, aioduct follows RFC semantics for method changes:

- **301, 302, 303** → method changes to `GET`, body is dropped, content headers (`Content-Type`, `Content-Length`, `Content-Encoding`) are stripped
- **307, 308** → method and body are preserved

Sensitive headers (`Authorization`, `Cookie`, `Proxy-Authorization`) are automatically stripped when redirecting to a different origin. Headers whose `HeaderValue` is marked sensitive with `set_sensitive(true)` are stripped as well.

## No Redirects

```rust,no_run
use aioduct::{TokioClient, RedirectPolicy};

let client = TokioClient::builder()
    .redirect_policy(RedirectPolicy::none())
    .build()?;
```

## Limited Redirects

```rust,no_run
use aioduct::{TokioClient, RedirectPolicy};

// Also available via the shorthand:
let client = TokioClient::builder()
    .max_redirects(5)
    .build()?;

// Equivalent to:
let client = TokioClient::builder()
    .redirect_policy(RedirectPolicy::limited(5))
    .build()?;
```

## Custom Policy

The custom callback receives the current URI, next (redirect target) URI, status code, and HTTP method. Return `RedirectAction::Follow` to follow the redirect, or `RedirectAction::Stop` to stop and return the redirect response.

```rust,no_run
use aioduct::{TokioClient, RedirectAction, RedirectPolicy};

let client = TokioClient::builder()
    .redirect_policy(RedirectPolicy::custom(|current, next, status, method| {
        // Only follow redirects that stay on the same host
        if current.host() == next.host() {
            RedirectAction::Follow
        } else {
            RedirectAction::Stop
        }
    }))
    .build()?;
```

### Use Cases for Custom Policies

- **Same-origin only**: prevent redirects to external domains
- **HTTPS-only**: reject downgrades from HTTPS to HTTP
- **Logging**: log each redirect decision while still following
- **Domain allowlist**: only follow redirects to trusted domains

## Referer Header

By default, aioduct does **not** set a `Referer` header on redirect hops. Enable it on the client builder:

```rust,no_run
use aioduct::TokioClient;

let client = TokioClient::builder()
    .referer(true)
    .build()?;
```

When enabled, each redirect sets the `Referer` header to the URI of the previous request. For cross-origin hops the `Referer` is reduced to the scheme and authority (no path). Following [RFC 7231 §5.5.2](https://www.rfc-editor.org/rfc/rfc7231#section-5.5.2), aioduct never sends `Referer` on an HTTPS→HTTP downgrade, so a secure source URL is not leaked into a plaintext request.

## URL Fragments

Per [RFC 7231 §7.1.2](https://www.rfc-editor.org/rfc/rfc7231#section-7.1.2), aioduct preserves the URL fragment across redirects:

- If the `Location` header carries its own fragment, that fragment wins.
- If `Location` has no fragment, the original request's fragment is inherited by the redirect target.

Fragments are not sent to servers (they are client-side per RFC 7230), so `http::Uri` strips them. aioduct tracks the effective fragment separately and exposes it on the final response via `Response::fragment()`.
