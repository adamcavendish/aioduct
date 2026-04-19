# Introduction

**aioduct** is an async-native Rust HTTP client built directly on [hyper 1.x](https://hyper.rs/) — with no hyper-util dependency and no legacy APIs.

## Motivation

The Rust HTTP client ecosystem has a gap:

- **reqwest** depends on hyper-util's `legacy::Client`, which wraps hyper 0.x-style patterns over hyper 1.x. It carries years of backwards-compatibility baggage.
- **hyper-util** itself labels its client as "legacy" — the hyper team acknowledges it's not the right long-term answer.
- **hyper 1.x** was redesigned to be a minimal HTTP protocol engine with clean connection-level primitives (`hyper::client::conn::http1`, `hyper::client::conn::http2`), but no production client uses it this way today.

aioduct fills this gap: a production-quality HTTP client that uses hyper 1.x **the way it was intended** — as a protocol engine you drive yourself, with your own connection pool, TLS, and runtime integration.

## Design Principles

1. **No hyper-util** — custom executor and IO adapters directly against `hyper::rt` traits. ~50 lines each, zero legacy baggage.
2. **No default runtime** — the core crate is pure types, traits, and logic. Opt into a runtime via feature flags.
3. **No default TLS** — plain HTTP works out of the box. Enable `rustls` for HTTPS.
4. **Runtime-agnostic core** — `Client<R: Runtime>` is generic over a `Runtime` trait. All pool, TLS, and HTTP logic works with any conforming runtime.
5. **HTTP/3 as experimental** — h3 + h3-quinn behind a feature flag.

## Comparison with reqwest

| Feature             | reqwest                       | aioduct                       |
|---------------------|-------------------------------|-------------------------------|
| hyper version       | 1.x via hyper-util legacy     | 1.x direct                    |
| hyper-util          | Required                      | Not used                      |
| Runtime             | tokio only                    | tokio / smol / compio / wasm  |
| TLS                 | rustls or native-tls          | rustls                        |
| HTTP/3              | Experimental                  | Experimental                  |
| io_uring            | No                            | Via compio feature             |
| Connection pool     | hyper-util legacy              | Custom, built for h1/h2/h3   |
| Cookie jar          | Yes                           | Yes                           |
| SSE streaming       | No (manual)                   | Built-in                      |
| Rate limiting       | No                            | Built-in                      |
| HTTP caching        | No                            | Built-in                      |
| Middleware          | Via tower                     | Built-in + tower              |
