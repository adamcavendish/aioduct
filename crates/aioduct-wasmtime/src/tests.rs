use super::*;
use bytes::Bytes;
use http::header::{AUTHORIZATION, FORWARDED, HeaderName};
use http::{HeaderMap, HeaderValue};
use http_body::{Body, Frame};
use http_body_util::{BodyExt, Empty, Full, StreamBody};
use std::marker::PhantomData;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::Mutex;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use wasmtime_wasi::p2::{InputStream, Pollable, StreamError};
use wasmtime_wasi_http::p2::WasiHttpHooks;
use wasmtime_wasi_http::p2::bindings::http::types::ErrorCode;
use wasmtime_wasi_http::p2::body::HostIncomingBody;
use wasmtime_wasi_http::p2::body::{HyperIncomingBody, HyperOutgoingBody};
use wasmtime_wasi_http::p2::types::{IncomingResponse, OutgoingRequestConfig};

fn config(use_tls: bool) -> OutgoingRequestConfig {
    OutgoingRequestConfig {
        use_tls,
        connect_timeout: Duration::from_secs(5),
        first_byte_timeout: Duration::from_secs(5),
        between_bytes_timeout: Duration::from_secs(5),
    }
}

fn empty_body() -> HyperOutgoingBody {
    Empty::<Bytes>::new()
        .map_err(|never| match never {})
        .boxed_unsync()
}

fn full_body(bytes: &'static [u8]) -> HyperOutgoingBody {
    Full::new(Bytes::from_static(bytes))
        .map_err(|never| match never {})
        .boxed_unsync()
}

fn request_trailers_body(headers: HeaderMap) -> HyperOutgoingBody {
    StreamBody::new(futures_util::stream::once(async move {
        Ok::<Frame<Bytes>, ErrorCode>(Frame::trailers(headers))
    }))
    .boxed_unsync()
}

fn native_trailers_body(headers: HeaderMap) -> aioduct::body::RequestBodySend {
    StreamBody::new(futures_util::stream::once(async move {
        Ok::<Frame<Bytes>, aioduct::Error>(Frame::trailers(headers))
    }))
    .boxed_unsync()
}

fn failing_body(code: ErrorCode) -> HyperOutgoingBody {
    StreamBody::new(futures_util::stream::once(async move {
        Err::<Frame<Bytes>, ErrorCode>(code)
    }))
    .boxed_unsync()
}

fn pending_body() -> HyperOutgoingBody {
    PendingBody::<ErrorCode>(PhantomData).boxed_unsync()
}

fn pending_incoming_body() -> HyperIncomingBody {
    PendingBody::<ErrorCode>(PhantomData).boxed_unsync()
}

fn pending_native_body() -> aioduct::body::RequestBodySend {
    PendingBody::<aioduct::Error>(PhantomData).boxed_unsync()
}

struct PendingBody<E>(PhantomData<fn() -> E>);

impl<E> Body for PendingBody<E> {
    type Data = Bytes;
    type Error = E;

    fn poll_frame(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        Poll::Pending
    }

    fn is_end_stream(&self) -> bool {
        false
    }
}

#[derive(Clone, Copy)]
struct PendingResponseTransport;

impl sealed::Sealed for PendingResponseTransport {}

impl WasiHostTransport for PendingResponseTransport {
    fn forward_wasi_http(
        &self,
        _request: http::Request<aioduct::body::RequestBodySend>,
        _options: HostForwardOptions,
    ) -> BoxFuture<Result<HostResponse, aioduct::Error>> {
        Box::pin(async {
            Ok(HostResponse::new(
                http::Response::builder()
                    .status(http::StatusCode::OK)
                    .body(pending_native_body())
                    .expect("response should build"),
            ))
        })
    }
}

#[derive(Clone, Copy)]
struct CollectingTransport;

impl sealed::Sealed for CollectingTransport {}

