#![cfg(feature = "tokio")]

use std::convert::Infallible;
use std::future::Future;
use std::io;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use bytes::Bytes;
use http_body_util::Full;
use hyper::Response;

use aioduct::HttpEngineSend;
use aioduct::runtime::tokio_rt::TcpConnector;
use aioduct::runtime::{ConnectorSend, TokioRuntime};

fn client() -> HttpEngineSend<TokioRuntime, TcpConnector> {
    HttpEngineSend::builder()
        .pool_idle_timeout(Duration::from_secs(60))
        .build()
        .unwrap()
}

fn client_with_timeout(t: Duration) -> HttpEngineSend<TokioRuntime, TcpConnector> {
    HttpEngineSend::builder()
        .pool_idle_timeout(Duration::from_secs(60))
        .timeout(t)
        .build()
        .unwrap()
}

#[path = "conn_lifecycle/h1_reuse.rs"]
mod h1_reuse;
#[path = "conn_lifecycle/h2_reuse.rs"]
mod h2_reuse;
#[path = "conn_lifecycle/pool_behavior.rs"]
mod pool_behavior;
#[path = "conn_lifecycle/pool_key.rs"]
mod pool_key;

use pool_key::SlowFirstConnector;
