use std::convert::Infallible;
use std::net::SocketAddr;
use std::time::Duration;

use aioduct::HttpEngineSend;
use aioduct::runtime::TokioRuntime;
use aioduct::runtime::tokio_rt::TcpConnector;
use aioduct_test_server::TokioExec;
use bytes::Bytes;
use http_body_util::Full;
use hyper::server::conn::{http1 as server_http1, http2 as server_http2};
use hyper::service::service_fn;
use hyper::{Request, Response};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;

pub(super) const TEST_TIMEOUT: Duration = Duration::from_secs(5);
pub(super) const DOWNSTREAM_BYTES: &[u8] = b"downstream-to-upstream";
pub(super) const UPSTREAM_GREETING: &[u8] = b"upstream-to-downstream";
pub(super) const UPSTREAM_ACK: &[u8] = b"upstream-received-downstream";

pub(super) type ReportReceiver<T> = mpsc::UnboundedReceiver<Result<T, String>>;
type ReportSender<T> = mpsc::UnboundedSender<Result<T, String>>;

#[derive(Debug)]
pub(super) struct TunnelObservation {
    pub(super) method: http::Method,
    pub(super) uri: http::Uri,
    pub(super) version: http::Version,
    pub(super) host: Option<String>,
    pub(super) upgrade: Option<String>,
    pub(super) protocol: Option<String>,
    pub(super) bytes: Vec<u8>,
}

impl TunnelObservation {
    pub(super) fn from_request(request: &Request<hyper::body::Incoming>) -> Self {
        Self {
            method: request.method().clone(),
            uri: request.uri().clone(),
            version: request.version(),
            host: request
                .headers()
                .get(http::header::HOST)
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned),
            upgrade: request
                .headers()
                .get(http::header::UPGRADE)
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned),
            protocol: request
                .extensions()
                .get::<hyper::ext::Protocol>()
                .map(|protocol| protocol.as_str().to_owned()),
            bytes: Vec::new(),
        }
    }
}

#[derive(Clone, Copy)]
pub(super) enum UpstreamProtocol {
    Http1,
    H2c,
}

pub(super) async fn start_broker(
    upstream: http::Uri,
    downstream_protocol: UpstreamProtocol,
    upstream_protocol: UpstreamProtocol,
) -> (SocketAddr, ReportReceiver<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (bridge_tx, bridge_rx) = mpsc::unbounded_channel();
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .timeout(TEST_TIMEOUT)
        .build()
        .unwrap();

    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let io = aioduct::runtime::tokio_rt::TokioIo::new(stream);
        let service = service_fn(move |request: Request<hyper::body::Incoming>| {
            forward_incoming_tunnel(
                client.clone(),
                upstream.clone(),
                request,
                upstream_protocol,
                bridge_tx.clone(),
            )
        });

        match downstream_protocol {
            UpstreamProtocol::Http1 => {
                let _ = server_http1::Builder::new()
                    .serve_connection(io, service)
                    .with_upgrades()
                    .await;
            }
            UpstreamProtocol::H2c => {
                let _ = server_http2::Builder::new(TokioExec)
                    .enable_connect_protocol()
                    .serve_connection(io, service)
                    .await;
            }
        }
    });

    (addr, bridge_rx)
}

async fn forward_incoming_tunnel(
    client: HttpEngineSend<TokioRuntime, TcpConnector>,
    upstream: http::Uri,
    mut request: Request<hyper::body::Incoming>,
    upstream_protocol: UpstreamProtocol,
    bridge_tx: ReportSender<()>,
) -> Result<Response<Full<Bytes>>, Infallible> {
    let downstream_version = request.version();
    // Only the downstream upgrade handle is removed. The received request and
    // its real `Incoming` body are passed directly to `forward`.
    let downstream_upgrade = hyper::upgrade::on(&mut request);
    let mut forward = client.forward(request).upstream(upstream);
    if matches!(upstream_protocol, UpstreamProtocol::H2c) {
        forward = forward.h2c();
    }

    let upstream_response = match forward.send().await {
        Ok(response) => response,
        Err(error) => {
            return Ok(Response::builder()
                .status(http::StatusCode::BAD_GATEWAY)
                .body(Full::new(Bytes::from(error.to_string())))
                .unwrap());
        }
    };
    let status = upstream_response.status();
    let headers = upstream_response.headers().clone();
    tokio::spawn(async move {
        let result = bridge_tunnels(downstream_upgrade, upstream_response).await;
        let _ = bridge_tx.send(result);
    });

    let mut response = Response::new(Full::new(Bytes::new()));
    *response.status_mut() = status;
    *response.version_mut() = downstream_version;
    *response.headers_mut() = headers;
    Ok(response)
}

