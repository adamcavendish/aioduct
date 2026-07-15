use std::convert::Infallible;
use std::net::SocketAddr;

use aioduct_test_server::TokioExec;
use bytes::Bytes;
use http_body_util::Full;
use hyper::client::conn::http2 as client_http2;
use hyper::server::conn::http2 as server_http2;
use hyper::service::service_fn;
use hyper::{Request, Response};
use tokio::net::{TcpListener, TcpStream};

use super::support::{
    DOWNSTREAM_BYTES, ReportReceiver, TEST_TIMEOUT, TunnelObservation, UpstreamProtocol,
    exchange_tunnel, receive_report, spawn_tunnel_peer, start_broker, tunnel_report_channel,
};

async fn start_h2_upstream() -> (SocketAddr, ReportReceiver<TunnelObservation>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (report_tx, report_rx) = tunnel_report_channel();

    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let io = aioduct::runtime::tokio_rt::TokioIo::new(stream);
        let _ = server_http2::Builder::new(TokioExec)
            .enable_connect_protocol()
            .serve_connection(
                io,
                service_fn(move |mut request: Request<hyper::body::Incoming>| {
                    let report_tx = report_tx.clone();
                    async move {
                        let observation = TunnelObservation::from_request(&request);
                        let upgrade = hyper::upgrade::on(&mut request);
                        spawn_tunnel_peer(upgrade, observation, report_tx);
                        Ok::<_, Infallible>(Response::new(Full::new(Bytes::new())))
                    }
                }),
            )
            .await;
    });

    (addr, report_rx)
}

#[tokio::test]
async fn forward_real_incoming_h2_extended_connect_round_trips_tunnel_bytes() {
    tokio::time::timeout(TEST_TIMEOUT, async {
        let (upstream_addr, mut upstream_reports) = start_h2_upstream().await;
        let (broker_addr, mut bridge_reports) = start_broker(
            format!("http://{upstream_addr}").parse().unwrap(),
            UpstreamProtocol::H2c,
            UpstreamProtocol::H2c,
        )
        .await;

        let stream = TcpStream::connect(broker_addr).await.unwrap();
        let io = aioduct::runtime::tokio_rt::TokioIo::new(stream);
        let (mut sender, connection) = client_http2::Builder::new(TokioExec)
            .handshake(io)
            .await
            .unwrap();
        tokio::spawn(async move {
            let _ = connection.await;
        });
        sender.ready().await.unwrap();

        let mut request = Request::builder()
            .method(http::Method::CONNECT)
            .uri("http://downstream.test/chat")
            .version(http::Version::HTTP_2)
            .body(Full::new(Bytes::new()))
            .unwrap();
        request
            .extensions_mut()
            .insert(hyper::ext::Protocol::from_static("websocket"));
        let mut response = sender.send_request(request).await.unwrap();
        assert_eq!(response.status(), http::StatusCode::OK);

        let upgraded = hyper::upgrade::on(&mut response).await.unwrap();
        let mut tunnel = aioduct::UpgradedSend::from(upgraded);
        exchange_tunnel(&mut tunnel).await;
        drop(tunnel);
        drop(sender);

        let observed = receive_report(&mut upstream_reports, "H2 CONNECT upstream report").await;
        assert_eq!(observed.method, http::Method::CONNECT);
        assert_eq!(observed.uri.scheme_str(), Some("http"));
        assert_eq!(
            observed.uri.authority().map(http::uri::Authority::as_str),
            Some(upstream_addr.to_string().as_str())
        );
        assert_eq!(observed.uri.path(), "/chat");
        assert_eq!(observed.version, http::Version::HTTP_2);
        assert_eq!(observed.upgrade, None);
        assert_eq!(observed.protocol.as_deref(), Some("websocket"));
        assert_eq!(observed.bytes, DOWNSTREAM_BYTES);
        receive_report(&mut bridge_reports, "H2 CONNECT bridge report").await;
    })
    .await
    .expect("real Incoming H2 extended CONNECT test timed out");
}
