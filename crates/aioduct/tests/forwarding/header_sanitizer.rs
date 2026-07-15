use std::convert::Infallible;

use aioduct::HttpEngineSend;
use aioduct::runtime::TokioRuntime;
use aioduct::runtime::tokio_rt::TcpConnector;
use bytes::Bytes;
use http::header::{CONNECTION, HeaderValue, TE, UPGRADE};
use http_body_util::Full;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response};
use tokio::net::TcpListener;

fn empty_request() -> Request<Full<Bytes>> {
    Request::builder()
        .method(http::Method::GET)
        .uri("/headers")
        .body(Full::new(Bytes::new()))
        .unwrap()
}

async fn start_h1_server<F, Fut>(handler: F) -> std::net::SocketAddr
where
    F: Fn(Request<hyper::body::Incoming>) -> Fut + Send + Clone + 'static,
    Fut: Future<Output = Result<Response<Full<Bytes>>, Infallible>> + Send + 'static,
{
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let handler = handler.clone();
            tokio::spawn(async move {
                let io = aioduct::runtime::tokio_rt::TokioIo::new(stream);
                let _ = http1::Builder::new()
                    .serve_connection(io, service_fn(handler))
                    .await;
            });
        }
    });
    addr
}

#[tokio::test]
async fn forward_request_strips_repeated_connection_options_and_orphan_upgrade() {
    let addr = start_h1_server(|req| async move {
        let names = [
            CONNECTION.as_str(),
            "x-first-hop",
            "x-second-hop",
            UPGRADE.as_str(),
        ];
        let leaked = names
            .iter()
            .filter(|name| req.headers().contains_key(**name))
            .copied()
            .collect::<Vec<_>>()
            .join(",");
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(leaked))))
    })
    .await;
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();
    let mut request = empty_request();
    request.headers_mut().append(
        CONNECTION,
        HeaderValue::from_static("X-First-Hop, keep-alive"),
    );
    request.headers_mut().append(
        CONNECTION,
        HeaderValue::from_static("x-SECOND-hop, bad token"),
    );
    request
        .headers_mut()
        .insert("x-first-hop", HeaderValue::from_static("secret-1"));
    request
        .headers_mut()
        .insert("x-second-hop", HeaderValue::from_static("secret-2"));
    request
        .headers_mut()
        .insert(UPGRADE, HeaderValue::from_static("h2c"));

    let response = client
        .forward(crate::valid_forward_request(request))
        .upstream(format!("http://{addr}").parse::<http::Uri>().unwrap())
        .send()
        .await
        .unwrap();

    assert_eq!(response.text().await.unwrap(), "");
}

#[tokio::test]
async fn forward_request_strips_connection_options_from_non_utf8_values() {
    let addr = start_h1_server(|req| async move {
        let leaked = req.headers().contains_key("x-secret");
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(leaked.to_string()))))
    })
    .await;
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();
    let mut request = empty_request();
    request.headers_mut().insert(
        CONNECTION,
        HeaderValue::from_bytes(b"x-secret, \x80").unwrap(),
    );
    request
        .headers_mut()
        .insert("x-secret", HeaderValue::from_static("must not leak"));

    let response = client
        .forward(crate::valid_forward_request(request))
        .upstream(format!("http://{addr}").parse::<http::Uri>().unwrap())
        .send()
        .await
        .unwrap();

    assert_eq!(response.text().await.unwrap(), "false");
}

#[tokio::test]
async fn forward_h1_strips_te_trailers() {
    let addr = start_h1_server(|req| async move {
        let te = req
            .headers()
            .get(TE)
            .map(|value| value.to_str().unwrap().to_owned())
            .unwrap_or_else(|| "absent".to_owned());
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(te))))
    })
    .await;
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();
    let mut request = empty_request();
    request
        .headers_mut()
        .insert(TE, HeaderValue::from_static("trailers"));

    let response = client
        .forward(crate::valid_forward_request(request))
        .upstream(format!("http://{addr}").parse::<http::Uri>().unwrap())
        .send()
        .await
        .unwrap();

    assert_eq!(response.text().await.unwrap(), "absent");
}

