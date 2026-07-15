use super::*;

use std::sync::Mutex;

use futures_channel::oneshot;
use smol::io::{AsyncReadExt as _, AsyncWriteExt as _};

const TEST_TIMEOUT: Duration = Duration::from_secs(5);
const UPSTREAM_GREETING: &[u8] = b"smol-upstream-greeting";
const DOWNSTREAM_PAYLOAD: &[u8] = b"smol-downstream-payload";
const UPSTREAM_ACK: &[u8] = b"smol-upstream-ack";

type TunnelResult = Result<Vec<u8>, String>;
type TunnelReport = oneshot::Receiver<TunnelResult>;
type TunnelSender = Arc<Mutex<Option<oneshot::Sender<TunnelResult>>>>;

async fn hyper_read<T>(io: &mut T, bytes: &mut [u8]) -> Result<usize, String>
where
    T: hyper::rt::Read + Unpin,
{
    std::future::poll_fn(|cx| {
        let mut buffer = hyper::rt::ReadBuf::new(bytes);
        match hyper::rt::Read::poll_read(Pin::new(&mut *io), cx, buffer.unfilled()) {
            Poll::Ready(Ok(())) => Poll::Ready(Ok(buffer.filled().len())),
            Poll::Ready(Err(error)) => Poll::Ready(Err(error)),
            Poll::Pending => Poll::Pending,
        }
    })
    .await
    .map_err(|error| format!("tunnel read failed: {error}"))
}

async fn hyper_read_exact<T>(io: &mut T, bytes: &mut [u8]) -> Result<(), String>
where
    T: hyper::rt::Read + Unpin,
{
    let mut filled = 0;
    while filled < bytes.len() {
        let read = hyper_read(io, &mut bytes[filled..]).await?;
        if read == 0 {
            return Err("tunnel closed before the expected bytes arrived".to_owned());
        }
        filled += read;
    }
    Ok(())
}

async fn hyper_write_all<T>(io: &mut T, bytes: &[u8]) -> Result<(), String>
where
    T: hyper::rt::Write + Unpin,
{
    let mut written = 0;
    while written < bytes.len() {
        let count = std::future::poll_fn(|cx| {
            hyper::rt::Write::poll_write(Pin::new(&mut *io), cx, &bytes[written..])
        })
        .await
        .map_err(|error| format!("tunnel write failed: {error}"))?;
        if count == 0 {
            return Err("tunnel write returned zero".to_owned());
        }
        written += count;
    }
    std::future::poll_fn(|cx| hyper::rt::Write::poll_flush(Pin::new(&mut *io), cx))
        .await
        .map_err(|error| format!("tunnel flush failed: {error}"))
}

async fn hyper_shutdown<T>(io: &mut T) -> Result<(), String>
where
    T: hyper::rt::Write + Unpin,
{
    std::future::poll_fn(|cx| hyper::rt::Write::poll_shutdown(Pin::new(&mut *io), cx))
        .await
        .map_err(|error| format!("tunnel shutdown failed: {error}"))
}

fn send_report(sender: &TunnelSender, result: TunnelResult) {
    if let Some(sender) = sender.lock().unwrap().take() {
        let _ = sender.send(result);
    }
}

