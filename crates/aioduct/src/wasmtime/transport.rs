use std::future::Future;
use std::pin::Pin;
#[cfg(feature = "compio")]
use std::sync::mpsc as std_mpsc;
#[cfg(feature = "compio")]
use std::task::{Context, Poll};
use std::time::Duration;

#[cfg(feature = "compio")]
use bytes::Bytes;
#[cfg(feature = "compio")]
use futures_channel::{mpsc, oneshot};
#[cfg(feature = "compio")]
use futures_util::{SinkExt, StreamExt};
use http::Uri;
#[cfg(feature = "compio")]
use http_body::{Body, Frame};
use http_body_util::BodyExt;
#[cfg(feature = "compio")]
use pin_project_lite::pin_project;

#[cfg(feature = "compio")]
use super::BuildError;
use super::sealed;

/// Tokio transport builder accepted by
/// [`crate::wasmtime::WasiHttpHostBuilder::transport_builder`].
#[cfg(feature = "tokio")]
pub type TokioTransportBuilder = crate::client::HttpEngineBuilder<
    crate::runtime::tokio_rt::TokioRuntime,
    crate::runtime::tokio_rt::TcpConnector,
>;

/// Smol transport builder for constructing a transport accepted by
/// [`crate::wasmtime::WasiHttpHostBuilder::transport`].
#[cfg(feature = "smol")]
pub type SmolTransportBuilder = crate::client::HttpEngineBuilder<
    crate::runtime::smol_rt::SmolRuntime,
    crate::runtime::smol_rt::TcpConnector,
>;

/// Compio transport builder accepted by [`crate::wasmtime::CompioHostTransport`].
#[cfg(feature = "compio")]
pub type CompioTransportBuilder = crate::client::HttpEngineBuilder<
    crate::runtime::compio_rt::CompioRuntime,
    crate::runtime::compio_rt::TcpConnector,
>;

/// Boxed future returned by host transports.
#[doc(hidden)]
pub type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send + 'static>>;

/// Native response returned by host transports.
#[doc(hidden)]
pub struct HostResponse {
    pub(crate) response: http::Response<crate::body::RequestBodySend>,
    pub(crate) worker: Option<::wasmtime_wasi::runtime::AbortOnDropJoinHandle<()>>,
}

impl HostResponse {
    pub(crate) fn new(response: http::Response<crate::body::RequestBodySend>) -> Self {
        Self {
            response,
            worker: None,
        }
    }

    #[cfg(feature = "compio")]
    pub(crate) fn with_worker(
        mut self,
        worker: ::wasmtime_wasi::runtime::AbortOnDropJoinHandle<()>,
    ) -> Self {
        self.worker = Some(worker);
        self
    }
}

/// Forwarding options passed from the Wasmtime host hook to the native transport.
#[doc(hidden)]
#[derive(Clone)]
pub struct HostForwardOptions {
    pub(crate) upstream: Uri,
    pub(crate) timeout: Option<Duration>,
    pub(crate) connect_timeout: Duration,
    pub(crate) first_byte_timeout: Duration,
    pub(crate) write_timeout: Option<Duration>,
    pub(crate) read_timeout: Duration,
}

/// Sealed native transport used by [`crate::wasmtime::WasiHttpHost`] to service WASI HTTP calls.
///
/// This trait is public so the host type can name its transport boundary, but
/// it is sealed because aioduct still owns the compatibility contract for the
/// bridge. Built-in implementations cover `HttpEngineSend<R, C>` for
/// `RuntimePoll` runtimes such as Tokio and smol, plus `CompioHostTransport`
/// when the `compio` feature is enabled.
pub trait WasiHostTransport: sealed::Sealed + Send + Sync + 'static {
    /// Forward a validated WASI HTTP request through native aioduct.
    #[doc(hidden)]
    fn forward_wasi_http(
        &self,
        request: http::Request<crate::body::RequestBodySend>,
        options: HostForwardOptions,
    ) -> BoxFuture<Result<HostResponse, crate::Error>>;
}