#[tokio::test]
async fn forward_h2c_preserves_canonical_te_trailers() {
    let (addr, _) = aioduct_test_server::h2::h2_server_with(|req| async move {
        let te = req.headers().get(TE).unwrap().to_str().unwrap().to_owned();
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(te))))
    })
    .await;
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();
    let mut request = empty_request();
    request
        .headers_mut()
        .append(TE, HeaderValue::from_static("Trailers"));
    request
        .headers_mut()
        .append(TE, HeaderValue::from_static("trailers, TRAILERS"));

    let response = client
        .forward(crate::valid_forward_request(request))
        .upstream(format!("http://{addr}").parse::<http::Uri>().unwrap())
        .h2c()
        .send()
        .await
        .unwrap();

    assert_eq!(response.text().await.unwrap(), "trailers");
}

#[tokio::test]
async fn forward_h2c_rejects_invalid_te_before_io() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let addr = listener.local_addr().unwrap();
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();
    let mut request = empty_request();
    request
        .headers_mut()
        .insert(TE, HeaderValue::from_static("trailers, gzip"));

    let error = client
        .forward(crate::valid_forward_request(request))
        .upstream(format!("http://{addr}").parse::<http::Uri>().unwrap())
        .h2c()
        .send()
        .await
        .unwrap_err();

    assert!(matches!(error, aioduct::Error::InvalidHeader(_)));
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock,
        "invalid TE must be rejected before TCP I/O"
    );
}

#[cfg(all(feature = "rustls", feature = "http3"))]
#[tokio::test]
async fn forward_exact_h3_preserves_te_trailers() {
    let (addr, _, _) = aioduct_test_server::h3::h3_server_with(|req, _body| {
        let te = req.headers().get(TE).unwrap().to_str().unwrap().to_owned();
        (http::StatusCode::OK, Bytes::from(te))
    })
    .await;
    let client: HttpEngineSend<TokioRuntime, TcpConnector> = HttpEngineSend::builder()
        .tls(aioduct::tls::RustlsConnector::danger_accept_invalid_certs())
        .http3(true)
        .unwrap()
        .build()
        .unwrap();
    let mut request = empty_request();
    request
        .headers_mut()
        .insert(TE, HeaderValue::from_static("trailers"));

    let response = client
        .forward(crate::valid_forward_request(request))
        .upstream(
            format!("https://127.0.0.1:{}", addr.port())
                .parse::<http::Uri>()
                .unwrap(),
        )
        .on_request(|parts| parts.version = http::Version::HTTP_3)
        .send()
        .await
        .unwrap();

    assert_eq!(response.text().await.unwrap(), "trailers");
}

#[tokio::test]
async fn forward_response_sanitizes_upstream_and_hook_connection_options() {
    let addr = start_h1_server(|_req| async move {
        let mut response = Response::new(Full::new(Bytes::from_static(b"ok")));
        response
            .headers_mut()
            .append(CONNECTION, HeaderValue::from_static("x-upstream-one"));
        response.headers_mut().append(
            CONNECTION,
            HeaderValue::from_static("X-Upstream-Two, bad token"),
        );
        response
            .headers_mut()
            .insert("x-upstream-one", HeaderValue::from_static("secret-1"));
        response
            .headers_mut()
            .insert("x-upstream-two", HeaderValue::from_static("secret-2"));
        Ok::<_, Infallible>(response)
    })
    .await;
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();

    let response = client
        .forward(crate::valid_forward_request(empty_request()))
        .upstream(format!("http://{addr}").parse::<http::Uri>().unwrap())
        .on_response(|response| {
            response
                .headers_mut()
                .append(CONNECTION, HeaderValue::from_static("x-hook-hop"));
            response
                .headers_mut()
                .insert("x-hook-hop", HeaderValue::from_static("secret-3"));
        })
        .send()
        .await
        .unwrap();

    for name in [
        CONNECTION.as_str(),
        "x-upstream-one",
        "x-upstream-two",
        "x-hook-hop",
    ] {
        assert!(!response.headers().contains_key(name), "leaked {name}");
    }
}

