# HTTP/3

aioduct has experimental HTTP/3 support via upstream
[h3](https://crates.io/crates/h3) and [quinn](https://crates.io/crates/quinn).
The project deliberately does not maintain an `h3` fork. Protocol behavior
that aioduct cannot implement against the released upstream API without
weakening validation or stream-lifecycle guarantees is deferred and fails
closed where aioduct can identify it.

The Tokio-only
[`http3-streaming-upload`](https://github.com/adamcavendish/aioduct/tree/main/examples/tokio/http3-streaming-upload)
example demonstrates ordered upload chunks, producer write timeouts, and
fail-closed request trailers against a local HTTP/3 server.

## Feature Flag

Enable the `http3` transport feature with the rustls backend and a rustls crypto provider:

```toml
[dependencies]
aioduct = { version = "0.2.4", features = ["tokio", "http3", "rustls", "rustls-ring"] }
```

To use AWS-LC instead of ring, select the AWS-LC rustls provider:

```toml
[dependencies]
aioduct = { version = "0.2.4", features = ["tokio", "http3", "rustls", "rustls-aws-lc-rs"] }
```

The `http3` feature only selects the QUIC/HTTP/3 transport dependencies. Today HTTP/3 still requires the rustls backend because quinn uses rustls for QUIC TLS; choose exactly one of `rustls-ring` or `rustls-aws-lc-rs`.

## Usage

There are two modes for HTTP/3:

### Always-H3 Mode

Force all HTTPS requests through QUIC/HTTP/3:

```rust,no_run
use aioduct::TokioClient;

// All HTTPS requests will use HTTP/3
let client = TokioClient::with_http3()?;
```

Or via the builder:

```rust,no_run
use aioduct::TokioClient;
use aioduct::tls::RustlsConnector;

let client = TokioClient::builder()
    .tls(RustlsConnector::with_webpki_roots())
    .http3(true)?
    .build()?;
```

### Alt-Svc Auto-Upgrade Mode

Start with HTTP/1.1 or HTTP/2 over TCP, and automatically upgrade to HTTP/3 when the server advertises it via the `Alt-Svc` header:

```rust,no_run
use aioduct::TokioClient;

// First request uses TCP; upgrades to QUIC when Alt-Svc is seen
let client = TokioClient::with_alt_svc_h3()?;
```

Or via the builder:

```rust,no_run
use aioduct::TokioClient;
use aioduct::tls::RustlsConnector;

let client = TokioClient::builder()
    .tls(RustlsConnector::with_webpki_roots())
    .alt_svc_h3(true)?
    .build()?;
```

> **Important:** `.tls()` must be called before `.http3(true)` or `.alt_svc_h3(true)` when you provide a custom TLS connector because HTTP/3 reuses that rustls configuration to build the QUIC endpoint.

## Alt-Svc Protocol Upgrade

When Alt-Svc auto-upgrade is enabled (`.alt_svc_h3(true)` or `with_alt_svc_h3()`):

1. The first request to a new origin goes over TCP (HTTP/1.1 or HTTP/2 via ALPN).
2. If the response includes an `Alt-Svc` header advertising `h3` (e.g., `Alt-Svc: h3=":443"; ma=86400`), the client caches this.
3. Subsequent requests to the same origin use QUIC/HTTP/3 instead of TCP.
4. The cache respects `ma` (max-age) — entries expire after the specified duration (default 24 hours).
5. `Alt-Svc: clear` removes cached entries, reverting to TCP for that origin.

The Alt-Svc cache supports alternate hosts and ports. For example, `h3="alt.example.com:8443"` routes QUIC traffic to a different endpoint while keeping the original host for SNI.

## How It Works

When HTTP/3 is enabled (either mode):

1. **HTTPS requests** are sent over QUIC using the quinn transport. The client opens a QUIC connection, performs the TLS 1.3 handshake, and sends the request via the h3 protocol.
2. **HTTP requests** (plain) continue to use TCP-based HTTP/1.1 or HTTP/2 as usual.
3. **Connection pooling** works for QUIC connections the same way it does for TCP. Reuse requires the full six-field pool key to match: scheme, authority, protocol hint, proxy route, any forced transport address, and the effective HTTP/3 endpoint. This keeps exact H3, ordinary TCP negotiation, proxied routes, and distinct Alt-Svc endpoints separate. Like HTTP/2, HTTP/3 multiplexes streams over a single connection.

When HTTP/3 is **not** enabled (default), the client uses TCP with HTTP/1.1 or HTTP/2 negotiated via ALPN, even for HTTPS.

## 0-RTT (Early Data)

Aioduct does not currently send HTTP/3 requests as 0-RTT early data. The
`h3_zero_rtt` setter remains available for compatibility, but enabling it fails
client construction with `Error::Unsupported`:

```rust,no_run
use aioduct::TokioClient;

let result = TokioClient::builder()
    .tls(aioduct::tls::RustlsConnector::with_webpki_roots())
    .http3(true)?
    .h3_zero_rtt(true)
    .build();

assert!(matches!(result, Err(aioduct::Error::Unsupported(_))));
# Ok::<(), aioduct::Error>(())
```

This fails closed because a correct implementation must validate remembered
peer SETTINGS and handle early-data rejection without weakening the request
replay policy. Those guarantees are not available through the released
upstream `h3` API, and aioduct does not maintain an `h3` fork. Leave 0-RTT
disabled, which is the default.

## Request Upload Lifecycle

HTTP/3 request uploads use data frames followed by the request-stream FIN:

```text
headers -> data* -> FIN
```

Request upload and response receipt run concurrently. A final response does not
implicitly cancel the upload. Once response headers are handed to the caller,
aioduct supervises the remaining upload in a detached task until it completes,
the peer sends STOP_SENDING, the request is canceled, or the upload fails.

If `write_timeout` expires before response handoff, `send()` returns
`Error::WriteTimeout`. After response handoff, the response remains available;
the timeout cancels the detached request send direction but cannot be surfaced
through the already-completed `send()` future. Dropping the response before its
body completes also cancels an unfinished upload.

Request trailers are not supported. If a trailer is observed before response
handoff, `send()` returns `Error::Unsupported` even when a final response became
ready on the same poll. If the body emits a trailer only after response handoff,
the detached upload is failed and its send direction is canceled, but the
already-completed `send()` future cannot be changed retroactively. Response
trailers similarly produce `Error::Unsupported` when response-body consumption
reaches them.

## Deferred Protocol Capabilities

The following capabilities are intentionally deferred while aioduct uses the
upstream `h3` crate. Some require protocol state that upstream does not expose;
others require additional aioduct lifecycle and validation work before they can
be enabled safely:

- **Strict malformed-field handling** — aioduct validates outgoing URI
  authority and Host consistency before calling upstream `h3`, and validates
  decoded regular fields it receives. Wire-level pseudo-field ordering,
  duplication, and malformed field-section metadata remain upstream-owned.
  Authority-free `OPTIONS *` forwarding fails closed because upstream cannot
  encode its pseudo-fields without inventing an authority.
- **Request and response trailers** — upstream exposes trailer primitives, but
  aioduct does not yet provide the required end-to-end forwarding, validation,
  timeout, and cancellation guarantees. Request trailers observed before
  response handoff and response trailers reached while consuming the body
  return `Error::Unsupported`. A request trailer emitted by a detached upload
  after response handoff cannot be surfaced through the already-completed
  `send()` future; aioduct fails the upload and cancels its send direction
  instead.
- **Extended CONNECT** — HTTP/3 CONNECT and forwarded HTTP/3 extended CONNECT
  metadata are rejected before opening a request stream.
- **0-RTT** — `h3_zero_rtt(true)` is retained for API compatibility, but
  building that client returns `Error::Unsupported`. Validating remembered peer
  SETTINGS is required before this can be safe.
- **GOAWAY-based replay** — upstream `h3` reports `RemoteClosing` without
  exposing the validated GOAWAY stream-ID cutoff together with the affected
  request stream. Public APIs can exercise GOAWAY on the wire, but cannot supply
  the per-request boundary evidence needed to authorize replay. Connection-
  closing evidence without a specific protocol code therefore remains ambiguous
  and never authorizes transparent replay. Explicit `H3_REQUEST_REJECTED` stream
  errors may still prove that a reproducible request was not processed.

`H3_VERSION_FALLBACK` is distinct from ambiguous connection closure, but it
does not prove that a non-idempotent operation had no application effect. In
opportunistic Alt-Svc mode, aioduct permits that fallback once only for a
reproducible idempotent request. Buffered `POST` requests and one-shot bodies
remain terminal. The fallback consumes the same internal transport-recovery
budget as other automatic recovery, and always-H3 mode remains terminal instead
of silently changing protocol.

These boundaries avoid promising behavior that would require maintaining a
private fork of the HTTP/3 protocol implementation.

## Limitations

- **Experimental** — the h3 ecosystem is pre-1.0.
- **No fallback** — in always-h3 mode, if the server doesn't support QUIC, the request fails rather than falling back to TCP. Use Alt-Svc mode or the default (non-h3) client for servers where QUIC support is uncertain.
- **Tokio transport** — aioduct's current Quinn transport requires the `tokio`
  feature and an active Tokio runtime when HTTP/3 is enabled. The generic
  `RuntimePoll` builder methods remain available for source compatibility, but
  enabling HTTP/3 on another runtime returns a setup error. Forwarded HTTP/3
  requests without a configured QUIC endpoint are rejected before network I/O.
- **rustls required today** — future TLS backend work may change the available combinations, but current HTTP/3 support composes with rustls provider features.