async fn start_upgrade_upstream() -> (SocketAddr, TunnelReport) {
    let listener = smol::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (report_tx, report_rx) = oneshot::channel();
    let report_tx = Arc::new(Mutex::new(Some(report_tx)));

    smol::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let report_tx_for_service = report_tx.clone();
        let service = service_fn(move |mut request: Request<hyper::body::Incoming>| {
            let report_tx = report_tx_for_service.clone();
            async move {
                assert_eq!(request.method(), http::Method::GET);
                assert_eq!(request.uri().path(), "/chat");
                assert_eq!(request.version(), http::Version::HTTP_11);
                assert_eq!(
                    request.headers().get(http::header::UPGRADE),
                    Some(&http::HeaderValue::from_static("aioduct-smol-test"))
                );

                let upgrade = hyper::upgrade::on(&mut request);
                smol::spawn(async move {
                    let result = async {
                        let upgraded = upgrade
                            .await
                            .map_err(|error| format!("upstream upgrade failed: {error}"))?;
                        let mut tunnel = aioduct::UpgradedSend::from(upgraded);
                        hyper_write_all(&mut tunnel, UPSTREAM_GREETING).await?;
                        let mut payload = vec![0; DOWNSTREAM_PAYLOAD.len()];
                        hyper_read_exact(&mut tunnel, &mut payload).await?;
                        hyper_write_all(&mut tunnel, UPSTREAM_ACK).await?;
                        hyper_shutdown(&mut tunnel).await?;
                        Ok(payload)
                    }
                    .await;
                    send_report(&report_tx, result);
                })
                .detach();

                Ok::<_, Infallible>(
                    Response::builder()
                        .status(http::StatusCode::SWITCHING_PROTOCOLS)
                        .header(http::header::CONNECTION, "upgrade")
                        .header(http::header::UPGRADE, "aioduct-smol-test")
                        .body(Full::new(Bytes::new()))
                        .unwrap(),
                )
            }
        });
        let result = server_http1::Builder::new()
            .serve_connection(SmolIo::new(stream), service)
            .with_upgrades()
            .await;
        if let Err(error) = result {
            send_report(&report_tx, Err(format!("upstream server failed: {error}")));
        }
    })
    .detach();

    (address, report_rx)
}

async fn start_upgrade_broker(
    client: HttpEngineSend<SmolRuntime, SmolTcpConnector>,
    upstream: http::Uri,
) -> (SocketAddr, TunnelReport) {
    let listener = smol::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (report_tx, report_rx) = oneshot::channel();
    let report_tx = Arc::new(Mutex::new(Some(report_tx)));

    smol::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let report_tx_for_service = report_tx.clone();
        let service = service_fn(move |mut request: Request<hyper::body::Incoming>| {
            let client = client.clone();
            let upstream = upstream.clone();
            let report_tx = report_tx_for_service.clone();
            async move {
                let downstream_upgrade = hyper::upgrade::on(&mut request);
                let upstream_response =
                    match client.forward(request).upstream(upstream).send().await {
                        Ok(response) => response,
                        Err(error) => {
                            return Ok::<_, Infallible>(
                                Response::builder()
                                    .status(http::StatusCode::BAD_GATEWAY)
                                    .body(Full::new(Bytes::from(error.to_string())))
                                    .unwrap(),
                            );
                        }
                    };

                let status = upstream_response.status();
                let version = upstream_response.version();
                let headers = upstream_response.headers().clone();
                smol::spawn(async move {
                    let result = async {
                        let downstream = downstream_upgrade
                            .await
                            .map_err(|error| format!("downstream upgrade failed: {error}"))?;
                        let mut downstream = aioduct::UpgradedSend::from(downstream);
                        let mut upstream = upstream_response
                            .upgrade()
                            .await
                            .map_err(|error| format!("forwarded upgrade failed: {error}"))?;

                        let mut greeting = vec![0; UPSTREAM_GREETING.len()];
                        hyper_read_exact(&mut upstream, &mut greeting).await?;
                        hyper_write_all(&mut downstream, &greeting).await?;
                        let mut payload = vec![0; DOWNSTREAM_PAYLOAD.len()];
                        hyper_read_exact(&mut downstream, &mut payload).await?;
                        hyper_write_all(&mut upstream, &payload).await?;
                        let mut acknowledgement = vec![0; UPSTREAM_ACK.len()];
                        hyper_read_exact(&mut upstream, &mut acknowledgement).await?;
                        hyper_write_all(&mut downstream, &acknowledgement).await?;
                        hyper_shutdown(&mut upstream).await?;
                        hyper_shutdown(&mut downstream).await?;
                        Ok(payload)
                    }
                    .await;
                    send_report(&report_tx, result);
                })
                .detach();

                let mut response = Response::new(Full::new(Bytes::new()));
                *response.status_mut() = status;
                *response.version_mut() = version;
                *response.headers_mut() = headers;
                Ok(response)
            }
        });
        let result = server_http1::Builder::new()
            .serve_connection(SmolIo::new(stream), service)
            .with_upgrades()
            .await;
        if let Err(error) = result {
            send_report(&report_tx, Err(format!("broker server failed: {error}")));
        }
    })
    .detach();

    (address, report_rx)
}

