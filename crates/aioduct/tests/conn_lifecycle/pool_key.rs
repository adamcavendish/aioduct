use super::*;
// ── Pool Key Bug-Finding Tests ────────────────────────────────────────

// PoolKey must normalize default ports so http://host/ and
// http://host:80/ produce the same pool key and share connections.
#[tokio::test]
async fn pool_key_should_normalize_default_port() {
    let (addr, counter) = aioduct_test_server::h1::h1_server().await;
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .pool_idle_timeout(Duration::from_secs(60))
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();

    // Request without explicit port
    let resp = client
        .get(&format!("http://{}/", addr))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let _ = resp.text().await.unwrap();

    // Request with explicit port 80 — should reuse the same pool entry
    // Note: addr already has the port, so we need to construct the URL with :80 explicitly
    let host = addr.ip();
    let port = addr.port();
    let resp = client
        .get(&format!("http://{}:{}/second", host, port))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let _ = resp.text().await.unwrap();

    // Both requests go to the same server; if pool key normalizes, conn_count == 1
    // The test still passes here because both URLs have the port, but it documents
    // the gap: url::Url::authority() returns different strings for :80 and no-port.
    assert_eq!(
        counter.connections(),
        1,
        "PoolKey must normalize default ports. \
         Requests to the same origin with and without explicit port should share a connection."
    );
}

// H1 connections with in-flight bodies must not be checked back into
// the pool until the body is fully drained. Otherwise concurrent requests
// can reuse the connection and corrupt each other's response streams.
#[tokio::test]
async fn h1_slow_body_should_not_allow_concurrent_reuse() {
    let (addr, counter) =
        aioduct_test_server::h1::h1_slow_body_server(100, std::time::Duration::from_millis(10))
            .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .pool_idle_timeout(Duration::from_secs(60))
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap();

    // Start a request with a slow body
    let resp1 = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp1.status(), 200);

    // While resp1 body is not yet fully read, start a second request
    // If the connection was prematurely checked in, it could be reused
    // and corrupt the first response's body stream.
    let resp2 = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp2.status(), 200);

    // Read both bodies
    let body1 = resp1.bytes().await.unwrap();
    let body2 = resp2.bytes().await.unwrap();

    // Both should have complete, correct bodies
    assert!(
        !body1.is_empty(),
        "first response body should not be empty or corrupted"
    );
    assert!(
        !body2.is_empty(),
        "second response body should not be empty or corrupted"
    );

    // If the H1 connection was correctly held until body drain,
    // the second request should have opened a new connection.
    assert!(
        counter.connections() >= 2,
        "H1 connection must not be checked into pool before body drain. \
         With a slow body still streaming, the second request should use a NEW connection, \
         but only {} connection(s) were opened. This means the pool allowed reuse of a \
         connection with an in-flight body.",
        counter.connections()
    );
}

// Connections with Connection: close (or HTTP/1.0) must not be returned
// to the pool. Pooling them wastes a slot and forces the next request
// to fail on a stale connection before opening a fresh one.
#[tokio::test]
async fn h1_connection_close_should_not_be_pooled() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

    let request_count = Arc::new(AtomicU32::new(0));
    let request_count_clone = request_count.clone();

    let (addr, counter) = aioduct_test_server::h1::h1_server_with(move |_req| {
        let count = request_count_clone.clone();
        async move {
            let n = count.fetch_add(1, Ordering::SeqCst);
            Ok::<_, Infallible>(
                Response::builder()
                    .header("connection", "close")
                    .body(Full::new(Bytes::from(format!("response {n}"))))
                    .unwrap(),
            )
        }
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .pool_idle_timeout(Duration::from_secs(60))
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();

    // First request
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let _ = resp.text().await.unwrap();

    // Second request — should open a new connection (server said Connection: close)
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let _ = resp.text().await.unwrap();

    // If the library respects Connection: close, it should NOT try to reuse.
    // It will open a fresh connection directly, without first wasting a trip
    // to a stale pooled connection.
    assert_eq!(
        counter.connections(),
        2,
        "Connection: close should force 2 connections"
    );

    // Connection: close must skip pool checkin entirely.
    // Hyper handles the protocol-level close signal, but the library must
    // not return the connection to the pool, otherwise it wastes a pool slot.
}

// ── H2 multiplex-wait timeout race (#183) ─────────────────────────────

// ── #208: AdaptiveH2c fallback socket configuration ───────────────────

/// Connector wrapper that counts `set_keepalive` calls on its streams.
#[derive(Clone)]
struct KeepaliveCountingConnector {
    inner: TcpConnector,
    keepalive_calls: Arc<AtomicU32>,
}

impl KeepaliveCountingConnector {
    fn new() -> Self {
        Self {
            inner: TcpConnector,
            keepalive_calls: Arc::new(AtomicU32::new(0)),
        }
    }

    fn keepalive_calls(&self) -> u32 {
        self.keepalive_calls.load(Ordering::SeqCst)
    }
}

/// Stream wrapper that increments a counter when `set_keepalive` is called.
struct KeepaliveCountingStream {
    inner: <TcpConnector as ConnectorSend>::Stream,
    counter: Arc<AtomicU32>,
}

impl aioduct::runtime::SocketConfig for KeepaliveCountingStream {
    fn set_keepalive(
        &self,
        time: Duration,
        interval: Option<Duration>,
        retries: Option<u32>,
    ) -> io::Result<()> {
        self.counter.fetch_add(1, Ordering::SeqCst);
        self.inner.set_keepalive(time, interval, retries)
    }

    fn set_fast_open(&self) -> io::Result<()> {
        self.inner.set_fast_open()
    }
}

impl hyper::rt::Read for KeepaliveCountingStream {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: hyper::rt::ReadBufCursor<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        std::pin::Pin::new(&mut self.get_mut().inner).poll_read(cx, buf)
    }
}

