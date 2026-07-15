use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use aioduct::HttpEngineLocal;
use aioduct::runtime::RuntimeCompletion as _;
use aioduct::runtime::compio_rt::{CompioRuntime, TcpConnector};
use bytes::Bytes;
use http_body_util::Full;
use hyper::{Request, Response};

const CLEANUP_TIMEOUT: Duration = Duration::from_secs(2);

async fn read_h1_request_head(stream: &mut tokio::net::TcpStream) -> Vec<u8> {
    use tokio::io::AsyncReadExt as _;

    let mut request = Vec::new();
    let mut buffer = [0_u8; 1024];
    while !request.windows(4).any(|window| window == b"\r\n\r\n") {
        let read = tokio::time::timeout(CLEANUP_TIMEOUT, stream.read(&mut buffer))
            .await
            .expect("HTTP/1.1 request head timed out")
            .unwrap();
        assert_ne!(read, 0, "client closed before sending request headers");
        request.extend_from_slice(&buffer[..read]);
    }
    request
}

fn start_h1_connect_cleanup_server() -> (SocketAddr, Arc<AtomicBool>, JoinHandle<()>) {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    let (addr_tx, addr_rx) = std::sync::mpsc::channel();
    let closed = Arc::new(AtomicBool::new(false));
    let server_closed = closed.clone();
    let server = std::thread::spawn(move || {
        tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(async move {
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                addr_tx.send(listener.local_addr().unwrap()).unwrap();

                let (mut tunnel, _) = tokio::time::timeout(CLEANUP_TIMEOUT, listener.accept())
                    .await
                    .expect("CONNECT connection was not accepted")
                    .unwrap();
                let request = read_h1_request_head(&mut tunnel).await;
                assert!(
                    request.starts_with(b"CONNECT target.example:443 HTTP/1.1\r\n"),
                    "unexpected CONNECT request: {}",
                    String::from_utf8_lossy(&request)
                );
                tunnel
                    .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                    .await
                    .unwrap();
                tunnel.flush().await.unwrap();

                let mut byte = [0_u8; 1];
                match tokio::time::timeout(CLEANUP_TIMEOUT, tunnel.read(&mut byte)).await {
                    Ok(Ok(0) | Err(_)) => {}
                    Ok(Ok(read)) => {
                        panic!("rejected Local CONNECT tunnel remained writable ({read} byte)")
                    }
                    Err(_) => panic!("response-hook failure did not close Local CONNECT tunnel"),
                }
                server_closed.store(true, Ordering::Release);

                let (mut follow_up, _) = tokio::time::timeout(CLEANUP_TIMEOUT, listener.accept())
                    .await
                    .expect("Local follow-up connection was not accepted")
                    .unwrap();
                let request = read_h1_request_head(&mut follow_up).await;
                assert!(
                    request.starts_with(b"GET /after HTTP/1.1\r\n"),
                    "unexpected Local follow-up request: {}",
                    String::from_utf8_lossy(&request)
                );
                follow_up
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nafter",
                    )
                    .await
                    .unwrap();
            });
    });

    (addr_rx.recv().unwrap(), closed, server)
}

fn start_h2_connect_cleanup_server() -> (SocketAddr, Arc<AtomicBool>, JoinHandle<()>) {
    let (addr_tx, addr_rx) = std::sync::mpsc::channel();
    let reset = Arc::new(AtomicBool::new(false));
    let server_reset = reset.clone();
    let server = std::thread::spawn(move || {
        tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(async move {
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                addr_tx.send(listener.local_addr().unwrap()).unwrap();

                let (stream, _) = tokio::time::timeout(CLEANUP_TIMEOUT, listener.accept())
                    .await
                    .expect("Local H2 CONNECT connection was not accepted")
                    .unwrap();
                let mut connection = h2::server::handshake(stream).await.unwrap();
                let (request, mut respond) =
                    tokio::time::timeout(CLEANUP_TIMEOUT, connection.accept())
                        .await
                        .expect("Local H2 CONNECT request timed out")
                        .unwrap()
                        .unwrap();
                assert_eq!(request.method(), http::Method::CONNECT);
                assert_eq!(request.uri().authority().unwrap(), "target.example:443");
                let mut tunnel = respond
                    .send_response(Response::builder().status(200).body(()).unwrap(), false)
                    .unwrap();

                let reset = std::future::poll_fn(|context| tunnel.poll_reset(context));
                tokio::pin!(reset);
                let reason = tokio::select! {
                    biased;
                    reason = &mut reset => reason,
                    accepted = connection.accept() => match accepted {
                        Some(Ok((request, _))) => panic!(
                            "request arrived before rejected Local CONNECT reset: {request:?}"
                        ),
                        Some(Err(error)) => {
                            panic!("Local H2 connection failed before reset: {error}")
                        }
                        None => panic!(
                            "response-hook failure closed the entire Local H2 connection"
                        ),
                    },
                    _ = tokio::time::sleep(CLEANUP_TIMEOUT) => {
                        panic!("response-hook failure did not reset Local H2 CONNECT stream")
                    }
                };
                assert_eq!(reason.unwrap(), h2::Reason::CANCEL);
                drop(tunnel);
                server_reset.store(true, Ordering::Release);

                enum FollowUp {
                    Existing(Box<(Request<h2::RecvStream>, h2::server::SendResponse<Bytes>)>),
                    Fresh(tokio::net::TcpStream),
                }
                let follow_up = tokio::select! {
                    accepted = connection.accept() => match accepted {
                        Some(Ok(request)) => FollowUp::Existing(Box::new(request)),
                        Some(Err(_)) | None => {
                            let (stream, _) = listener.accept().await.unwrap();
                            FollowUp::Fresh(stream)
                        }
                    },
                    accepted = listener.accept() => {
                        let (stream, _) = accepted.unwrap();
                        FollowUp::Fresh(stream)
                    },
                    _ = tokio::time::sleep(CLEANUP_TIMEOUT) => {
                        panic!("Local H2 follow-up request timed out")
                    }
                };
                match follow_up {
                    FollowUp::Existing(existing) => {
                        let (request, mut respond) = *existing;
                        assert_eq!(request.uri().path(), "/after");
                        respond
                            .send_response(Response::builder().status(200).body(()).unwrap(), true)
                            .unwrap();
                        let _ =
                            tokio::time::timeout(Duration::from_millis(100), connection.accept())
                                .await;
                    }
                    FollowUp::Fresh(stream) => {
                        let mut fresh = h2::server::handshake(stream).await.unwrap();
                        let (request, mut respond) =
                            tokio::time::timeout(CLEANUP_TIMEOUT, fresh.accept())
                                .await
                                .expect("fresh Local H2 follow-up request timed out")
                                .unwrap()
                                .unwrap();
                        assert_eq!(request.uri().path(), "/after");
                        respond
                            .send_response(Response::builder().status(200).body(()).unwrap(), true)
                            .unwrap();
                        let _ =
                            tokio::time::timeout(Duration::from_millis(100), fresh.accept()).await;
                    }
                }
            });
    });

    (addr_rx.recv().unwrap(), reset, server)
}

