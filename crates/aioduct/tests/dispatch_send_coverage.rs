#![cfg(feature = "tokio")]

//! Integration tests targeting dispatch_send.rs code paths for coverage.

#[path = "dispatch/basic_features.rs"]
mod basic_features;
#[path = "dispatch/connection_pool.rs"]
mod connection_pool;
#[path = "dispatch/dispatch_misc.rs"]
mod dispatch_misc;
#[path = "dispatch/forward_etc.rs"]
mod forward_etc;
#[path = "dispatch/tcp_h2c.rs"]
mod tcp_h2c;

use basic_features::TestObserver;

use std::convert::Infallible;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use bytes::Bytes;
use http_body_util::Full;
use hyper::Response;

use aioduct::HttpEngineSend;
use aioduct::observer::{ConnectionEvent, RequestEvent, RequestObserver, RequestPhase};
use aioduct::runtime::TokioRuntime;
use aioduct::runtime::tokio_rt::TcpConnector;

use aioduct_test_server::h1::{echo, h1_server, h1_server_with};
use aioduct_test_server::h2::h2_server_with;