impl<R, C> WasiHostTransport for crate::HttpEngineSend<R, C>
where
    R: crate::RuntimePoll,
    C: crate::ConnectorSend,
{
    fn forward_wasi_http(
        &self,
        request: http::Request<crate::body::RequestBodySend>,
        options: HostForwardOptions,
    ) -> BoxFuture<Result<HostResponse, crate::Error>> {
        let transport = self.clone();
        Box::pin(async move {
            let mut forward = transport
                .forward(request)
                .upstream(options.upstream)
                .without_message_signature()
                .connect_timeout(options.connect_timeout)
                .first_byte_timeout(options.first_byte_timeout)
                .read_timeout(options.read_timeout);

            if let Some(timeout) = options.timeout {
                forward = forward.timeout(timeout);
            }
            if let Some(write_timeout) = options.write_timeout {
                forward = forward.write_timeout(write_timeout);
            }

            let response = forward.send().await?;
            let (parts, body) = response.into_http_response().into_parts();
            Ok(HostResponse::new(http::Response::from_parts(
                parts,
                body.boxed_unsync(),
            )))
        })
    }
}

#[cfg(feature = "compio")]
const LOCAL_WORKER_QUEUE: usize = 64;
#[cfg(feature = "compio")]
const BODY_CHANNEL_CAPACITY: usize = 16;

#[cfg(feature = "compio")]
type BodyFrame = Result<Frame<Bytes>, crate::Error>;
#[cfg(feature = "compio")]
type BodyFrameSender = mpsc::Sender<BodyFrame>;
#[cfg(feature = "compio")]
type BodyFrameReceiver = mpsc::Receiver<BodyFrame>;

/// Host transport wrapper for compio's thread-local native runtime.
///
/// `CompioClient` uses `HttpEngineLocal`, so it cannot implement
/// [`crate::wasmtime::WasiHostTransport`] directly. This wrapper owns a dedicated compio worker
/// thread and moves request and response body frames across bounded channels.
#[cfg(feature = "compio")]
pub struct CompioHostTransport {
    requests: std::sync::Mutex<mpsc::Sender<LocalForwardRequest>>,
}

#[cfg(feature = "compio")]
impl CompioHostTransport {
    /// Start a host transport worker from a factory that creates the compio
    /// transport builder on the worker thread.
    pub fn from_builder_factory(
        transport: impl FnOnce() -> CompioTransportBuilder + Send + 'static,
    ) -> Result<Self, BuildError> {
        let (sender, receiver) = mpsc::channel(LOCAL_WORKER_QUEUE);
        spawn_compio_worker(transport, receiver)?;
        Ok(Self {
            requests: std::sync::Mutex::new(sender),
        })
    }

    /// Start a host transport worker with the default compio transport.
    pub fn new() -> Result<Self, BuildError> {
        Self::from_builder_factory(crate::CompioClient::builder)
    }
}

#[cfg(feature = "compio")]
impl WasiHostTransport for CompioHostTransport {
    fn forward_wasi_http(
        &self,
        request: http::Request<crate::body::RequestBodySend>,
        options: HostForwardOptions,
    ) -> BoxFuture<Result<HostResponse, crate::Error>> {
        let request_sender = match self.requests.lock() {
            Ok(sender) => sender.clone(),
            Err(_) => {
                return Box::pin(async { Err(local_worker_closed_error()) });
            }
        };

        Box::pin(async move {
            let (parts, body) = request.into_parts();
            let body_is_end_stream = body.is_end_stream();
            let (body_sender, body_receiver) = mpsc::channel(BODY_CHANNEL_CAPACITY);
            let body_pump = spawn_send_body_pump(body, body_sender);
            let request = http::Request::from_parts(
                parts,
                ChannelBody::new(body_receiver, body_is_end_stream),
            );
            let (response_sender, response_receiver) = oneshot::channel();

            let mut request_sender = request_sender;
            request_sender
                .send(LocalForwardRequest {
                    request,
                    options,
                    response_sender,
                })
                .await
                .map_err(|_| local_worker_closed_error())?;
            let response = response_receiver
                .await
                .map_err(|_| local_worker_closed_error())?;
            match response {
                Ok(response) => {
                    // A full-duplex peer can return headers before upload drain.
                    // Keep pumping until the response body is consumed or dropped.
                    Ok(response.with_worker(body_pump))
                }
                Err(error) => {
                    drop(body_pump);
                    Err(error)
                }
            }
        })
    }
}

#[cfg(feature = "compio")]
struct LocalForwardRequest {
    request: http::Request<ChannelBody>,
    options: HostForwardOptions,
    response_sender: oneshot::Sender<Result<HostResponse, crate::Error>>,
}

