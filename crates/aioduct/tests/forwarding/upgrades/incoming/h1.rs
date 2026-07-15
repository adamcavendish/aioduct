use std::convert::Infallible;
use std::net::SocketAddr;

use bytes::Bytes;
use http_body_util::Full;
use hyper::server::conn::http1 as server_http1;
use hyper::service::service_fn;
use hyper::{Request, Response};
use tokio::net::TcpListener;

use super::support::{
    DOWNSTREAM_BYTES, ReportReceiver, TEST_TIMEOUT, TunnelObservation, UpstreamProtocol,
    exchange_tunnel, open_raw_h1_tunnel, open_raw_h1_tunnel_with_headers, receive_report,
    spawn_tunnel_peer, start_broker, tunnel_report_channel,
};

#[derive(Clone, Copy)]
enum H1TunnelKind {
    SwitchingProtocols,
    Connect,
}

async fn start_h1_upstream(kind: H1TunnelKind) -> (SocketAddr, ReportReceiver<TunnelObservation>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (report_tx, report_rx) = tunnel_report_channel();

    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let io = aioduct::runtime::tokio_rt::TokioIo::new(stream);
        let _ = server_http1::Builder::new()
            .serve_connection(
                io,
                service_fn(move |mut request: Request<hyper::body::Incoming>| {
                    let report_tx = report_tx.clone();
                    async move {
                        let observation = TunnelObservation::from_request(&request);
                        let upgrade = hyper::upgrade::on(&mut request);
                        spawn_tunnel_peer(upgrade, observation, report_tx);

                        let response = match kind {
                            H1TunnelKind::SwitchingProtocols => Response::builder()
                                .status(http::StatusCode::SWITCHING_PROTOCOLS)
                                .header(http::header::CONNECTION, "upgrade")
                                .header(http::header::UPGRADE, "aioduct-test")
                                .body(Full::new(Bytes::new()))
                                .unwrap(),
                            H1TunnelKind::Connect => Response::new(Full::new(Bytes::new())),
                        };
                        Ok::<_, Infallible>(response)
                    }
                }),
            )
            .with_upgrades()
            .await;
    });

    (addr, report_rx)
}

#[tokio::test]
async fn forward_real_incoming_h1_101_upgrade_round_trips_tunnel_bytes() {
    tokio::time::timeout(TEST_TIMEOUT, async {
        let (upstream_addr, mut upstream_reports) =
            start_h1_upstream(H1TunnelKind::SwitchingProtocols).await;
        let (broker_addr, mut bridge_reports) = start_broker(
            format!("http://{upstream_addr}").parse().unwrap(),
            UpstreamProtocol::Http1,
            UpstreamProtocol::Http1,
        )
        .await;
        let request = format!(
            "GET /chat HTTP/1.1\r\nHost: {broker_addr}\r\nConnection: upgrade\r\nUpgrade: aioduct-test\r\n\r\n"
        );

        let mut tunnel = open_raw_h1_tunnel(
            broker_addr,
            &request,
            http::StatusCode::SWITCHING_PROTOCOLS,
        )
        .await;
        exchange_tunnel(&mut tunnel).await;
        drop(tunnel);

        let observed = receive_report(&mut upstream_reports, "H1 upgrade upstream report").await;
        assert_eq!(observed.method, http::Method::GET);
        assert_eq!(observed.uri.path(), "/chat");
        assert_eq!(observed.version, http::Version::HTTP_11);
        assert_eq!(observed.upgrade.as_deref(), Some("aioduct-test"));
        assert_eq!(observed.protocol, None);
        assert_eq!(observed.bytes, DOWNSTREAM_BYTES);
        receive_report(&mut bridge_reports, "H1 upgrade bridge report").await;
    })
    .await
    .expect("real Incoming H1 upgrade test timed out");
}

#[tokio::test]
async fn forward_real_incoming_h1_connect_200_round_trips_tunnel_bytes() {
    tokio::time::timeout(TEST_TIMEOUT, async {
        let (upstream_addr, mut upstream_reports) = start_h1_upstream(H1TunnelKind::Connect).await;
        let (broker_addr, mut bridge_reports) = start_broker(
            format!("http://{upstream_addr}").parse().unwrap(),
            UpstreamProtocol::Http1,
            UpstreamProtocol::Http1,
        )
        .await;
        let request = "CONNECT target.example:443 HTTP/1.1\r\n\
                       Host: target.example:443\r\n\
                       Proxy-Connection: keep-alive\r\n\r\n";

        let mut tunnel = open_raw_h1_tunnel(broker_addr, request, http::StatusCode::OK).await;
        exchange_tunnel(&mut tunnel).await;
        drop(tunnel);

        let observed = receive_report(&mut upstream_reports, "H1 CONNECT upstream report").await;
        assert_eq!(observed.method, http::Method::CONNECT);
        assert_eq!(
            observed.uri.authority().map(http::uri::Authority::as_str),
            Some("target.example:443")
        );
        assert_eq!(observed.version, http::Version::HTTP_11);
        assert_eq!(observed.upgrade, None);
        assert_eq!(observed.protocol, None);
        assert_eq!(observed.bytes, DOWNSTREAM_BYTES);
        receive_report(&mut bridge_reports, "H1 CONNECT bridge report").await;
    })
    .await
    .expect("real Incoming H1 CONNECT test timed out");
}

#[tokio::test]
async fn forward_raw_http10_connect_preserves_authority_and_tunnel_wire_semantics() {
    tokio::time::timeout(TEST_TIMEOUT, async {
        let (upstream_addr, mut upstream_reports) = start_h1_upstream(H1TunnelKind::Connect).await;
        let (broker_addr, mut bridge_reports) = start_broker(
            format!("http://{upstream_addr}").parse().unwrap(),
            UpstreamProtocol::Http1,
            UpstreamProtocol::Http1,
        )
        .await;
        let request = "CONNECT target.example:443 HTTP/1.0\r\n\r\n";

        let (mut tunnel, response_headers) =
            open_raw_h1_tunnel_with_headers(broker_addr, request, http::StatusCode::OK).await;
        assert!(
            response_headers.starts_with("HTTP/1.0 200 "),
            "unexpected downstream response: {response_headers}"
        );
        exchange_tunnel(&mut tunnel).await;
        drop(tunnel);

        let observed =
            receive_report(&mut upstream_reports, "HTTP/1.0 CONNECT upstream report").await;
        assert_eq!(observed.method, http::Method::CONNECT);
        assert_eq!(
            observed.uri.authority().map(http::uri::Authority::as_str),
            Some("target.example:443")
        );
        assert_eq!(observed.version, http::Version::HTTP_11);
        assert_eq!(observed.host.as_deref(), Some("target.example:443"));
        assert_eq!(observed.upgrade, None);
        assert_eq!(observed.protocol, None);
        assert_eq!(observed.bytes, DOWNSTREAM_BYTES);
        receive_report(&mut bridge_reports, "HTTP/1.0 CONNECT bridge report").await;
    })
    .await
    .expect("raw HTTP/1.0 CONNECT tunnel test timed out");
}