async fn h2_connect_echo_response(
    mut request: Request<hyper::body::Incoming>,
    expected_protocol: Option<&'static str>,
    upstream: SocketAddr,
    closed_tunnels: Arc<AtomicUsize>,
) -> Result<Response<Full<Bytes>>, Infallible> {
    if request.method() != http::Method::CONNECT {
        return Ok(Response::new(Full::new(Bytes::from_static(b"ordinary"))));
    }

    assert_eq!(
        request
            .extensions()
            .get::<hyper::ext::Protocol>()
            .map(hyper::ext::Protocol::as_str),
        expected_protocol
    );
    if expected_protocol.is_some() {
        let expected_authority = upstream.to_string();
        assert_eq!(request.uri().scheme_str(), Some("http"));
        assert_eq!(
            request.uri().authority().map(http::uri::Authority::as_str),
            Some(expected_authority.as_str())
        );
        assert_eq!(request.uri().path(), "/tunnel");
    } else {
        assert_eq!(
            request.uri().authority().map(http::uri::Authority::as_str),
            Some("target.example:443")
        );
    }
    smol::spawn(async move {
        let Ok(upgraded) = hyper::upgrade::on(&mut request).await else {
            return;
        };
        let mut tunnel = aioduct::UpgradedSend::from(upgraded);
        let mut buffer = [0_u8; 64];
        loop {
            let read = match hyper_read(&mut tunnel, &mut buffer).await {
                Ok(0) | Err(_) => break,
                Ok(read) => read,
            };
            if hyper_write_all(&mut tunnel, &buffer[..read]).await.is_err() {
                break;
            }
        }
        closed_tunnels.fetch_add(1, Ordering::SeqCst);
    })
    .detach();

    Ok(Response::new(Full::new(Bytes::new())))
}

struct H2ConnectServer {
    address: SocketAddr,
    connections: Arc<AtomicUsize>,
    closed_tunnels: Arc<AtomicUsize>,
}

async fn start_h2_connect_echo_server(expected_protocol: Option<&'static str>) -> H2ConnectServer {
    use hyper::server::conn::http2 as server_http2;

    let listener = smol::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let connections = Arc::new(AtomicUsize::new(0));
    let server_connections = connections.clone();
    let closed_tunnels = Arc::new(AtomicUsize::new(0));
    let server_closed_tunnels = closed_tunnels.clone();

    smol::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            server_connections.fetch_add(1, Ordering::SeqCst);
            let closed_tunnels = server_closed_tunnels.clone();
            smol::spawn(async move {
                let _ = server_http2::Builder::new(SmolExec)
                    .enable_connect_protocol()
                    .serve_connection(
                        SmolIo::new(stream),
                        service_fn(move |request| {
                            h2_connect_echo_response(
                                request,
                                expected_protocol,
                                address,
                                closed_tunnels.clone(),
                            )
                        }),
                    )
                    .await;
            })
            .detach();
        }
    })
    .detach();

    H2ConnectServer {
        address,
        connections,
        closed_tunnels,
    }
}

fn h2_connect_request(expected_protocol: Option<&'static str>) -> Request<Full<Bytes>> {
    let uri = if expected_protocol.is_some() {
        "http://downstream.test/tunnel"
    } else {
        "target.example:443"
    };
    let mut request = Request::builder()
        .method(http::Method::CONNECT)
        .uri(uri)
        .version(http::Version::HTTP_2)
        .body(Full::new(Bytes::new()))
        .unwrap();
    if let Some(protocol) = expected_protocol {
        request
            .extensions_mut()
            .insert(aioduct::Protocol::from_static(protocol));
    }
    request
}

