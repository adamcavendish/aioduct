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

## Quick Start

The fastest local path is the runnable examples. They build the WASI Preview 2
guest demo, start a local HTTP server, install `WasiHttpHost` into Wasmtime,
and show host policy forwarding through a native transport:

```sh
rustup target add wasm32-wasip2
cargo run -p example-wasmtime-host-tokio
cargo run -p example-wasmtime-host-smol
cargo run -p example-wasmtime-host-compio
```

Successful output includes the guest `Status: 200` path, the expected
`error_for_status` path, and host observations like:

```text
host observations:
  GET /get HTTP/1.1 | authorization injected: yes
  POST /post HTTP/1.1 | authorization injected: yes
  GET /status/404 HTTP/1.1 | authorization injected: yes
host-owned secret header value was withheld from host output
```

The examples live under `examples/wasmtime-host`. They use local HTTP so they
can be run without external network access. Pass a component path after `--` if
you want to run an already-built WASI command component instead of the bundled
demo.

## Shape

```rust,no_run
use std::time::{Duration, Instant};

use aioduct_wasmtime::{ExactOriginPolicy, WasiHttpHost};
use http::header::{AUTHORIZATION, FORWARDED};
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
        ExactOriginPolicy::new("https://directory.local:8443")?
            .forbid_sensitive_headers()
            .deny_headers([FORWARDED])
            .deny_header_prefixes(["x-forwarded-", "proxy-"])
            .inject_header(AUTHORIZATION, secret)
            .header_limit(16 * 1024)
            .body_limit(1024 * 1024)
            .deadline(deadline),
    )
    .build()?;
# Ok(hooks)
# }
```

The hooks become active when the Wasmtime host state exposes them through
`WasiHttpView` and the component linker installs the WASI HTTP interfaces:

```rust,no_run
use aioduct_wasmtime::WasiHttpHost;
use wasmtime::component::{Component, Linker, ResourceTable};
use wasmtime::{Config, Engine, Store};
use wasmtime_wasi::p2::bindings::Command as WasiCommand;
use wasmtime_wasi::{WasiCtx, WasiCtxView, WasiView};
use wasmtime_wasi_http::WasiHttpCtx;
use wasmtime_wasi_http::p2::{WasiHttpCtxView, WasiHttpView};

struct HostState {
    table: ResourceTable,
    wasi: WasiCtx,
    http: WasiHttpCtx,
    hooks: WasiHttpHost,
}

impl WasiView for HostState {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.table,
        }
    }
}

impl WasiHttpView for HostState {
    fn http(&mut self) -> WasiHttpCtxView<'_> {
        WasiHttpCtxView {
            ctx: &mut self.http,
            table: &mut self.table,
            hooks: &mut self.hooks,
        }
    }
}

# async fn run(component_path: &std::path::Path, hooks: WasiHttpHost) -> Result<(), Box<dyn std::error::Error>> {
let mut config = Config::new();
config.wasm_component_model(true);
let engine = Engine::new(&config)?;
let component = Component::from_file(&engine, component_path)?;
let mut store = Store::new(
    &engine,
    HostState {
        table: ResourceTable::new(),
        wasi: WasiCtx::builder().build(),
        http: WasiHttpCtx::new(),
        hooks,
    },
);

let mut linker = Linker::new(&engine);
wasmtime_wasi::p2::add_to_linker_async(&mut linker)?;
wasmtime_wasi_http::p2::add_only_http_to_linker_async(&mut linker)?;
let command = WasiCommand::instantiate_async(&mut store, &component, &linker).await?;
command
    .wasi_cli_run()
    .call_run(&mut store)
    .await?
    .map_err(|()| std::io::Error::other("guest returned failure"))?;
# Ok(())
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
`compio`, pass the host transport explicitly. The `examples/wasmtime-host`
directory contains runnable Tokio, smol, and compio host examples.

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
- host-specific guest header names and families, such as `forwarded`,
  `x-forwarded-*`, or `proxy-*`, can be denied before forwarding
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

Denied header names and prefixes are also reported to Wasmtime as forbidden
field names. Reserve them for host-owned metadata that guests should not set or
depend on.

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
