# Compio observability example

`example-compio-observability` is the completion-based runtime example. One
binary installs the same backend-neutral observer contract as the Tokio and
smol examples and exercises the same complete scenario set as
`example-tokio-observability`: ordinary requests, pool reuse, redirect, retry,
upload success/failure, selected-body failure, SSE lifecycle/abandonment/decoder
failure, cancellation, opt-out, HTTP/1.1 Upgrade, channel detachment, and
ordinary CONNECT. Summary and `--trace` output use the same renderer and event
vocabulary as the Tokio reference, without propagating the observer type into
response or stream values.

Run it with:

```bash
cargo run -p example-compio-observability
# Add --trace for callback-level detail.
cargo run -p example-compio-observability -- --trace
```

The example starts a deterministic local HTTP/1.1 fixture, so it needs no
network access or API key. `NO_COLOR=1` disables ANSI colors for CI logs.
