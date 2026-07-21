use std::convert::Infallible;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::task::{Context, Poll};
use std::time::Duration;

use aioduct::HttpEngineSend;
use aioduct::runtime::TokioRuntime;
use aioduct::runtime::tokio_rt::{TcpConnector, TokioIo};
use aioduct_test_server::ConnectionCounter;
#[cfg(all(feature = "rustls", feature = "http3"))]
use bytes::Buf;
use bytes::Bytes;
use http_body_util::{BodyExt as _, Full};
use hyper::server::conn::http1 as server_http1;
use hyper::service::service_fn;
use hyper::{Request, Response};
use tokio::net::{TcpListener, TcpStream};

const TEST_TIMEOUT: Duration = Duration::from_secs(3);
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(1);
const WRITE_TIMEOUT: Duration = Duration::from_millis(500);
const STALLED_UPLOAD_LENGTH: u64 = 1024 * 1024;
const FILE_BYTES: &[u8] = b"non-empty-file-bytes";
const MULTIPART_PREFIX: &[u8] = b"--aioduct-boundary\r\n\
Content-Disposition: form-data; name=\"file\"; filename=\"upload.bin\"\r\n\
Content-Type: application/octet-stream\r\n\r\n\
non-empty-file-bytes";

struct StalledUploadBody {
    first: Option<Bytes>,
}

impl http_body::Body for StalledUploadBody {
    type Data = Bytes;
    type Error = Infallible;

    fn poll_frame(
        self: Pin<&mut Self>,
        _context: &mut Context<'_>,
    ) -> Poll<Option<Result<http_body::Frame<Self::Data>, Self::Error>>> {
        match self.get_mut().first.take() {
            Some(first) => Poll::Ready(Some(Ok(http_body::Frame::data(first)))),
            None => Poll::Pending,
        }
    }

    fn is_end_stream(&self) -> bool {
        false
    }

    fn size_hint(&self) -> http_body::SizeHint {
        http_body::SizeHint::with_exact(STALLED_UPLOAD_LENGTH)
    }
}

#[derive(Default)]
struct UploadObservations {
    attempts: AtomicUsize,
    quiesced: AtomicBool,
}

async fn reject_upload(
    request: Request<hyper::body::Incoming>,
    observations: Arc<UploadObservations>,
) -> Result<Response<Full<Bytes>>, Infallible> {
    if request.method() == http::Method::GET {
        return Ok(Response::new(Full::new(Bytes::from_static(b"after"))));
    }

    assert_eq!(request.method(), http::Method::POST);
    assert_eq!(request.uri().path(), "/upload");
    observations.attempts.fetch_add(1, Ordering::SeqCst);
    let mut body = request.into_body();
    let mut received = Vec::new();
    while !received
        .windows(FILE_BYTES.len())
        .any(|window| window == FILE_BYTES)
    {
        let frame = body
            .frame()
            .await
            .expect("forwarded upload ended before its file bytes")
            .expect("forwarded upload failed before its file bytes");
        let data = frame
            .into_data()
            .expect("forwarded upload sent a non-data frame before its file bytes");
        received.extend_from_slice(&data);
    }

    tokio::spawn(async move {
        while let Some(frame) = body.frame().await {
            if frame.is_err() {
                break;
            }
        }
        observations.quiesced.store(true, Ordering::Release);
    });

    Ok(Response::builder()
        .status(http::StatusCode::PAYLOAD_TOO_LARGE)
        .body(Full::new(Bytes::from_static(b"upload rejected")))
        .unwrap())
}

#[derive(Clone, Copy)]
enum UpstreamProtocol {
    Automatic,
    H2c,
}