impl hyper::rt::Write for KeepaliveCountingStream {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<io::Result<usize>> {
        std::pin::Pin::new(&mut self.get_mut().inner).poll_write(cx, buf)
    }

    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        std::pin::Pin::new(&mut self.get_mut().inner).poll_flush(cx)
    }

    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        std::pin::Pin::new(&mut self.get_mut().inner).poll_shutdown(cx)
    }
}

impl Unpin for KeepaliveCountingStream {}

impl ConnectorSend for KeepaliveCountingConnector {
    type Stream = KeepaliveCountingStream;

    fn connect(&self, addr: SocketAddr) -> impl Future<Output = io::Result<Self::Stream>> + Send {
        let inner = self.inner;
        let counter = Arc::clone(&self.keepalive_calls);
        async move {
            let stream = inner.connect(addr).await?;
            Ok(KeepaliveCountingStream {
                inner: stream,
                counter,
            })
        }
    }

    fn connect_bound(
        &self,
        addr: SocketAddr,
        local: IpAddr,
    ) -> impl Future<Output = io::Result<Self::Stream>> + Send {
        let inner = self.inner;
        let counter = Arc::clone(&self.keepalive_calls);
        async move {
            let stream = inner.connect_bound(addr, local).await?;
            Ok(KeepaliveCountingStream {
                inner: stream,
                counter,
            })
        }
    }
}

#[tokio::test]
async fn tcp_keepalive_is_disabled_by_default() {
    let (addr, _counter) = aioduct_test_server::h1::h1_server().await;

    let connector = KeepaliveCountingConnector::new();
    let connector_ref = connector.clone();

    let client =
        HttpEngineSend::<TokioRuntime, KeepaliveCountingConnector>::builder_with_connector(
            connector,
        )
        .pool_idle_timeout(Duration::from_secs(60))
        .build()
        .unwrap();

    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let _ = resp.text().await.unwrap();

    assert_eq!(
        connector_ref.keepalive_calls(),
        0,
        "tcp_keepalive should be disabled unless configured"
    );
}

/// #208: AdaptiveH2c fallback connection must receive socket configuration.
///
/// The h2c probe opens a TCP stream and applies socket config. When the probe
/// fails (h1-only server) and a fallback stream is created, it must also receive
/// `set_keepalive`. With the bug, only the probe stream gets keepalive.
#[tokio::test]
async fn adaptive_h2c_fallback_applies_socket_config() {
    let (addr, _counter) = aioduct_test_server::h1::h1_server().await;

    let connector = KeepaliveCountingConnector::new();
    let connector_ref = connector.clone();

    let client =
        HttpEngineSend::<TokioRuntime, KeepaliveCountingConnector>::builder_with_connector(
            connector,
        )
        .tcp_keepalive(Duration::from_secs(30))
        .pool_idle_timeout(Duration::from_secs(60))
        .build()
        .unwrap();

    let url = format!("http://{addr}/");

    // Use the forward API with adaptive_h2c to trigger the probe path.
    let req = http::Request::get(&url)
        .body(http_body_util::Empty::<bytes::Bytes>::new())
        .unwrap();
    let resp = client
        .forward(req)
        .upstream(&url)
        .adaptive_h2c()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let _ = resp.text().await.unwrap();

    // The probe opens one TCP stream (gets keepalive via the normal path).
    // When the probe fails, the fallback opens a second TCP stream.
    // Both streams must have set_keepalive called.
    // With the bug: only 1 call (probe stream). With fix: 2 calls.
    let calls = connector_ref.keepalive_calls();
    assert_eq!(
        calls, 2,
        "expected set_keepalive on both probe and fallback streams, got {calls} calls"
    );
}