impl WasiHostTransport for CollectingTransport {
    fn forward_wasi_http(
        &self,
        request: http::Request<aioduct::body::RequestBodySend>,
        _options: HostForwardOptions,
    ) -> BoxFuture<Result<HostResponse, aioduct::Error>> {
        Box::pin(async move {
            request.into_body().collect().await?;
            Ok(HostResponse::new(
                http::Response::builder()
                    .status(http::StatusCode::OK)
                    .body(
                        Empty::<Bytes>::new()
                            .map_err(|never| match never {})
                            .boxed_unsync(),
                    )
                    .expect("response should build"),
            ))
        })
    }
}

#[derive(Clone, Copy)]
struct PanickingTransport;

impl sealed::Sealed for PanickingTransport {}

impl WasiHostTransport for PanickingTransport {
    fn forward_wasi_http(
        &self,
        _request: http::Request<aioduct::body::RequestBodySend>,
        _options: HostForwardOptions,
    ) -> BoxFuture<Result<HostResponse, aioduct::Error>> {
        panic!("denied request must not reach transport")
    }
}

#[derive(Clone)]
struct TrailerResponseTransport {
    trailers: HeaderMap,
}

impl sealed::Sealed for TrailerResponseTransport {}

impl WasiHostTransport for TrailerResponseTransport {
    fn forward_wasi_http(
        &self,
        _request: http::Request<aioduct::body::RequestBodySend>,
        _options: HostForwardOptions,
    ) -> BoxFuture<Result<HostResponse, aioduct::Error>> {
        let trailers = self.trailers.clone();
        Box::pin(async move {
            Ok(HostResponse::new(
                http::Response::builder()
                    .status(http::StatusCode::OK)
                    .body(native_trailers_body(trailers))
                    .expect("response should build"),
            ))
        })
    }
}

fn request(uri: String) -> hyper::Request<HyperOutgoingBody> {
    hyper::Request::builder()
        .uri(uri)
        .body(empty_body())
        .expect("request should build")
}

async fn raw_server(
    response: &'static [u8],
) -> (std::net::SocketAddr, tokio::sync::oneshot::Receiver<String>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("listener should bind");
    let addr = listener.local_addr().expect("listener should have address");
    let (tx, rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let Ok((mut stream, _)) = listener.accept().await else {
            return;
        };
        let mut buf = vec![0_u8; 4096];
        let n = match stream.read(&mut buf).await {
            Ok(n) => n,
            Err(_) => return,
        };
        let text = String::from_utf8_lossy(&buf[..n]).into_owned();
        let _ = tx.send(text);
        let _ = stream.write_all(response).await;
    });
    (addr, rx)
}

fn test_host(policy: ExactOriginPolicy) -> WasiHttpHost {
    let builder = WasiHttpHost::builder().policy(policy);
    #[cfg(feature = "tokio")]
    {
        builder.build().expect("host should build")
    }
    #[cfg(all(not(feature = "tokio"), feature = "smol"))]
    {
        let transport = aioduct::SmolClient::builder()
            .build()
            .expect("smol transport should build");
        builder
            .transport(transport)
            .build()
            .expect("host should build")
    }
    #[cfg(all(not(feature = "tokio"), not(feature = "smol"), feature = "compio"))]
    {
        let transport = CompioHostTransport::new().expect("compio host transport should start");
        builder
            .transport(transport)
            .build()
            .expect("host should build")
    }
    #[cfg(all(not(feature = "tokio"), not(feature = "smol"), not(feature = "compio")))]
    {
        panic!("tests require a tokio, smol, or compio transport feature")
    }
}

fn record_rejections(
    policy: ExactOriginPolicy,
) -> (ExactOriginPolicy, Arc<Mutex<Vec<RejectionReason>>>) {
    let reasons = Arc::new(Mutex::new(Vec::new()));
    let observed = reasons.clone();
    let policy = policy.on_rejection(move |reason| {
        observed.lock().expect("observer lock").push(reason);
    });
    (policy, reasons)
}

mod body;
mod limits;
mod policy;
mod transport;
