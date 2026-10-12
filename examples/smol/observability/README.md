# Smol observability example

`example-smol-observability` is the smol counterpart to the complete native
Tokio reference. One binary installs a backend-neutral `Observer` and asks for
the same complete scenario set as `example-tokio-observability`: ordinary
requests, pool reuse, redirect, retry, upload success/failure, selected-body
failure, SSE lifecycle/abandonment/decoder failure, cancellation, opt-out,
HTTP/1.1 Upgrade, channel detachment, and ordinary CONNECT. Its summary and
`--trace` output use the same renderer and event vocabulary as the Tokio
example. The observer only prints facts; applications may route the same
callbacks to tracing, OpenTelemetry, metrics, or several backends.

Run it with:

```bash
cargo run -p example-smol-observability
# Add --trace for callback-level detail.
cargo run -p example-smol-observability -- --trace
```

The example starts a deterministic local HTTP/1.1 fixture, so it needs no
network access or API key. Use `NO_COLOR=1` when capturing output in a log or
CI system.
