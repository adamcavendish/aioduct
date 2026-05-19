#![cfg(feature = "tokio")]

use std::convert::Infallible;
use std::future::Future;
use std::io;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use bytes::Bytes;
use http_body_util::Full;
use hyper::Response;

use aioduct::HttpEngineSend;
use aioduct::runtime::tokio_rt::TcpConnector;
use aioduct::runtime::{ConnectorSend, TokioRuntime};

use aioduct_test_server::h1::{h1_server, h1_server_with};

// =============================================================================
// CountingConnector — wraps TcpConnector with a connection counter
// =============================================================================

#[derive(Clone)]
struct CountingConnector {
    inner: TcpConnector,
    count: Arc<AtomicU32>,
}

impl CountingConnector {
    fn new() -> Self {
        Self {
            inner: TcpConnector,
            count: Arc::new(AtomicU32::new(0)),
        }
    }

    #[allow(dead_code)]
    fn count(&self) -> u32 {
        self.count.load(Ordering::Relaxed)
    }
}

impl ConnectorSend for CountingConnector {
    type Stream = <TcpConnector as ConnectorSend>::Stream;

    fn connect(&self, addr: SocketAddr) -> impl Future<Output = io::Result<Self::Stream>> + Send {
        self.count.fetch_add(1, Ordering::Relaxed);
        self.inner.connect(addr)
    }

    fn connect_bound(
        &self,
        addr: SocketAddr,
        local: IpAddr,
    ) -> impl Future<Output = io::Result<Self::Stream>> + Send {
        self.count.fetch_add(1, Ordering::Relaxed);
        self.inner.connect_bound(addr, local)
    }

    fn from_std_tcp(&self, stream: std::net::TcpStream) -> io::Result<Self::Stream> {
        self.inner.from_std_tcp(stream)
    }
}

// =============================================================================
// Tests
// =============================================================================

#[tokio::test]
async fn tokio_engine_alias_works() {
    let (addr, _counter) = h1_server().await;
    let client = aioduct::TokioEngine::new();
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
}

#[test]
fn tokio_client_and_engine_are_same_type() {
    assert_eq!(
        std::any::TypeId::of::<aioduct::TokioClient>(),
        std::any::TypeId::of::<aioduct::TokioEngine>(),
    );
}

#[tokio::test]
async fn builder_auto_wires_default_resolver() {
    let (addr, _counter) = h1_server().await;
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .build()
        .unwrap();
    // Use localhost (not 127.0.0.1) to exercise the resolver
    let resp = client
        .get(&format!("http://localhost:{}/", addr.port()))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
}

#[tokio::test]
async fn custom_connector_send_integration() {
    let (addr, _counter) = h1_server().await;
    let connector = CountingConnector::new();
    let count = connector.count.clone();
    let client =
        HttpEngineSend::<TokioRuntime, CountingConnector>::builder_with_connector(connector)
            .no_connection_reuse()
            .build()
            .unwrap();
    for _ in 0..3 {
        let resp = client
            .get(&format!("http://{addr}/"))
            .unwrap()
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
    }
    assert_eq!(count.load(Ordering::Relaxed), 3);
}

#[tokio::test]
async fn custom_connector_two_clients_isolated() {
    let (addr, _counter) = h1_server().await;
    let c1 = CountingConnector::new();
    let c2 = CountingConnector::new();
    let count1 = c1.count.clone();
    let count2 = c2.count.clone();
    let client1 = HttpEngineSend::<TokioRuntime, CountingConnector>::builder_with_connector(c1)
        .no_connection_reuse()
        .build()
        .unwrap();
    let client2 = HttpEngineSend::<TokioRuntime, CountingConnector>::builder_with_connector(c2)
        .no_connection_reuse()
        .build()
        .unwrap();
    client1
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();
    client1
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();
    client2
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(count1.load(Ordering::Relaxed), 2);
    assert_eq!(count2.load(Ordering::Relaxed), 1);
}

#[tokio::test]
async fn http_engine_default_works() {
    let (addr, _counter) = h1_server().await;
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::default();
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
}

#[tokio::test]
async fn forward_preserves_method_and_body() {
    let (upstream_addr, _counter) = h1_server_with(|req| async move {
        use http_body_util::BodyExt;
        let method = req.method().to_string();
        let body_bytes = req.into_body().collect().await.unwrap().to_bytes();
        let text = format!("{method}:{}", String::from_utf8_lossy(&body_bytes));
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(text))))
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();
    let req = http::Request::builder()
        .method(http::Method::POST)
        .uri(format!("http://{upstream_addr}/"))
        .body(Full::new(Bytes::from("payload")))
        .unwrap();

    let resp = client
        .forward(req)
        .upstream(
            format!("http://{upstream_addr}")
                .parse::<http::Uri>()
                .unwrap(),
        )
        .send()
        .await
        .unwrap();

    let text = resp.text().await.unwrap();
    assert_eq!(text, "POST:payload");
}

#[tokio::test]
async fn forward_strips_connection_header() {
    let (upstream_addr, _counter) = h1_server_with(|req| async move {
        let has_connection = req.headers().contains_key("connection");
        let has_custom = req.headers().contains_key("x-custom");
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(format!(
            "conn={has_connection},custom={has_custom}"
        )))))
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();
    let req = http::Request::builder()
        .method(http::Method::GET)
        .uri("/test")
        .header("Connection", "keep-alive")
        .header("X-Custom", "preserved")
        .body(Full::new(Bytes::new()))
        .unwrap();

    let resp = client
        .forward(req)
        .upstream(
            format!("http://{upstream_addr}")
                .parse::<http::Uri>()
                .unwrap(),
        )
        .send()
        .await
        .unwrap();

    let text = resp.text().await.unwrap();
    assert_eq!(text, "conn=false,custom=true");
}

#[tokio::test]
async fn forward_with_custom_connector() {
    let (upstream_addr, _counter) = h1_server().await;

    let connector = CountingConnector::new();
    let count = connector.count.clone();
    let client =
        HttpEngineSend::<TokioRuntime, CountingConnector>::builder_with_connector(connector)
            .no_connection_reuse()
            .build()
            .unwrap();

    let req = http::Request::builder()
        .method(http::Method::GET)
        .uri("/hello")
        .body(Full::new(Bytes::new()))
        .unwrap();

    let resp = client
        .forward(req)
        .upstream(
            format!("http://{upstream_addr}")
                .parse::<http::Uri>()
                .unwrap(),
        )
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    assert!(count.load(Ordering::Relaxed) > 0);
}
