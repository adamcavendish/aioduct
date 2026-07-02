# Wasmtime host examples

These examples run the `example-wasi-p2-demo` guest component under Wasmtime and
service the guest's `aioduct::WasiClient` calls with host-owned
`aioduct-wasmtime` policy.

The host examples use a local HTTP server, so they do not depend on external
network access. The server records only low-value request facts in the example
output: request lines and whether the host-injected authorization header was
present. It does not print the injected secret value.

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

Each example builds `example-wasi-p2-demo` for `wasm32-wasip2`, starts a local
HTTP server, wires `WasiHttpHost` into a Wasmtime component `Linker`, and runs
the guest command.

You can also pass an existing component path:

```sh
cargo run -p example-wasmtime-host-tokio -- target/wasm32-wasip2/debug/example-wasi-p2-demo.wasm
```

## Transports

| Example | Host forwarding transport |
|---------|---------------------------|
| `example-wasmtime-host-tokio` | `TokioClient` |
| `example-wasmtime-host-smol` | `SmolClient` |
| `example-wasmtime-host-compio` | `CompioHostTransport` wrapping `CompioClient` |

The Wasmtime embedder and the native forwarding transport are separate choices.
The guest remains a WASI Preview 2 component using `aioduct::WasiClient`; the
host owns origin validation, limits, deadlines, and secret header injection.

## Expected output

The exact local port will differ, but successful output includes:

```text
aioduct-wasmtime tokio host demo
origin: http://127.0.0.1:...
guest stdout:
=== GET http://127.0.0.1:.../get ===
Status: 200
...
Expected error:
host observations:
  GET /get HTTP/1.1 | authorization injected: yes
  POST /post HTTP/1.1 | authorization injected: yes
  GET /status/404 HTTP/1.1 | authorization injected: yes
host-owned secret header value was withheld from host output
```

If Cargo reports that `wasm32-wasip2` is unavailable, run the `rustup target add`
command above and retry.