#[cfg(feature = "compio")]
fn spawn_compio_worker(
    transport: impl FnOnce() -> CompioTransportBuilder + Send + 'static,
    mut receiver: mpsc::Receiver<LocalForwardRequest>,
) -> Result<(), BuildError> {
    let (ready_sender, ready_receiver) = std_mpsc::sync_channel(1);
    std::thread::Builder::new()
        .name("aioduct-wasi-host-compio".into())
        .spawn(move || {
            let transport = transport();
            let ready_sender_for_task = ready_sender.clone();
            let result = <crate::runtime::compio_rt::CompioRuntime as crate::RuntimeCompletion>::block_on(async move {
                let transport = match transport.build_local() {
                    Ok(transport) => transport,
                    Err(error) => {
                        let _ = ready_sender_for_task.send(Err(error));
                        return;
                    }
                };
                let _ = ready_sender_for_task.send(Ok(()));
                while let Some(request) = receiver.next().await {
                    let transport = transport.clone();
                    <crate::runtime::compio_rt::CompioRuntime as crate::RuntimeLocal>::spawn_local(
                        async move {
                            let response =
                                forward_compio_request(transport, request.request, request.options)
                                    .await;
                            let _ = request.response_sender.send(response);
                        },
                    );
                }
            });
            if let Err(error) = result {
                let _ = ready_sender.send(Err(error));
            }
        })
        .map_err(BuildError::WorkerThread)?;
    ready_receiver
        .recv()
        .map_err(|_| BuildError::WorkerStartup)??;
    Ok(())
}

#[cfg(feature = "compio")]
async fn forward_compio_request(
    transport: crate::CompioClient,
    request: http::Request<ChannelBody>,
    options: HostForwardOptions,
) -> Result<HostResponse, crate::Error> {
    let mut forward = transport
        .forward_local(request)
        .upstream(options.upstream)
        .without_message_signature()
        .connect_timeout(options.connect_timeout)
        .first_byte_timeout(options.first_byte_timeout)
        .read_timeout(options.read_timeout);

    if let Some(timeout) = options.timeout {
        forward = forward.timeout(timeout);
    }
    if let Some(write_timeout) = options.write_timeout {
        forward = forward.write_timeout(write_timeout);
    }

    let response = forward.send().await?;
    let (parts, body) = response.into_http_response().into_parts();
    let (body_sender, body_receiver) = mpsc::channel(BODY_CHANNEL_CAPACITY);
    <crate::runtime::compio_rt::CompioRuntime as crate::RuntimeLocal>::spawn_local(
        pump_local_response_body(body, body_sender),
    );
    Ok(HostResponse::new(http::Response::from_parts(
        parts,
        ChannelBody::new(body_receiver, false).boxed_unsync(),
    )))
}

#[cfg(feature = "compio")]
fn spawn_send_body_pump(
    body: crate::body::RequestBodySend,
    sender: BodyFrameSender,
) -> ::wasmtime_wasi::runtime::AbortOnDropJoinHandle<()> {
    ::wasmtime_wasi::runtime::spawn(async move {
        pump_send_body(body, sender).await;
    })
}

#[cfg(feature = "compio")]
async fn pump_send_body(mut body: crate::body::RequestBodySend, mut sender: BodyFrameSender) {
    while let Some(frame) = body.frame().await {
        let should_stop = frame.is_err();
        if sender.send(frame).await.is_err() || should_stop {
            break;
        }
    }
}

#[cfg(feature = "compio")]
async fn pump_local_response_body(
    mut body: crate::body::ResponseBodyLocal,
    mut sender: BodyFrameSender,
) {
    while let Some(frame) = std::future::poll_fn(|cx| body.as_mut().poll_frame(cx)).await {
        let should_stop = frame.is_err();
        if sender.send(frame).await.is_err() || should_stop {
            break;
        }
    }
}

#[cfg(feature = "compio")]
pin_project! {
    struct ChannelBody {
        #[pin]
        receiver: BodyFrameReceiver,
        end_stream: bool,
    }
}

#[cfg(feature = "compio")]
impl ChannelBody {
    fn new(receiver: BodyFrameReceiver, end_stream: bool) -> Self {
        Self {
            receiver,
            end_stream,
        }
    }
}

#[cfg(feature = "compio")]
impl Body for ChannelBody {
    type Data = Bytes;
    type Error = crate::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        let this = self.project();
        match futures_core::Stream::poll_next(this.receiver, cx) {
            Poll::Ready(None) => {
                *this.end_stream = true;
                Poll::Ready(None)
            }
            other => other,
        }
    }

    fn is_end_stream(&self) -> bool {
        self.end_stream
    }
}

#[cfg(feature = "compio")]
fn local_worker_closed_error() -> crate::Error {
    crate::Error::Other("WASI HTTP local transport worker closed".into())
}