async fn start_broker(
    client: HttpEngineSend<TokioRuntime, TcpConnector>,
    upstream: http::Uri,
    protocol: UpstreamProtocol,
) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let client = client.clone();
            let upstream = upstream.clone();
            tokio::spawn(async move {
                let service = service_fn(move |request: Request<hyper::body::Incoming>| {
                    let client = client.clone();
                    let upstream = upstream.clone();
                    async move {
                        let forward = client
                            .forward(request)
                            .upstream(upstream)
                            .write_timeout(WRITE_TIMEOUT);
                        let result = match protocol {
                            UpstreamProtocol::Automatic => forward.send().await,
                            UpstreamProtocol::H2c => forward.h2c().send().await,
                        };
                        let response = match result {
                            Ok(response) => {
                                let status = response.status();
                                match response.bytes().await {
                                    Ok(body) => Response::builder()
                                        .status(status)
                                        .body(Full::new(body))
                                        .unwrap(),
                                    Err(error) => bad_gateway(error.to_string()),
                                }
                            }
                            Err(error) => bad_gateway(error.to_string()),
                        };
                        Ok::<_, Infallible>(response)
                    }
                });
                let _ = server_http1::Builder::new()
                    .serve_connection(TokioIo::new(stream), service)
                    .await;
            });
        }
    });

    addr
}

fn bad_gateway(message: String) -> Response<Full<Bytes>> {
    Response::builder()
        .status(http::StatusCode::BAD_GATEWAY)
        .body(Full::new(Bytes::from(message)))
        .unwrap()
}

async fn send_stalled_upload(addr: SocketAddr, version: http::Version) -> Bytes {
    let stream = TcpStream::connect(addr).await.unwrap();
    let (mut sender, connection) =
        hyper::client::conn::http1::handshake::<_, StalledUploadBody>(TokioIo::new(stream))
            .await
            .unwrap();
    tokio::spawn(async move {
        let _ = connection.await;
    });
    let request = Request::builder()
        .method(http::Method::POST)
        .uri("/upload")
        .version(version)
        .header(http::header::HOST, addr.to_string())
        .header(
            http::header::CONTENT_TYPE,
            "multipart/form-data; boundary=aioduct-boundary",
        )
        .header(http::header::CONTENT_LENGTH, STALLED_UPLOAD_LENGTH)
        .body(StalledUploadBody {
            first: Some(Bytes::from_static(MULTIPART_PREFIX)),
        })
        .unwrap();

    let response = tokio::time::timeout(RESPONSE_TIMEOUT, sender.send_request(request))
        .await
        .expect("broker waited for the unfinished Incoming upload")
        .unwrap();
    assert_eq!(response.version(), version);
    assert_eq!(response.status(), http::StatusCode::PAYLOAD_TOO_LARGE);
    response.collect().await.unwrap().to_bytes()
}