async fn start_h2_connect_broker(
    client: HttpEngineSend<SmolRuntime, SmolTcpConnector>,
    upstream: http::Uri,
) -> (SocketAddr, TunnelReport) {
    use hyper::server::conn::http2 as server_http2;

    let listener = smol::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (report_tx, report_rx) = oneshot::channel();
    let report_tx = Arc::new(Mutex::new(Some(report_tx)));

    smol::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let report_tx_for_service = report_tx.clone();
        let service = service_fn(move |mut request: Request<hyper::body::Incoming>| {
            let client = client.clone();
            let upstream = upstream.clone();
            let report_tx = report_tx_for_service.clone();
            async move {
                let downstream_upgrade = hyper::upgrade::on(&mut request);
                let upstream_response = match client
                    .forward(request)
                    .upstream(upstream)
                    .h2c()
                    .send()
                    .await
                {
                    Ok(response) => response,
                    Err(error) => {
                        return Ok::<_, Infallible>(
                            Response::builder()
                                .status(http::StatusCode::BAD_GATEWAY)
                                .body(Full::new(Bytes::from(error.to_string())))
                                .unwrap(),
                        );
                    }
                };
                let status = upstream_response.status();
                let headers = upstream_response.headers().clone();
                smol::spawn(async move {
                    let result = async {
                        let downstream = downstream_upgrade
                            .await
                            .map_err(|error| format!("downstream H2 upgrade failed: {error}"))?;
                        let mut downstream = aioduct::UpgradedSend::from(downstream);
                        let mut upstream = upstream_response
                            .upgrade()
                            .await
                            .map_err(|error| format!("upstream H2 upgrade failed: {error}"))?;
                        let mut payload = vec![0_u8; DOWNSTREAM_PAYLOAD.len()];
                        hyper_read_exact(&mut downstream, &mut payload).await?;
                        hyper_write_all(&mut upstream, &payload).await?;
                        let mut echoed = vec![0_u8; payload.len()];
                        hyper_read_exact(&mut upstream, &mut echoed).await?;
                        hyper_write_all(&mut downstream, &echoed).await?;
                        // Observe the downstream END_STREAM before closing the
                        // response half, so both H2 tunnel directions shut down
                        // without racing a peer reset.
                        let mut extra = [0_u8; 1];
                        if hyper_read(&mut downstream, &mut extra).await? != 0 {
                            return Err(
                                "downstream sent bytes after the expected payload".to_owned()
                            );
                        }
                        hyper_shutdown(&mut upstream).await?;
                        hyper_shutdown(&mut downstream).await?;
                        Ok(payload)
                    }
                    .await;
                    send_report(&report_tx, result);
                })
                .detach();

                let mut response = Response::new(Full::new(Bytes::new()));
                *response.status_mut() = status;
                *response.version_mut() = http::Version::HTTP_2;
                *response.headers_mut() = headers;
                Ok(response)
            }
        });
        let result = server_http2::Builder::new(SmolExec)
            .enable_connect_protocol()
            .serve_connection(SmolIo::new(stream), service)
            .await;
        if let Err(error) = result {
            send_report(&report_tx, Err(format!("H2 broker server failed: {error}")));
        }
    })
    .detach();

    (address, report_rx)
}

async fn assert_real_incoming_h2_connect_round_trips(expected_protocol: Option<&'static str>) {
    use hyper::client::conn::http2 as client_http2;

    let server = start_h2_connect_echo_server(expected_protocol).await;
    let client = HttpEngineSend::<SmolRuntime, SmolTcpConnector>::builder()
        .timeout(TEST_TIMEOUT)
        .build()
        .unwrap();
    let (broker, report) = start_h2_connect_broker(
        client,
        format!("http://{}", server.address).parse().unwrap(),
    )
    .await;

    let stream = smol::net::TcpStream::connect(broker).await.unwrap();
    let (mut sender, connection) = client_http2::Builder::new(SmolExec)
        .handshake(SmolIo::new(stream))
        .await
        .unwrap();
    smol::spawn(async move {
        let _ = connection.await;
    })
    .detach();
    sender.ready().await.unwrap();

    let mut response = sender
        .send_request(h2_connect_request(expected_protocol))
        .await
        .unwrap();
    assert_eq!(response.status(), http::StatusCode::OK);
    let upgraded = await_with_timeout(
        hyper::upgrade::on(&mut response),
        "real-Incoming Smol H2 CONNECT upgrade",
    )
    .await
    .unwrap();
    let mut tunnel = aioduct::UpgradedSend::from(upgraded);
    hyper_write_all(&mut tunnel, DOWNSTREAM_PAYLOAD)
        .await
        .unwrap();
    let mut echoed = [0_u8; DOWNSTREAM_PAYLOAD.len()];
    hyper_read_exact(&mut tunnel, &mut echoed).await.unwrap();
    assert_eq!(&echoed, DOWNSTREAM_PAYLOAD);
    hyper_shutdown(&mut tunnel).await.unwrap();

    assert_eq!(
        await_report(report, "real-Incoming Smol H2 CONNECT bridge").await,
        DOWNSTREAM_PAYLOAD
    );
    assert_eq!(server.connections.load(Ordering::SeqCst), 1);
}