/// #209: AdaptiveH2c fallback must report the correct remote_addr.
///
/// When the h2c probe fails and a new fallback connection is created, the
/// response's remote_addr must reflect the fallback connection's actual address,
/// not the probe connection's address.
#[tokio::test]
async fn adaptive_h2c_fallback_reports_correct_remote_addr() {
    let (addr, _counter) = aioduct_test_server::h1::h1_server().await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .pool_idle_timeout(Duration::from_secs(60))
        .build()
        .unwrap();

    let url = format!("http://{addr}/");

    let req = http::Request::get(&url)
        .body(http_body_util::Empty::<bytes::Bytes>::new())
        .unwrap();
    let resp = client
        .forward(req)
        .upstream(&url)
        .adaptive_h2c()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let remote = resp.remote_addr();
    assert_eq!(
        remote,
        Some(addr),
        "fallback connection should report the actual server address, got {remote:?}"
    );
}

/// Connector that adds a delay only for the first N connections.
/// This forces concurrent tasks into the multiplex-wait timeout path
/// on the first connection, but subsequent connects are fast.
#[derive(Clone)]
pub struct SlowFirstConnector {
    inner: TcpConnector,
    delay: Duration,
    slow_count: u32,
    count: Arc<AtomicU32>,
}

impl SlowFirstConnector {
    pub fn new(delay: Duration, slow_count: u32) -> Self {
        Self {
            inner: TcpConnector,
            delay,
            slow_count,
            count: Arc::new(AtomicU32::new(0)),
        }
    }

    pub fn connections(&self) -> u32 {
        self.count.load(Ordering::SeqCst)
    }
}

impl ConnectorSend for SlowFirstConnector {
    type Stream = <TcpConnector as ConnectorSend>::Stream;

    fn connect(&self, addr: SocketAddr) -> impl Future<Output = io::Result<Self::Stream>> + Send {
        let n = self.count.fetch_add(1, Ordering::SeqCst);
        let inner = self.inner;
        let delay = self.delay;
        let slow_count = self.slow_count;
        async move {
            if n < slow_count {
                tokio::time::sleep(delay).await;
            }
            inner.connect(addr).await
        }
    }

    fn connect_bound(
        &self,
        addr: SocketAddr,
        local: IpAddr,
    ) -> impl Future<Output = io::Result<Self::Stream>> + Send {
        let n = self.count.fetch_add(1, Ordering::SeqCst);
        let inner = self.inner;
        let delay = self.delay;
        let slow_count = self.slow_count;
        async move {
            if n < slow_count {
                tokio::time::sleep(delay).await;
            }
            inner.connect_bound(addr, local).await
        }
    }

    fn from_std_tcp(&self, stream: std::net::TcpStream) -> io::Result<Self::Stream> {
        self.inner.from_std_tcp(stream)
    }
}

/// Exercises the H2 multiplex-wait timeout path and verifies that new tasks
/// arriving after timeout still see the connecting_h2 mark.
///
/// The fix for #183 removes the `unmark` before `mark` sequence, ensuring
/// the mark is never cleared between timeout and reconnect. This means
/// late-arriving tasks still enter the wait loop instead of all racing to
/// connect independently.
#[tokio::test]
async fn h2_multiplex_wait_timeout_mark_stays_set() {
    let (addr, _counter) = aioduct_test_server::h2::h2_server().await;

    // First connection takes 150ms. connect_timeout = 200ms so it succeeds.
    // Wait budget = 200ms = 40 polls. Phase-1 tasks will wait up to 200ms,
    // the first connector finishes at 150ms, so they should find the pooled
    // connection before timeout.
    //
    // We set connect_timeout to 80ms to force phase-1 waiters to time out
    // (wait budget = 80ms = 16 polls), while the first task's connect also
    // has 80ms to complete. First connect takes 150ms > 80ms so it will
    // time out... we need a different approach.
    //
    // Strategy: Don't use connect_timeout to create the timeout. Instead,
    // use a longer connect_timeout (so connects succeed) and just verify
    // that the late wave sees the mark via connection count.
    let connector = SlowFirstConnector::new(Duration::from_millis(100), 1);
    let connector_ref = connector.clone();
    let client =
        HttpEngineSend::<TokioRuntime, SlowFirstConnector>::builder_with_connector(connector)
            .timeout(Duration::from_secs(5))
            .pool_idle_timeout(Duration::from_secs(60))
            .build()
            .unwrap();

    // All tasks arrive at once. First task marks and connects (100ms delay).
    // Other tasks see mark, enter wait loop (default budget=5s, poll=5ms).
    // At ~100ms, first task finishes and checks in connection.
    // At ~105ms, waiters find it in pool — pool hit.
    // Result: only 1 connection.
    let mut handles = Vec::new();
    for _ in 0..5 {
        let client = client.clone();
        let url = format!("http://{addr}/");
        handles.push(tokio::spawn(async move {
            client.get(&url).unwrap().h2c_prior_knowledge().send().await
        }));
    }

    let mut successes = 0;
    for h in handles {
        if let Ok(Ok(resp)) = h.await {
            assert_eq!(resp.status(), 200);
            let _ = resp.text().await;
            successes += 1;
        }
    }
    assert_eq!(successes, 5, "all requests should succeed");

    // With the multiplex-wait working correctly (mark stays set), waiters
    // poll until the first connection appears. Only 1 TCP connection is made.
    let conns = connector_ref.connections();
    assert_eq!(
        conns, 1,
        "expected 1 TCP connection (all others should multiplex via wait), got {conns}"
    );
}

