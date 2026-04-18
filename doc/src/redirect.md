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

- **301, 302, 303** → method changes to `GET`, body is dropped
- **307, 308** → method and body are preserved

## No Redirects

```rust,no_run
use aioduct::{Client, RedirectPolicy};
use aioduct::runtime::TokioRuntime;

let client = Client::<TokioRuntime>::builder()
    .redirect_policy(RedirectPolicy::none())
    .build();
```

## Limited Redirects

```rust,no_run
use aioduct::{Client, RedirectPolicy};
use aioduct::runtime::TokioRuntime;

// Also available via the shorthand:
let client = Client::<TokioRuntime>::builder()
    .max_redirects(5)
    .build();

// Equivalent to:
let client = Client::<TokioRuntime>::builder()
    .redirect_policy(RedirectPolicy::limited(5))
    .build();
```

## Custom Policy

The custom callback receives the current URI, next (redirect target) URI, status code, and HTTP method. Return `RedirectAction::Follow` to follow the redirect, or `RedirectAction::Stop` to stop and return the redirect response.

```rust,no_run
use aioduct::{Client, RedirectAction, RedirectPolicy};
use aioduct::runtime::TokioRuntime;

let client = Client::<TokioRuntime>::builder()
    .redirect_policy(RedirectPolicy::custom(|current, next, status, method| {
        // Only follow redirects that stay on the same host
        if current.host() == next.host() {
            RedirectAction::Follow
        } else {
            RedirectAction::Stop
        }
    }))
    .build();
```

### Use Cases for Custom Policies

- **Same-origin only**: prevent redirects to external domains
- **HTTPS-only**: reject downgrades from HTTPS to HTTP
- **Logging**: log each redirect decision while still following
- **Domain allowlist**: only follow redirects to trusted domains
