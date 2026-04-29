# HTTP/2 Tuning

aioduct automatically negotiates HTTP/2 when the server supports it via ALPN during TLS. You can fine-tune HTTP/2 connection parameters using `Http2Config`.

## Usage

```rust,no_run
use aioduct::{Client, Http2Config};
use aioduct::runtime::TokioRuntime;
use std::time::Duration;

let client = Client::<TokioRuntime>::builder()
    .tls(aioduct::tls::RustlsConnector::with_webpki_roots())
    .http2(
        Http2Config::new()
            .initial_stream_window_size(2 * 1024 * 1024)
            .initial_connection_window_size(4 * 1024 * 1024)
            .max_frame_size(32_768)
            .adaptive_window(true)
            .keep_alive_interval(Duration::from_secs(20))
            .keep_alive_timeout(Duration::from_secs(10))
            .keep_alive_while_idle(true),
    )
    .build();
```

## Available Options

| Method | Description |
|--------|-------------|
| `initial_stream_window_size(u32)` | Per-stream flow control window (bytes) |
| `initial_connection_window_size(u32)` | Connection-level flow control window (bytes) |
| `max_frame_size(u32)` | Max HTTP/2 frame payload (16,384–16,777,215) |
| `adaptive_window(bool)` | Auto-tune window sizes based on BDP estimates |
| `keep_alive_interval(Duration)` | Send PING frames at this interval |
| `keep_alive_timeout(Duration)` | Close connection if PING not ACK'd within this time |
| `keep_alive_while_idle(bool)` | Send PINGs even when no active streams |
| `max_header_list_size(u32)` | Max size of received header list (bytes) |
| `max_send_buf_size(usize)` | Max write buffer size per stream (bytes) |
| `max_concurrent_reset_streams(usize)` | Max locally-reset streams tracked |

## Flow Control Window Sizing

HTTP/2 uses flow control to prevent a fast sender from overwhelming a slow receiver. The default window sizes (65,535 bytes) are conservative. For high-bandwidth or high-latency connections, larger windows improve throughput:

```rust,no_run
# use aioduct::Http2Config;
let config = Http2Config::new()
    .initial_stream_window_size(1024 * 1024)       // 1 MB per stream
    .initial_connection_window_size(2 * 1024 * 1024) // 2 MB total
    .adaptive_window(true);                         // auto-tune
```

## Keep-Alive

HTTP/2 PING frames detect dead connections before the OS does. This is especially useful for long-lived connections behind load balancers:

```rust,no_run
# use aioduct::Http2Config;
# use std::time::Duration;
let config = Http2Config::new()
    .keep_alive_interval(Duration::from_secs(30))
    .keep_alive_timeout(Duration::from_secs(10))
    .keep_alive_while_idle(true);
```

## When to Use

- **Default (no config)**: Fine for most use cases
- **Large downloads/uploads**: Increase window sizes
- **High-latency links**: Enable adaptive window
- **Long-lived connections**: Enable keep-alive PINGs
- **Behind aggressive LBs/proxies**: Short keep-alive intervals