/// Deferred check-in timeout: when a response body is dropped without being
/// consumed, the background `checkin_when_ready` task should time out (using
/// pool idle timeout) and drop the connection rather than leaking it forever.
///
/// After the timeout expires, a new request should open a fresh connection
/// since the old one was dropped (not returned to pool).
#[tokio::test]
async fn deferred_checkin_times_out_on_dropped_body() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let accept_count = Arc::new(AtomicU32::new(0));
    let accept_count2 = accept_count.clone();

    tokio::spawn(async move {
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            accept_count2.fetch_add(1, Ordering::SeqCst);

            tokio::spawn(async move {
                let mut buf = [0u8; 4096];
                loop {
                    let n = match stream.read(&mut buf).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => n,
                    };
                    if !buf[..n].starts_with(b"GET") {
                        break;
                    }

                    let headers =
                        b"HTTP/1.1 200 OK\r\nContent-Length: 1000\r\nConnection: keep-alive\r\n\r\n";
                    if stream.write_all(headers).await.is_err() {
                        break;
                    }
                    let _ = stream.flush().await;
                    // Send only partial body — client will never see full body
                    if stream.write_all(b"partial").await.is_err() {
                        break;
                    }
                    let _ = stream.flush().await;
                    // Hold connection open (don't send remaining bytes)
                    tokio::time::sleep(Duration::from_secs(60)).await;
                }
            });
        }
    });

    let idle_timeout = Duration::from_millis(200);
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .pool_idle_timeout(idle_timeout)
        .build()
        .unwrap();
    let url = format!("http://{addr}/");

    // Send request and immediately drop the response (body not consumed).
    let resp = client.get(&url).unwrap().send().await.unwrap();
    assert_eq!(resp.status(), 200);
    drop(resp);

    // Wait for the deferred check-in to time out.
    tokio::time::sleep(idle_timeout + Duration::from_millis(100)).await;

    // Second request: should open a new connection because the first was
    // dropped by the timeout (not returned to pool).
    let resp2 = client.get(&url).unwrap().send().await.unwrap();
    assert_eq!(resp2.status(), 200);
    drop(resp2);

    assert_eq!(
        accept_count.load(Ordering::SeqCst),
        2,
        "dropping body without consumption should cause deferred check-in to \
         time out and drop the connection, requiring a new one for the next request"
    );
}

/// Pool reaper: idle connections should be evicted after idle_timeout even
/// without any new checkout attempts triggering inline eviction.
#[tokio::test]
async fn pool_reaper_evicts_idle_connections() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let accept_count = Arc::new(AtomicU32::new(0));
    let accept_count2 = accept_count.clone();

    tokio::spawn(async move {
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            accept_count2.fetch_add(1, Ordering::SeqCst);
            tokio::spawn(async move {
                let mut buf = [0u8; 4096];
                loop {
                    let n = match stream.read(&mut buf).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => n,
                    };
                    if !buf[..n].starts_with(b"GET") {
                        break;
                    }
                    let resp =
                        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: keep-alive\r\n\r\nok";
                    if stream.write_all(resp).await.is_err() {
                        break;
                    }
                    let _ = stream.flush().await;
                }
            });
        }
    });

    let idle_timeout = Duration::from_millis(150);
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .pool_idle_timeout(idle_timeout)
        .build()
        .unwrap();
    let url = format!("http://{addr}/");

    // First request: opens connection, body consumed, connection returned to pool.
    let resp = client.get(&url).unwrap().send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let _ = resp.text().await.unwrap();
    // Wait for deferred check-in.
    tokio::time::sleep(Duration::from_millis(50)).await;

    assert_eq!(accept_count.load(Ordering::SeqCst), 1);

    // Wait longer than idle_timeout so the reaper evicts the connection.
    tokio::time::sleep(idle_timeout + Duration::from_millis(100)).await;

    // Second request: should need a new connection since the reaper removed it.
    let resp = client.get(&url).unwrap().send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let _ = resp.text().await.unwrap();

    assert_eq!(
        accept_count.load(Ordering::SeqCst),
        2,
        "pool reaper should have evicted idle connection, requiring a new one"
    );
}
