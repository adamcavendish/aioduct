# Complete Tokio observability reference

This example is the backend-neutral reference for the native Tokio
observability API. It starts a local HTTP/1.1 fixture and exercises one
request-level `Observer` against all public native callback families:

| Fixture | What to look for |
| --- | --- |
| `/ok` twice | request start, selected response headers, response-body end, request finish, connection acquisition, and connection establishment/lifecycle |
| `/redirect` | one logical request with multiple `AttemptEvent` values and intermediate-response cleanup |
| `/retry` | retryable 503 disposition, cleanup, backoff, and the final response |
| `POST /upload` | `RequestBodyEvent::Ended` and the normal response lifecycle |
| failed streaming upload | `RequestBodyEvent::Failed`, an attempt failure, and a request failure before response selection |
| raw `/body-error` peer | headers are selected first; the later body error is reported as a body failure and a request failure, without retrying the selected response |
| `/sse` | independent SSE `Started`/`Ended` lifecycle and `SseStatisticsSnapshot` values; there are no per-message or `Pending` callbacks |
| dropping an SSE stream early | SSE `Abandoned` is independent from HTTP/body `ConsumerAbandoned` |
| SSE payload limit | borrowed SSE decoder error and `ResponseSseEvent::Error` |
| `/hang` plus outer timeout | `ExecutionAbandoned` when no response is selected |
| `.disable_observation()` | per-request opt-out; `Observer::begin` is not called for that request |
| `/upgrade` | HTTP/1.1 `Upgrade`, response handoff, and independent channel I/O lifecycle |
| `UpgradedSend::into_inner()` | explicit channel detachment without exposing aioduct's private wrapper |
| ordinary `CONNECT` | HTTP/1.1 CONNECT handoff, distinct from HTTP/1.1 Upgrade |

The observer asks for every `Interest` bit. By default, the terminal renderer
prints a compact scenario report and uses color when stdout is an interactive
terminal:

```text
01  ordinary GET · start, headers, body, finish
────────────────────────────────────────
    • GET /ok
    · attempt 1 · GET /ok
    ✓ response  200 OK HTTP/1.1 · transport
    ✓ body     ended
    ✓ finished ended · attempts=1 · response=200 · data=14 B
    → body 14 · "hello from /ok"
```

It intentionally omits raw `Debug` structures and `Instant` values. The
observer still does not report every body frame, successful SSE message,
`Pending` poll, successful channel read/write/flush, or comment line: those are
deliberately not observability callbacks because their frequency would make the
default instrumentation expensive. The `app` lines only show application
consumption in this example; selected body/SSE/channel summaries are delivered
at their defined lifecycle milestones.

Pass `--trace` after `--` to render every subscribed callback with the same
semantic format:

```bash
cargo run -p example-tokio-observability -- --trace
```

Color is automatically disabled when output is redirected. Set `NO_COLOR` to
disable it explicitly.

The example uses compact `println!` renderers only to keep the public contract
visible. A real application owns the `Observer`, `RequestObservation`, and
`ConnectionObservation` implementations and can send the same events to
tracing, OTLP, metrics, logs, or multiple backends without changing aioduct.
Errors are borrowed only for the synchronous callback and are never cloned by
aioduct.

## Run it

From the Rust workspace directory:

```bash
cd /Volumes/files/repo/adamcavendish/aioduct/aioduct
cargo run -p example-tokio-observability
```

For a compile-only check:

```bash
cargo check -p example-tokio-observability
```

The server binds to a random loopback port and uses `localhost` in request
URLs. Therefore no external service, API key, or network access is required.
The exact connection phase sequence can vary with the host resolver and
address-family configuration; pool hits, redirects, retries, body outcomes,
SSE lifecycle, and both HTTP/1.1 handoff modes are deterministic.