async fn wait_for_cleanup(cleaned_up: &AtomicBool, description: &str) {
    compio_runtime::time::timeout(CLEANUP_TIMEOUT, async {
        while !cleaned_up.load(Ordering::Acquire) {
            CompioRuntime::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("{description} was not cleaned up"));
}

fn bad_gateway_replacement(addr: SocketAddr) -> aioduct::Response {
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        aioduct::HttpEngineSend::<
            aioduct::runtime::TokioRuntime,
            aioduct::runtime::tokio_rt::TcpConnector,
        >::new()
        .get(&format!("http://{addr}/replacement"))
        .unwrap()
        .send()
        .await
        .unwrap()
    })
}

#[test]
fn forward_local_on_response_cannot_demote_h1_connect_and_closes_tunnel() {
    let (addr, closed, server) = start_h1_connect_cleanup_server();
    let replacement_addr = super::super::start_server_with_tokio(|_request| async move {
        Ok::<_, std::convert::Infallible>(
            Response::builder()
                .status(http::StatusCode::BAD_GATEWAY)
                .body(Full::new(Bytes::new()))
                .unwrap(),
        )
    });
    let replacement = bad_gateway_replacement(replacement_addr);

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::new();
        let request = Request::builder()
            .method(http::Method::CONNECT)
            .uri("target.example:443")
            .version(http::Version::HTTP_11)
            .header(http::header::HOST, "target.example:443")
            .body(Full::new(Bytes::new()))
            .unwrap();

        let error = compio_runtime::time::timeout(
            CLEANUP_TIMEOUT,
            client
                .forward_local(request)
                .upstream(format!("http://{addr}").parse::<http::Uri>().unwrap())
                .on_response(move |response| *response = replacement)
                .send(),
        )
        .await
        .expect("Local CONNECT response hook timed out")
        .unwrap_err();
        assert!(
            matches!(error, aioduct::Error::InvalidHeader(ref message) if message.contains("establishes a tunnel")),
            "{error}"
        );
        wait_for_cleanup(&closed, "Local CONNECT tunnel").await;

        let follow_up = compio_runtime::time::timeout(
            CLEANUP_TIMEOUT,
            client
                .get_local(&format!("http://{addr}/after"))
                .unwrap()
                .send(),
        )
        .await
        .expect("Local follow-up request timed out")
        .unwrap();
        assert_eq!(follow_up.text().await.unwrap(), "after");
    });

    server.join().unwrap();
}

#[test]
fn forward_local_on_response_cannot_demote_h2_connect_and_releases_stream_permit() {
    let (addr, reset, server) = start_h2_connect_cleanup_server();
    let replacement_addr = super::super::start_server_with_tokio(|_request| async move {
        Ok::<_, std::convert::Infallible>(
            Response::builder()
                .status(http::StatusCode::BAD_GATEWAY)
                .body(Full::new(Bytes::new()))
                .unwrap(),
        )
    });
    let replacement = bad_gateway_replacement(replacement_addr);

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .pool_max_active_streams_per_connection(1)
            .build_local()
            .unwrap();
        let request = Request::builder()
            .method(http::Method::CONNECT)
            .uri("target.example:443")
            .version(http::Version::HTTP_2)
            .body(Full::new(Bytes::new()))
            .unwrap();

        let error = compio_runtime::time::timeout(
            CLEANUP_TIMEOUT,
            client
                .forward_local(request)
                .upstream(format!("http://{addr}").parse::<http::Uri>().unwrap())
                .h2c()
                .on_response(move |response| *response = replacement)
                .send(),
        )
        .await
        .expect("Local H2 CONNECT response hook timed out")
        .unwrap_err();
        assert!(
            matches!(error, aioduct::Error::InvalidHeader(ref message) if message.contains("establishes a tunnel")),
            "{error}"
        );
        wait_for_cleanup(&reset, "Local H2 CONNECT stream").await;

        let follow_up = compio_runtime::time::timeout(
            CLEANUP_TIMEOUT,
            client
                .get_local(&format!("http://{addr}/after"))
                .unwrap()
                .h2c_prior_knowledge()
                .send(),
        )
        .await
        .expect("Local H2 follow-up request timed out")
        .unwrap();
        assert_eq!(follow_up.status(), http::StatusCode::OK);
    });

    server.join().unwrap();
}
