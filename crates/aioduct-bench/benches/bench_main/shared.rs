use std::net::SocketAddr;
use std::sync::{LazyLock, OnceLock};
use std::time::Duration;

use bytes::Bytes;
use http_body_util::Full;
use tokio::runtime::Runtime;

use aioduct_bench::*;

// ── Type aliases ────────────────────────────────────────────────────────────

pub type AioductClient = aioduct::HttpEngineSend<
    aioduct::runtime::TokioRuntime,
    aioduct::runtime::tokio_rt::TcpConnector,
>;

pub type HyperUtilH1Client = hyper_util::client::legacy::Client<
    hyper_util::client::legacy::connect::HttpConnector,
    Full<Bytes>,
>;

pub type HyperUtilH2Client = hyper_util::client::legacy::Client<
    hyper_util::client::legacy::connect::HttpConnector,
    Full<Bytes>,
>;

// ── Shared runtime ──────────────────────────────────────────────────────────

pub static RT: LazyLock<Runtime> = LazyLock::new(|| Runtime::new().unwrap());

// ── Shared clients ──────────────────────────────────────────────────────────

pub static AIODUCT_H1: LazyLock<AioductClient> = LazyLock::new(aioduct::HttpEngineSend::new);

pub static AIODUCT_H2: LazyLock<AioductClient> = LazyLock::new(|| {
    aioduct::HttpEngineSend::builder()
        .http2_prior_knowledge()
        .http2(
            aioduct::Http2Config::new()
                .initial_stream_window_size(2 * 1024 * 1024)
                .initial_connection_window_size(4 * 1024 * 1024)
                .max_concurrent_reset_streams(1024),
        )
        .build()
        .unwrap()
});

pub static AIODUCT_H1_LARGE_POOL: LazyLock<AioductClient> = LazyLock::new(|| {
    aioduct::HttpEngineSend::builder()
        .pool_max_idle_per_host(100)
        .build()
        .unwrap()
});

pub static REQWEST: LazyLock<reqwest::Client> = LazyLock::new(reqwest::Client::new);

pub static HYPER_UTIL_H1: LazyLock<HyperUtilH1Client> = LazyLock::new(|| {
    hyper_util::client::legacy::Client::builder(hyper_util::rt::TokioExecutor::new())
        .build_http::<Full<Bytes>>()
});

pub static HYPER_UTIL_H2: LazyLock<HyperUtilH2Client> = LazyLock::new(|| {
    hyper_util::client::legacy::Client::builder(hyper_util::rt::TokioExecutor::new())
        .http2_only(true)
        .pool_idle_timeout(Duration::from_secs(90))
        .http2_initial_stream_window_size(2 * 1024 * 1024)
        .http2_initial_connection_window_size(4 * 1024 * 1024)
        .http2_max_concurrent_reset_streams(1024)
        .build_http::<Full<Bytes>>()
});

#[cfg(not(target_env = "musl"))]
pub static ISAHC: LazyLock<isahc::HttpClient> = LazyLock::new(|| isahc::HttpClient::new().unwrap());

// ── Shared servers ──────────────────────────────────────────────────────────

static H1_SMALL_SERVER: OnceLock<SocketAddr> = OnceLock::new();
static H1_64K_SERVER: OnceLock<SocketAddr> = OnceLock::new();
static H1_1M_SERVER: OnceLock<SocketAddr> = OnceLock::new();
static H1_ECHO_SERVER: OnceLock<SocketAddr> = OnceLock::new();
static H2C_SMALL_SERVER: OnceLock<SocketAddr> = OnceLock::new();
static H2C_64K_SERVER: OnceLock<SocketAddr> = OnceLock::new();
static H2C_1M_SERVER: OnceLock<SocketAddr> = OnceLock::new();
static H2C_ECHO_SERVER: OnceLock<SocketAddr> = OnceLock::new();
static SSE_SERVER: OnceLock<SocketAddr> = OnceLock::new();
static RANGE_SERVER: OnceLock<SocketAddr> = OnceLock::new();

pub fn h1_small_addr() -> SocketAddr {
    *H1_SMALL_SERVER.get_or_init(|| RT.block_on(start_http1_server(Bytes::from(JSON_BODY))))
}

pub fn h1_64k_addr() -> SocketAddr {
    *H1_64K_SERVER
        .get_or_init(|| RT.block_on(start_http1_server(Bytes::from(vec![b'x'; BODY_64K]))))
}

pub fn h1_1m_addr() -> SocketAddr {
    *H1_1M_SERVER.get_or_init(|| RT.block_on(start_http1_server(Bytes::from(vec![b'x'; BODY_1M]))))
}

pub fn h1_echo_addr() -> SocketAddr {
    *H1_ECHO_SERVER.get_or_init(|| RT.block_on(start_echo_server()))
}

pub fn h2c_small_addr() -> SocketAddr {
    *H2C_SMALL_SERVER.get_or_init(|| RT.block_on(start_h2c_server(Bytes::from(JSON_BODY))))
}

pub fn h2c_64k_addr() -> SocketAddr {
    *H2C_64K_SERVER.get_or_init(|| RT.block_on(start_h2c_server(Bytes::from(vec![b'x'; BODY_64K]))))
}

pub fn h2c_1m_addr() -> SocketAddr {
    *H2C_1M_SERVER.get_or_init(|| RT.block_on(start_h2c_server(Bytes::from(vec![b'x'; BODY_1M]))))
}

pub fn h2c_echo_addr() -> SocketAddr {
    *H2C_ECHO_SERVER.get_or_init(|| RT.block_on(start_h2c_echo_server()))
}

pub fn sse_addr() -> SocketAddr {
    *SSE_SERVER.get_or_init(|| RT.block_on(start_sse_server(SSE_EVENT_COUNT)))
}

pub fn range_addr() -> SocketAddr {
    *RANGE_SERVER.get_or_init(|| RT.block_on(start_range_server(BODY_1M)))
}

pub fn h1_small_url() -> String {
    format!("http://{}/", h1_small_addr())
}
pub fn h1_64k_url() -> String {
    format!("http://{}/", h1_64k_addr())
}
pub fn h1_1m_url() -> String {
    format!("http://{}/", h1_1m_addr())
}
pub fn h1_echo_url() -> String {
    format!("http://{}/", h1_echo_addr())
}
pub fn h2c_small_url() -> String {
    format!("http://{}/", h2c_small_addr())
}
pub fn h2c_64k_url() -> String {
    format!("http://{}/", h2c_64k_addr())
}
pub fn h2c_1m_url() -> String {
    format!("http://{}/", h2c_1m_addr())
}
pub fn h2c_echo_url() -> String {
    format!("http://{}/", h2c_echo_addr())
}
pub fn sse_url() -> String {
    format!("http://{}/", sse_addr())
}
pub fn range_url() -> String {
    format!("http://{}/data", range_addr())
}
