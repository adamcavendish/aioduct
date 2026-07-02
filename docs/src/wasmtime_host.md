# Wasmtime Host Adapter

`aioduct-wasmtime` provides a host-side adapter for Wasmtime components that
use WASI Preview 2 `wasi:http`. The guest keeps using `aioduct::WasiClient`.
The host installs `WasiHttpHost` as the Wasmtime HTTP hook and forwards
validated requests through a native `aioduct` transport.

This split is intentional. A guest component should not choose the trust model
for a directory connector. The embedding host owns origin allow-lists, TLS
roots, insecure test mode, secret header injection, request and response size
limits, deadline budgets, and diagnostic redaction.

`aioduct-wasmtime` has an empty default feature set. Enable the host runtime
you want, such as `tokio`, `smol`, or `compio`, and enable one rustls provider
when the host transport needs TLS.

## Shape

```rust,no_run
use std::time::{Duration, Instant};

use aioduct_wasmtime::{ExactOriginPolicy, WasiHttpHost};
use http::header::AUTHORIZATION;
use http::HeaderValue;

# fn build() -> Result<WasiHttpHost, Box<dyn std::error::Error>> {
let secret = HeaderValue::from_static("Bearer host-owned-token");
let deadline = Instant::now() + Duration::from_secs(5);

let hooks = WasiHttpHost::builder()
    .transport(
        aioduct::TokioClient::builder()
            .add_root_certificates_pem_bundle(include_bytes!("ca.pem"))?
            .build()?,
    )
    .policy(
        ExactOriginPolicy::new("https://kanidm.local:8443")?
            .forbid_sensitive_headers()
            .inject_header(AUTHORIZATION, secret)
            .header_limit(16 * 1024)
            .body_limit(1024 * 1024)
            .deadline(deadline),
    )
    .build()?;
# Ok(hooks)
# }
```

The same hook can use a smol transport by building a `SmolClient` and passing
it to `.transport(...)`. The adapter can also use compio through
`CompioHostTransport`, which starts a local-runtime worker. Use a builder
factory so local-runtime connector slots are created on the worker thread:

```rust,no_run
use aioduct_wasmtime::{CompioHostTransport, ExactOriginPolicy, WasiHttpHost};

# fn build() -> Result<WasiHttpHost, Box<dyn std::error::Error>> {
let hooks = WasiHttpHost::builder()
    .transport(CompioHostTransport::from_builder_factory(
        aioduct::CompioClient::builder,
    )?)
    .policy(ExactOriginPolicy::new("http://127.0.0.1:8080")?)
    .build()?;
# Ok(hooks)
# }
```

If the `tokio` feature is explicitly enabled, the builder creates a default
Tokio transport when no explicit transport is supplied. With `smol` or
`compio`, pass the host transport explicitly.

## Runtime Line

The adapter forwards through native `HttpEngineSend<R, C>` transports where
`R: RuntimePoll` and `C: ConnectorSend`. That covers the current Send-capable
native runtimes. Compio is supported through a separate local-runtime worker
bridge because its `HttpEngineLocal` body and connection state are not `Send`:

| Host transport | Supported by `aioduct-wasmtime` | Notes |
|----------------|----------------------------------|-------|
| `TokioClient`  | Yes                              | Explicit `tokio` feature; can be default-built after feature selection |
| `SmolClient`   | Yes                              | Explicit `smol` feature and transport |
| `CompioClient` | Yes                              | Explicit `compio` feature via `CompioHostTransport` |

Browser `wasm` also does not have a host adapter. Browser WASM delegates
networking to Fetch in the browser process; there is no Wasmtime host hook to
install.

## Policy Boundary

`ExactOriginPolicy` validates each outgoing WASI request before the native
transport sees it:

- the request origin must exactly match the configured scheme, host, and port
- guest-supplied forbidden or sensitive headers can be rejected
- host-owned headers are injected only after validation
- injected header names are protected from guest override
- request and response header section sizes can be capped
- known and streaming request body sizes can be capped
- response body size can be capped
- an absolute host deadline can cap connect, first-byte, request-body write,
  response-body read, and total exchange time

Failures are mapped to WASI `wasi:http` `ErrorCode` values. Rejection observers
receive low-cardinality `RejectionReason` values suitable for metrics and logs
without including target URLs, header values, or secret material.

## TLS Operator Config

Native transports keep TLS configuration in `aioduct`, not in the guest. For
operator-provided CA bundles, prefer:

```rust,no_run
# fn build() -> Result<aioduct::TokioClient, Box<dyn std::error::Error>> {
let client = aioduct::TokioClient::builder()
    .add_root_certificates_pem_bundle(include_bytes!("ca.pem"))?
    .build()?;
# Ok(client)
# }
```

This path rejects empty input, private keys, unsupported PEM sections,
malformed PEM, and certificates rustls will not accept as trust roots.
`danger_accept_invalid_certs()` remains available for test and development
hosts, but should not be used for production connector policy.

## Out Of Scope

The adapter is deliberately transport and policy infrastructure. It does not
own directory-provider concepts, token-file loading, grant catalog semantics,
component digest validation, or application-specific startup checks.
