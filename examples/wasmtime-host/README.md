# Wasmtime host examples

These examples run the `example-wasi-p2-demo` guest component under Wasmtime and
service the guest's `aioduct::WasiClient` calls with host-owned
`aioduct::wasmtime` policy.

They deliberately show two independent observation chains. The guest prints
the request-level facts visible through `WasiClient`; the host prints the
native forwarding transport's request and connection facts. Host-managed WASI
does not expose guest-to-native one-to-one correlation, DNS, negotiated
protocol, or wire boundaries to the guest observer, so the examples do not
invent those events.

The host examples use a local HTTP server, so they do not depend on external
network access. The fixture records only low-value request facts: request lines
and whether the host-injected authorization header was present. The host
observer prints native request/connection lifecycle facts, but never prints the
injected secret value.

## Run

Install the WASI Preview 2 target once:

```sh
rustup target add wasm32-wasip2
```

Then run any host transport:

```sh
cargo run -p example-wasmtime-host-tokio
cargo run -p example-wasmtime-host-smol
cargo run -p example-wasmtime-host-compio
```

The default is the same compact summary view as the runtime observability
examples. To render every subscribed callback, use trace mode:

```sh
cargo run -p example-wasmtime-host-tokio -- --trace
```

Each example builds `example-wasi-p2-demo` for `wasm32-wasip2`, starts a local
HTTP server, wires `WasiHttpHost` into a Wasmtime component `Linker`, and runs
the guest command.

You can also pass an existing component path:

```sh
cargo run -p example-wasmtime-host-tokio -- target/wasm32-wasip2/debug/example-wasi-p2-demo.wasm
```

The component path and `--trace` can be combined in either order.

## Transports

| Example | Host forwarding transport |
|---------|---------------------------|
| `example-wasmtime-host-tokio` | `TokioClient` |
| `example-wasmtime-host-smol` | `SmolClient` |
| `example-wasmtime-host-compio` | `CompioHostTransport` wrapping `CompioClient` |

The Wasmtime embedder and the native forwarding transport are separate choices.
The guest remains a WASI Preview 2 component using `aioduct::WasiClient`; the
host owns origin validation, host-specific header denial, limits, deadlines, and
secret header injection. The shared example policy denies guest-supplied
`forwarded`, `x-forwarded-*`, and `proxy-*` headers before forwarding.

## Expected output

The exact local port will differ, but successful output includes:

```text
aioduct observability · wasmtime-host-tokio · WASI host forwarding transport
summary mode · use --trace for callback-level detail
────────────────────────────────────────────────────────
01  native host transport · callback lifecycle
────────────────────────────────────────
    → local fixture · http://127.0.0.1:...
    • GET /get
    ✓ response  200 OK HTTP/1.1 · transport
    ✓ finished ended · attempts=1 · response=200
    ...

02  WASI guest · request-level result
────────────────────────────────────────
    → GET /get · 200 OK
    → POST /post · 200 OK
    → GET /status/404 · error_for_status · HTTP status error: 404 Not Found

03  host policy · local forwarding fixture
────────────────────────────────────────
    → wasmtime-host-tokio · origin=http://127.0.0.1:... · secret value redacted
    → GET /get HTTP/1.1 · authorization injected=yes
    ...
✓ complete
```

The example keeps the guest and host lifecycles separate while rendering them
with the same summary/trace layout. Summary mode shows guest application
results; `--trace` also renders the captured guest callback lines. Host
callbacks describe the native forwarding transport and its physical
connections. The adapter does not invent a guest-to-host correlation ID or
copy host DNS, negotiated protocol, or wire events into the guest stream.

If Cargo reports that `wasm32-wasip2` is unavailable, run the `rustup target add`
command above and retry.