async fn bridge_tunnels(
    downstream_upgrade: hyper::upgrade::OnUpgrade,
    upstream_response: aioduct::Response,
) -> Result<(), String> {
    let downstream = downstream_upgrade
        .await
        .map_err(|error| format!("downstream upgrade failed: {error}"))?;
    let mut downstream = aioduct::UpgradedSend::from(downstream);
    let mut upstream = upstream_response
        .upgrade()
        .await
        .map_err(|error| format!("upstream upgrade failed: {error}"))?;
    tokio::io::copy_bidirectional(&mut downstream, &mut upstream)
        .await
        .map_err(|error| format!("tunnel bridge failed: {error}"))?;
    Ok(())
}

pub(super) fn spawn_tunnel_peer(
    upgrade: hyper::upgrade::OnUpgrade,
    mut observation: TunnelObservation,
    report_tx: ReportSender<TunnelObservation>,
) {
    tokio::spawn(async move {
        let result = async {
            let upgraded = upgrade
                .await
                .map_err(|error| format!("upstream upgrade failed: {error}"))?;
            let mut tunnel = aioduct::UpgradedSend::from(upgraded);
            tunnel
                .write_all(UPSTREAM_GREETING)
                .await
                .map_err(|error| format!("upstream greeting failed: {error}"))?;
            tunnel
                .flush()
                .await
                .map_err(|error| format!("upstream greeting flush failed: {error}"))?;

            observation.bytes.resize(DOWNSTREAM_BYTES.len(), 0);
            tunnel
                .read_exact(&mut observation.bytes)
                .await
                .map_err(|error| format!("upstream tunnel read failed: {error}"))?;
            tunnel
                .write_all(UPSTREAM_ACK)
                .await
                .map_err(|error| format!("upstream acknowledgement failed: {error}"))?;
            tunnel
                .flush()
                .await
                .map_err(|error| format!("upstream acknowledgement flush failed: {error}"))?;
            tunnel
                .shutdown()
                .await
                .map_err(|error| format!("upstream tunnel shutdown failed: {error}"))?;
            let mut extra = [0_u8; 1];
            let read = tunnel
                .read(&mut extra)
                .await
                .map_err(|error| format!("upstream tunnel EOF read failed: {error}"))?;
            if read != 0 {
                return Err("upstream received bytes after the expected payload".to_owned());
            }
            Ok(observation)
        }
        .await;
        let _ = report_tx.send(result);
    });
}

pub(super) async fn exchange_tunnel<T>(tunnel: &mut T)
where
    T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let mut greeting = vec![0; UPSTREAM_GREETING.len()];
    tunnel.read_exact(&mut greeting).await.unwrap();
    assert_eq!(greeting, UPSTREAM_GREETING);

    tunnel.write_all(DOWNSTREAM_BYTES).await.unwrap();
    tunnel.flush().await.unwrap();

    let mut acknowledgement = vec![0; UPSTREAM_ACK.len()];
    tunnel.read_exact(&mut acknowledgement).await.unwrap();
    assert_eq!(acknowledgement, UPSTREAM_ACK);
    tunnel.shutdown().await.unwrap();
}

pub(super) async fn open_raw_h1_tunnel(
    addr: SocketAddr,
    request: &str,
    expected_status: http::StatusCode,
) -> TcpStream {
    open_raw_h1_tunnel_with_headers(addr, request, expected_status)
        .await
        .0
}

pub(super) async fn open_raw_h1_tunnel_with_headers(
    addr: SocketAddr,
    request: &str,
    expected_status: http::StatusCode,
) -> (TcpStream, String) {
    let mut stream = TcpStream::connect(addr).await.unwrap();
    stream.write_all(request.as_bytes()).await.unwrap();
    stream.flush().await.unwrap();

    let mut headers = Vec::new();
    while !headers.ends_with(b"\r\n\r\n") {
        assert!(headers.len() < 16 * 1024, "response headers are too large");
        headers.push(stream.read_u8().await.unwrap());
    }
    let headers = String::from_utf8(headers).unwrap();
    let status = headers
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|value| value.parse::<u16>().ok())
        .and_then(|value| http::StatusCode::from_u16(value).ok())
        .expect("response status line should be valid");
    assert_eq!(status, expected_status, "unexpected response: {headers}");
    (stream, headers)
}

pub(super) async fn receive_report<T>(reports: &mut ReportReceiver<T>, description: &str) -> T {
    tokio::time::timeout(TEST_TIMEOUT, reports.recv())
        .await
        .unwrap_or_else(|_| panic!("timed out waiting for {description}"))
        .unwrap_or_else(|| panic!("{description} channel closed"))
        .unwrap_or_else(|error| panic!("{description} failed: {error}"))
}

pub(super) fn tunnel_report_channel<T>() -> (ReportSender<T>, ReportReceiver<T>) {
    mpsc::unbounded_channel()
}