async fn await_with_timeout<F>(future: F, description: &'static str) -> F::Output
where
    F: Future,
{
    smol::future::race(future, async move {
        async_io::Timer::after(TEST_TIMEOUT).await;
        panic!("timed out waiting for {description}");
    })
    .await
}

async fn assert_h2_connect_round_trips_and_reuses_transport(
    expected_protocol: Option<&'static str>,
) {
    let server = start_h2_connect_echo_server(expected_protocol).await;
    let upstream = server.address;
    let client = HttpEngineSend::<SmolRuntime, SmolTcpConnector>::builder()
        .timeout(TEST_TIMEOUT)
        .pool_max_active_streams_per_connection(2)
        .build()
        .unwrap();
    let mut forward = client
        .forward(h2_connect_request(expected_protocol))
        .upstream(format!("http://{upstream}").parse::<http::Uri>().unwrap())
        .h2c();
    if expected_protocol.is_some() {
        forward = forward.on_response(|response| response.extensions_mut().clear());
    }
    let response = forward.send().await.unwrap();
    assert_eq!(response.status(), http::StatusCode::OK);

    let mut tunnel = await_with_timeout(response.upgrade(), "Smol H2 CONNECT upgrade")
        .await
        .unwrap();
    let echoed = await_with_timeout(
        async {
            hyper_write_all(&mut tunnel, DOWNSTREAM_PAYLOAD).await?;
            let mut echoed = [0_u8; DOWNSTREAM_PAYLOAD.len()];
            hyper_read_exact(&mut tunnel, &mut echoed).await?;
            Ok::<_, String>(echoed)
        },
        "Smol H2 CONNECT tunnel echo",
    )
    .await
    .unwrap();
    assert_eq!(&echoed, DOWNSTREAM_PAYLOAD);

    let ordinary = client
        .get(&format!("http://{upstream}/ordinary"))
        .unwrap()
        .h2c_prior_knowledge()
        .send()
        .await
        .unwrap();
    assert_eq!(ordinary.text().await.unwrap(), "ordinary");
    assert_eq!(
        server.connections.load(Ordering::SeqCst),
        1,
        "an open Smol H2 CONNECT tunnel must leave spare stream capacity reusable"
    );
}

async fn assert_raw_h2_connect_response_retires_transport(expected_protocol: Option<&'static str>) {
    let server = start_h2_connect_echo_server(expected_protocol).await;
    let upstream = server.address;
    let client = HttpEngineSend::<SmolRuntime, SmolTcpConnector>::builder()
        .timeout(TEST_TIMEOUT)
        .pool_max_active_streams_per_connection(1)
        .build()
        .unwrap();
    let response = client
        .forward(h2_connect_request(expected_protocol))
        .upstream(format!("http://{upstream}").parse::<http::Uri>().unwrap())
        .h2c()
        .send()
        .await
        .unwrap();

    let mut raw = response.into_http_response();
    let upgraded = await_with_timeout(hyper::upgrade::on(&mut raw), "raw Smol H2 CONNECT upgrade")
        .await
        .unwrap();
    drop(raw);
    let mut tunnel = aioduct::UpgradedSend::from(upgraded);
    hyper_write_all(&mut tunnel, DOWNSTREAM_PAYLOAD)
        .await
        .unwrap();
    let mut echoed = [0_u8; DOWNSTREAM_PAYLOAD.len()];
    hyper_read_exact(&mut tunnel, &mut echoed).await.unwrap();
    assert_eq!(&echoed, DOWNSTREAM_PAYLOAD);
    hyper_shutdown(&mut tunnel).await.unwrap();
    drop(tunnel);
    await_with_timeout(
        async {
            while server.closed_tunnels.load(Ordering::SeqCst) == 0 {
                async_io::Timer::after(Duration::from_millis(1)).await;
            }
        },
        "raw Smol H2 CONNECT stream release",
    )
    .await;

    let ordinary = client
        .get(&format!("http://{upstream}/ordinary"))
        .unwrap()
        .h2c_prior_knowledge()
        .send()
        .await
        .unwrap();
    assert_eq!(ordinary.text().await.unwrap(), "ordinary");
    assert_eq!(
        server.connections.load(Ordering::SeqCst),
        2,
        "raw Smol H2 upgrade extraction must retire the transport after stream capacity is released"
    );
}

async fn await_report(report: TunnelReport, description: &str) -> Vec<u8> {
    smol::future::race(
        async move {
            report
                .await
                .unwrap_or_else(|_| panic!("{description} channel closed"))
                .unwrap_or_else(|error| panic!("{description} failed: {error}"))
        },
        async move {
            async_io::Timer::after(TEST_TIMEOUT).await;
            panic!("timed out waiting for {description}");
        },
    )
    .await
}

async fn open_raw_upgrade(address: SocketAddr) -> smol::net::TcpStream {
    let mut stream = smol::net::TcpStream::connect(address).await.unwrap();
    let request = format!(
        "GET /chat HTTP/1.1\r\nHost: {address}\r\nConnection: upgrade\r\nUpgrade: aioduct-smol-test\r\n\r\n"
    );
    stream.write_all(request.as_bytes()).await.unwrap();
    stream.flush().await.unwrap();

    let mut headers = Vec::new();
    while !headers.ends_with(b"\r\n\r\n") {
        assert!(headers.len() < 16 * 1024, "response headers are too large");
        let mut byte = [0];
        stream.read_exact(&mut byte).await.unwrap();
        headers.push(byte[0]);
    }
    let headers = String::from_utf8(headers).unwrap();
    assert!(
        headers.starts_with("HTTP/1.1 101 "),
        "unexpected upgrade response: {headers}"
    );
    stream
}

#[test]
fn smol_forward_real_incoming_h1_upgrade_round_trips_tunnel_bytes() {
    smol::block_on(async {
        let (upstream, upstream_report) = start_upgrade_upstream().await;
        let client = HttpEngineSend::<SmolRuntime, SmolTcpConnector>::builder()
            .timeout(TEST_TIMEOUT)
            .build()
            .unwrap();
        let (broker, broker_report) =
            start_upgrade_broker(client, format!("http://{upstream}").parse().unwrap()).await;

        let mut tunnel = open_raw_upgrade(broker).await;
        let mut greeting = vec![0; UPSTREAM_GREETING.len()];
        tunnel.read_exact(&mut greeting).await.unwrap();
        assert_eq!(greeting, UPSTREAM_GREETING);
        tunnel.write_all(DOWNSTREAM_PAYLOAD).await.unwrap();
        tunnel.flush().await.unwrap();
        let mut acknowledgement = vec![0; UPSTREAM_ACK.len()];
        tunnel.read_exact(&mut acknowledgement).await.unwrap();
        assert_eq!(acknowledgement, UPSTREAM_ACK);
        tunnel.close().await.unwrap();

        assert_eq!(
            await_report(upstream_report, "upstream tunnel").await,
            DOWNSTREAM_PAYLOAD
        );
        assert_eq!(
            await_report(broker_report, "forwarded tunnel").await,
            DOWNSTREAM_PAYLOAD
        );
    });
}

#[test]
fn smol_forward_h2_ordinary_connect_round_trips_and_reuses_transport() {
    smol::block_on(assert_h2_connect_round_trips_and_reuses_transport(None));
}

#[test]
fn smol_forward_h2_extended_connect_round_trips_and_reuses_transport() {
    smol::block_on(assert_h2_connect_round_trips_and_reuses_transport(Some(
        "websocket",
    )));
}

#[test]
fn smol_forward_real_incoming_h2_ordinary_connect_round_trips_tunnel_bytes() {
    smol::block_on(assert_real_incoming_h2_connect_round_trips(None));
}

#[test]
fn smol_forward_real_incoming_h2_extended_connect_round_trips_tunnel_bytes() {
    smol::block_on(assert_real_incoming_h2_connect_round_trips(Some(
        "websocket",
    )));
}

#[test]
fn smol_raw_h2_ordinary_connect_response_retires_transport() {
    smol::block_on(assert_raw_h2_connect_response_retires_transport(None));
}

#[test]
fn smol_raw_h2_extended_connect_response_retires_transport() {
    smol::block_on(assert_raw_h2_connect_response_retires_transport(Some(
        "websocket",
    )));
}