async fn send_follow_up(addr: SocketAddr) -> Bytes {
    let stream = TcpStream::connect(addr).await.unwrap();
    let (mut sender, connection) =
        hyper::client::conn::http1::handshake::<_, Full<Bytes>>(TokioIo::new(stream))
            .await
            .unwrap();
    tokio::spawn(async move {
        let _ = connection.await;
    });
    let response = sender
        .send_request(
            Request::builder()
                .uri("/after")
                .header(http::header::HOST, addr.to_string())
                .body(Full::new(Bytes::new()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), http::StatusCode::OK);
    response.collect().await.unwrap().to_bytes()
}

async fn wait_for_quiescence(observations: &UploadObservations) {
    tokio::time::timeout(TEST_TIMEOUT, async {
        while !observations.quiesced.load(Ordering::Acquire) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("upstream upload did not quiesce");
}

async fn exercise(
    broker: SocketAddr,
    downstream_version: http::Version,
    observations: &UploadObservations,
    counter: &ConnectionCounter,
    expected_connections: usize,
) {
    assert_eq!(
        send_stalled_upload(broker, downstream_version).await,
        Bytes::from_static(b"upload rejected")
    );
    wait_for_quiescence(observations).await;
    assert_eq!(observations.attempts.load(Ordering::SeqCst), 1);
    assert_eq!(send_follow_up(broker).await, Bytes::from_static(b"after"));
    assert_eq!(counter.requests(), 2);
    assert_eq!(counter.connections(), expected_connections);
}

async fn exercise_h1(downstream_version: http::Version) {
    let observations = Arc::new(UploadObservations::default());
    let handler_observations = observations.clone();
    let (upstream, counter) = aioduct_test_server::h1::h1_server_with(move |request| {
        reject_upload(request, handler_observations.clone())
    })
    .await;
    let broker = start_broker(
        HttpEngineSend::<TokioRuntime, TcpConnector>::new(),
        format!("http://{upstream}").parse().unwrap(),
        UpstreamProtocol::Automatic,
    )
    .await;

    exercise(broker, downstream_version, &observations, &counter, 2).await;
}

#[tokio::test]
async fn forward_real_incoming_early_response_http10_to_http11_is_prompt_and_isolated() {
    exercise_h1(http::Version::HTTP_10).await;
}

#[tokio::test]
async fn forward_real_incoming_early_response_http11_is_prompt_and_isolated() {
    exercise_h1(http::Version::HTTP_11).await;
}

#[tokio::test]
async fn forward_real_incoming_early_response_h2c_is_prompt_and_stream_isolated() {
    let observations = Arc::new(UploadObservations::default());
    let handler_observations = observations.clone();
    let (upstream, counter) = aioduct_test_server::h2::h2_server_with(move |request| {
        reject_upload(request, handler_observations.clone())
    })
    .await;
    let broker = start_broker(
        HttpEngineSend::<TokioRuntime, TcpConnector>::new(),
        format!("http://{upstream}").parse().unwrap(),
        UpstreamProtocol::H2c,
    )
    .await;

    exercise(broker, http::Version::HTTP_11, &observations, &counter, 1).await;
}

#[cfg(feature = "rustls")]
#[tokio::test]
async fn forward_real_incoming_early_response_https_h2_is_prompt_and_stream_isolated() {
    let observations = Arc::new(UploadObservations::default());
    let handler_observations = observations.clone();
    let (upstream, certificate, counter) =
        aioduct_test_server::tls::tls_h2_server_with(move |request| {
            reject_upload(request, handler_observations.clone())
        })
        .await;
    let client_config = aioduct_test_server::tls::make_client_config(&certificate);
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .tls(aioduct::tls::RustlsConnector::new(client_config))
        .build()
        .unwrap();
    let broker = start_broker(
        client,
        format!("https://localhost:{}", upstream.port())
            .parse()
            .unwrap(),
        UpstreamProtocol::Automatic,
    )
    .await;

    exercise(broker, http::Version::HTTP_11, &observations, &counter, 1).await;
}

#[cfg(all(feature = "rustls", feature = "http3"))]
#[tokio::test]
async fn forward_real_incoming_early_response_h3_is_prompt_and_stream_isolated() {
    let observations = Arc::new(UploadObservations::default());
    let handler_observations = observations.clone();
    let (upstream, _, counter) =
        aioduct_test_server::h3::h3_server_streaming(move |request, mut stream| {
            let observations = handler_observations.clone();
            async move {
                if request.method() == http::Method::GET {
                    stream
                        .send_response(http::Response::builder().status(200).body(()).unwrap())
                        .await
                        .unwrap();
                    stream
                        .send_data(Bytes::from_static(b"after"))
                        .await
                        .unwrap();
                    stream.finish().await.unwrap();
                    return;
                }

                observations.attempts.fetch_add(1, Ordering::SeqCst);
                let mut received = Vec::new();
                while !received
                    .windows(FILE_BYTES.len())
                    .any(|window| window == FILE_BYTES)
                {
                    let mut data = stream.recv_data().await.unwrap().unwrap();
                    received.extend_from_slice(data.chunk());
                    data.advance(data.remaining());
                }
                stream
                    .send_response(http::Response::builder().status(413).body(()).unwrap())
                    .await
                    .unwrap();
                stream
                    .send_data(Bytes::from_static(b"upload rejected"))
                    .await
                    .unwrap();
                stream.finish().await.unwrap();
                while matches!(stream.recv_data().await, Ok(Some(_))) {}
                observations.quiesced.store(true, Ordering::Release);
            }
        })
        .await;
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .tls(aioduct::tls::RustlsConnector::danger_accept_invalid_certs())
        .http3(true)
        .unwrap()
        .build()
        .unwrap();
    let broker = start_broker(
        client,
        format!("https://127.0.0.1:{}", upstream.port())
            .parse()
            .unwrap(),
        UpstreamProtocol::Automatic,
    )
    .await;

    exercise(broker, http::Version::HTTP_11, &observations, &counter, 1).await;
}