#[tokio::test]
async fn forward_h1_response_sanitizes_before_h2_h3_downstream_validation() {
    let addr = start_h1_server(|_req| async move {
        let response = Response::builder()
            .header(CONNECTION, "close, x-upstream-hop")
            .header(http::header::TRANSFER_ENCODING, "chunked")
            .header("x-upstream-hop", "remove")
            .header("x-end-to-end", "preserve")
            .body(Full::new(Bytes::from_static(b"ok")))
            .unwrap();
        Ok::<_, Infallible>(response)
    })
    .await;
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();

    for version in [http::Version::HTTP_2, http::Version::HTTP_3] {
        let mut request = empty_request();
        *request.version_mut() = version;
        let response = client
            .forward(request)
            .upstream(format!("http://{addr}").parse::<http::Uri>().unwrap())
            .send()
            .await
            .unwrap();

        assert_eq!(response.version(), version);
        assert!(!response.headers().contains_key(CONNECTION));
        assert!(
            !response
                .headers()
                .contains_key(http::header::TRANSFER_ENCODING)
        );
        assert!(!response.headers().contains_key("x-upstream-hop"));
        assert_eq!(response.headers()["x-end-to-end"], "preserve");
        assert_eq!(response.text().await.unwrap(), "ok");
    }
}

#[tokio::test]
async fn forward_response_strips_connection_options_from_non_utf8_values() {
    let addr = start_h1_server(|_req| async move {
        let mut response = Response::new(Full::new(Bytes::new()));
        response.headers_mut().insert(
            CONNECTION,
            HeaderValue::from_bytes(b"x-secret, \x80").unwrap(),
        );
        response
            .headers_mut()
            .insert("x-secret", HeaderValue::from_static("must not leak"));
        Ok::<_, Infallible>(response)
    })
    .await;
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();

    let response = client
        .forward(crate::valid_forward_request(empty_request()))
        .upstream(format!("http://{addr}").parse::<http::Uri>().unwrap())
        .send()
        .await
        .unwrap();

    assert!(!response.headers().contains_key(CONNECTION));
    assert!(!response.headers().contains_key("x-secret"));
}

#[tokio::test]
async fn rejected_h1_upgrade_response_gets_ordinary_cleanup() {
    let addr = start_h1_server(|_req| async move {
        let response = Response::builder()
            .status(http::StatusCode::BAD_REQUEST)
            .header(CONNECTION, "upgrade, x-hop")
            .header(UPGRADE, "websocket")
            .header("x-hop", "remove")
            .body(Full::new(Bytes::from_static(b"rejected")))
            .unwrap();
        Ok::<_, Infallible>(response)
    })
    .await;
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();
    let request = Request::builder()
        .method(http::Method::GET)
        .uri("/upgrade")
        .header(CONNECTION, "upgrade")
        .header(UPGRADE, "websocket")
        .body(Full::new(Bytes::new()))
        .unwrap();

    let response = client
        .forward(crate::valid_forward_request(request))
        .upstream(format!("http://{addr}").parse::<http::Uri>().unwrap())
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), http::StatusCode::BAD_REQUEST);
    assert!(!response.headers().contains_key(CONNECTION));
    assert!(!response.headers().contains_key(UPGRADE));
    assert!(!response.headers().contains_key("x-hop"));
}

#[cfg(all(feature = "rustls", feature = "http3"))]
#[tokio::test]
async fn forward_h3_preserves_inbound_early_data_and_returns_425() {
    let (addr, _, counter) = aioduct_test_server::h3::h3_server_with(|request, _body| {
        let early_data = request
            .headers()
            .get("early-data")
            .and_then(|value| value.to_str().ok())
            .unwrap_or("missing");
        (
            http::StatusCode::TOO_EARLY,
            Bytes::from(early_data.to_owned()),
        )
    })
    .await;
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .tls(aioduct::tls::RustlsConnector::danger_accept_invalid_certs())
        .http3(true)
        .unwrap()
        .build()
        .unwrap();
    let request = Request::builder()
        .method(http::Method::GET)
        .uri("/early-data")
        .header("early-data", "1")
        .body(Full::new(Bytes::new()))
        .unwrap();

    let response = client
        .forward(crate::valid_forward_request(request))
        .upstream(
            format!("https://127.0.0.1:{}", addr.port())
                .parse::<http::Uri>()
                .unwrap(),
        )
        .on_request(|parts| parts.version = http::Version::HTTP_3)
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), http::StatusCode::TOO_EARLY);
    assert_eq!(response.text().await.unwrap(), "1");
    assert_eq!(counter.requests(), 1);
}
