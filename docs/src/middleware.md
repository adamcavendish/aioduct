# Middleware

aioduct supports a middleware layer that lets you intercept and modify requests before they are sent and responses after they are received. This is useful for cross-cutting concerns like logging, metrics, authentication token refresh, or header injection.

## The Middleware Trait

```rust,no_run
pub trait Middleware: Send + Sync + 'static {
    fn on_request(&self, request: &mut http::Request<AioductBody>, uri: &Uri) { }
    fn on_response(&self, response: &mut http::Response<AioductBody>, uri: &Uri) { }
}
```

Both methods have default no-op implementations, so you only need to override what you use.

## Using Closures

For simple request-only middleware, you can pass a closure directly:

```rust,no_run
use aioduct::Client;
use aioduct::runtime::TokioRuntime;

let client = Client::<TokioRuntime>::builder()
    .middleware(|req: &mut http::Request<aioduct::AioductBody>, _uri: &http::Uri| {
        req.headers_mut().insert(
            http::header::HeaderName::from_static("x-custom"),
            http::header::HeaderValue::from_static("value"),
        );
    })
    .build();
```

## Using a Struct

For middleware that needs to modify responses or maintain state, implement the trait on a struct:

```rust,no_run
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use aioduct::{Client, AioductBody, Middleware};
use aioduct::runtime::TokioRuntime;

struct RequestCounter {
    count: Arc<AtomicU64>,
}

impl Middleware for RequestCounter {
    fn on_request(&self, _req: &mut http::Request<AioductBody>, _uri: &http::Uri) {
        self.count.fetch_add(1, Ordering::Relaxed);
    }
}

let counter = Arc::new(AtomicU64::new(0));
let client = Client::<TokioRuntime>::builder()
    .middleware(RequestCounter { count: counter.clone() })
    .build();
```

## Stacking Multiple Middleware

You can add multiple middleware layers. They execute in order:

- **Request hooks** run first-to-last (in the order they were added).
- **Response hooks** run last-to-first (reverse order).

```rust,no_run
use aioduct::Client;
use aioduct::runtime::TokioRuntime;

let client = Client::<TokioRuntime>::builder()
    .middleware(|req: &mut http::Request<aioduct::AioductBody>, _uri: &http::Uri| {
        // Runs first on request
        req.headers_mut().insert(
            http::header::HeaderName::from_static("x-trace-id"),
            http::header::HeaderValue::from_static("abc123"),
        );
    })
    .middleware(|req: &mut http::Request<aioduct::AioductBody>, _uri: &http::Uri| {
        // Runs second on request
        req.headers_mut().insert(
            http::header::HeaderName::from_static("x-auth"),
            http::header::HeaderValue::from_static("Bearer tok"),
        );
    })
    .build();
```

## When Middleware Runs

Middleware hooks run at these points in the request lifecycle:

1. The request is fully built (headers, body, query params applied).
2. **`on_request`** is called for each middleware in order.
3. The request is sent over the connection.
4. The response is received.
5. **`on_response`** is called for each middleware in reverse order.
6. Decompression is applied (if enabled).
7. The response is returned to the caller.

Note that middleware runs on each individual request, including redirect hops.
