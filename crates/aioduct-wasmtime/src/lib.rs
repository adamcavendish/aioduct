//! Host-side Wasmtime WASI HTTP adapter backed by native `aioduct`.
//!
//! This crate is for hosts embedding WASI Preview 2 components with
//! `wasi:http`. Guests keep using `aioduct::WasiClient`; the host installs
//! [`WasiHttpHost`] as the Wasmtime HTTP hook and owns transport trust policy.
//!
//! The default feature set is empty. Enable exactly the native host transport
//! runtime you want to use, such as `tokio`, `smol`, or `compio`, plus a rustls
//! provider when the native transport needs TLS.
//!
//! Runnable host examples live under `examples/wasmtime-host` in the workspace.
//! They show a WASI guest component using `aioduct::WasiClient` while the host
//! validates origin policy, injects a host-owned header, and forwards through
//! Tokio, smol, or compio native transports.

#![deny(missing_docs)]

use std::error::Error as StdError;
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
#[cfg(feature = "compio")]
use std::sync::mpsc as std_mpsc;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use bytes::Bytes;
#[cfg(feature = "compio")]
use futures_channel::{mpsc, oneshot};
#[cfg(feature = "compio")]
use futures_util::{SinkExt, StreamExt};
use http::header::{
    AUTHORIZATION, CONTENT_LENGTH, COOKIE, HeaderName, HeaderValue, PROXY_AUTHORIZATION,
};
use http::{HeaderMap, Uri};
use http_body::{Body, Frame};
use http_body_util::BodyExt;
use pin_project_lite::pin_project;
use wasmtime_wasi_http::DEFAULT_FORBIDDEN_HEADERS;
use wasmtime_wasi_http::p2::bindings::http::types::ErrorCode;
use wasmtime_wasi_http::p2::body::{HyperIncomingBody, HyperOutgoingBody};
use wasmtime_wasi_http::p2::types::{
    HostFutureIncomingResponse, IncomingResponse, OutgoingRequestConfig,
};
use wasmtime_wasi_http::p2::{HttpResult, WasiHttpHooks};

/// Tokio transport builder accepted by [`WasiHttpHostBuilder::transport_builder`].
#[cfg(feature = "tokio")]
pub type TokioTransportBuilder = aioduct::client::HttpEngineBuilder<
    aioduct::runtime::tokio_rt::TokioRuntime,
    aioduct::runtime::tokio_rt::TcpConnector,
>;

/// Smol transport builder for constructing a transport accepted by
/// [`WasiHttpHostBuilder::transport`].
#[cfg(feature = "smol")]
pub type SmolTransportBuilder = aioduct::client::HttpEngineBuilder<
    aioduct::runtime::smol_rt::SmolRuntime,
    aioduct::runtime::smol_rt::TcpConnector,
>;

/// Compio transport builder accepted by [`CompioHostTransport`].
#[cfg(feature = "compio")]
pub type CompioTransportBuilder = aioduct::client::HttpEngineBuilder<
    aioduct::runtime::compio_rt::CompioRuntime,
    aioduct::runtime::compio_rt::TcpConnector,
>;

/// Boxed future returned by host transports.
#[doc(hidden)]
pub type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send + 'static>>;

type RejectionObserver = Arc<dyn Fn(RejectionReason) + Send + Sync>;

/// Native response returned by host transports.
#[doc(hidden)]
pub struct HostResponse {
    response: http::Response<aioduct::body::RequestBodySend>,
    worker: Option<wasmtime_wasi::runtime::AbortOnDropJoinHandle<()>>,
}

impl HostResponse {
    fn new(response: http::Response<aioduct::body::RequestBodySend>) -> Self {
        Self {
            response,
            worker: None,
        }
    }

    #[cfg(feature = "compio")]
    fn with_worker(mut self, worker: wasmtime_wasi::runtime::AbortOnDropJoinHandle<()>) -> Self {
        self.worker = Some(worker);
        self
    }
}

/// Forwarding options passed from the Wasmtime host hook to the native transport.
#[doc(hidden)]
#[derive(Clone)]
pub struct HostForwardOptions {
    upstream: Uri,
    timeout: Option<Duration>,
    connect_timeout: Duration,
    first_byte_timeout: Duration,
    write_timeout: Option<Duration>,
    read_timeout: Duration,
}

/// Sealed native transport used by [`WasiHttpHost`] to service WASI HTTP calls.
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
        request: http::Request<aioduct::body::RequestBodySend>,
        options: HostForwardOptions,
    ) -> BoxFuture<Result<HostResponse, aioduct::Error>>;
}

impl<R, C> WasiHostTransport for aioduct::HttpEngineSend<R, C>
where
    R: aioduct::RuntimePoll,
    C: aioduct::ConnectorSend,
{
    fn forward_wasi_http(
        &self,
        request: http::Request<aioduct::body::RequestBodySend>,
        options: HostForwardOptions,
    ) -> BoxFuture<Result<HostResponse, aioduct::Error>> {
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
type BodyFrame = Result<Frame<Bytes>, aioduct::Error>;
#[cfg(feature = "compio")]
type BodyFrameSender = mpsc::Sender<BodyFrame>;
#[cfg(feature = "compio")]
type BodyFrameReceiver = mpsc::Receiver<BodyFrame>;

/// Host transport wrapper for compio's thread-local native runtime.
///
/// `CompioClient` uses `HttpEngineLocal`, so it cannot implement
/// [`WasiHostTransport`] directly. This wrapper owns a dedicated compio worker
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
        Self::from_builder_factory(aioduct::CompioClient::builder)
    }
}

#[cfg(feature = "compio")]
impl WasiHostTransport for CompioHostTransport {
    fn forward_wasi_http(
        &self,
        request: http::Request<aioduct::body::RequestBodySend>,
        options: HostForwardOptions,
    ) -> BoxFuture<Result<HostResponse, aioduct::Error>> {
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
    response_sender: oneshot::Sender<Result<HostResponse, aioduct::Error>>,
}

#[cfg(feature = "compio")]
fn spawn_compio_worker(
    transport: impl FnOnce() -> CompioTransportBuilder + Send + 'static,
    mut receiver: mpsc::Receiver<LocalForwardRequest>,
) -> Result<(), BuildError> {
    let (ready_sender, ready_receiver) = std_mpsc::sync_channel(1);
    std::thread::Builder::new()
        .name("aioduct-wasmtime-compio".into())
        .spawn(move || {
            let transport = transport();
            let ready_sender_for_task = ready_sender.clone();
            let result = <aioduct::runtime::compio_rt::CompioRuntime as aioduct::RuntimeCompletion>::block_on(async move {
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
                    <aioduct::runtime::compio_rt::CompioRuntime as aioduct::RuntimeLocal>::spawn_local(
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
    transport: aioduct::CompioClient,
    request: http::Request<ChannelBody>,
    options: HostForwardOptions,
) -> Result<HostResponse, aioduct::Error> {
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
    <aioduct::runtime::compio_rt::CompioRuntime as aioduct::RuntimeLocal>::spawn_local(
        pump_local_response_body(body, body_sender),
    );
    Ok(HostResponse::new(http::Response::from_parts(
        parts,
        ChannelBody::new(body_receiver, false).boxed_unsync(),
    )))
}

#[cfg(feature = "compio")]
fn spawn_send_body_pump(
    body: aioduct::body::RequestBodySend,
    sender: BodyFrameSender,
) -> wasmtime_wasi::runtime::AbortOnDropJoinHandle<()> {
    wasmtime_wasi::runtime::spawn(async move {
        pump_send_body(body, sender).await;
    })
}

#[cfg(feature = "compio")]
async fn pump_send_body(mut body: aioduct::body::RequestBodySend, mut sender: BodyFrameSender) {
    while let Some(frame) = body.frame().await {
        let should_stop = frame.is_err();
        if sender.send(frame).await.is_err() || should_stop {
            break;
        }
    }
}

#[cfg(feature = "compio")]
async fn pump_local_response_body(
    mut body: aioduct::body::ResponseBodyLocal,
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
    type Error = aioduct::Error;

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
fn local_worker_closed_error() -> aioduct::Error {
    aioduct::Error::Other("WASI HTTP local transport worker closed".into())
}

#[doc(hidden)]
pub mod sealed {
    /// Marker trait sealing [`WasiHostTransport`](super::WasiHostTransport).
    pub trait Sealed {}

    impl<R, C> Sealed for aioduct::HttpEngineSend<R, C>
    where
        R: aioduct::RuntimePoll,
        C: aioduct::ConnectorSend,
    {
    }

    #[cfg(feature = "compio")]
    impl Sealed for super::CompioHostTransport {}
}

/// Host-side implementation of Wasmtime's WASI HTTP hooks.
#[derive(Clone)]
pub struct WasiHttpHost {
    transport: Arc<dyn WasiHostTransport>,
    policy: ExactOriginPolicy,
}

impl WasiHttpHost {
    /// Create a new host hook builder.
    pub fn builder() -> WasiHttpHostBuilder {
        WasiHttpHostBuilder::default()
    }

    async fn send_inner(
        self,
        request: hyper::Request<HyperOutgoingBody>,
        config: OutgoingRequestConfig,
    ) -> Result<IncomingResponse, ErrorCode> {
        let native_request = self.prepare_request(request, config.use_tls)?;
        let connect_timeout = self.policy.cap_with_deadline(config.connect_timeout)?;
        let first_byte_timeout = self.policy.cap_with_deadline(config.first_byte_timeout)?;
        let read_timeout = self
            .policy
            .cap_with_deadline(config.between_bytes_timeout)?;

        let total_timeout = self.policy.deadline_remaining()?;
        let options = HostForwardOptions {
            upstream: self.policy.origin_uri.clone(),
            timeout: total_timeout,
            connect_timeout,
            first_byte_timeout,
            write_timeout: total_timeout,
            read_timeout,
        };

        let response = self
            .transport
            .forward_wasi_http(native_request, options)
            .await
            .map_err(|error| self.policy.map_forward_error(error))?;
        self.policy
            .check_response_header_limit(response.response.headers())?;

        let worker = response.worker;
        let (parts, body) = response.response.into_parts();
        let mut body: HyperIncomingBody = body.map_err(map_aioduct_error).boxed_unsync();

        if self.policy.body_limit.is_some() || self.policy.header_limit.is_some() {
            body = ResponseLimitBody::new_policy(
                body,
                self.policy.body_limit,
                self.policy.header_limit,
                self.policy.rejection_observer.clone(),
            )
            .boxed_unsync();
        }
        if let Some(deadline) = self.policy.deadline {
            body = DeadlineBody::new(body, deadline, self.policy.rejection_observer.clone())
                .boxed_unsync();
        }

        Ok(IncomingResponse {
            resp: hyper::Response::from_parts(parts, body),
            worker,
            between_bytes_timeout: config.between_bytes_timeout,
        })
    }

    fn prepare_request(
        &self,
        request: hyper::Request<HyperOutgoingBody>,
        use_tls: bool,
    ) -> Result<http::Request<aioduct::body::RequestBodySend>, ErrorCode> {
        self.policy.validate_origin(request.uri(), use_tls)?;
        self.policy.check_request_headers(request.headers())?;
        self.policy.check_request_header_limit(request.headers())?;
        self.policy
            .check_request_body_known_limit(request.headers(), request.body())?;

        let (mut parts, body) = request.into_parts();
        for name in self.policy.injected_headers.keys() {
            if parts.headers.contains_key(name) {
                return Err(self.policy.request_denied(RejectionReason::ProtectedHeader));
            }
        }
        for (name, value) in &self.policy.injected_headers {
            parts.headers.insert(name, value.clone());
        }
        self.policy.check_request_header_limit(&parts.headers)?;

        let mut body: aioduct::body::RequestBodySend =
            body.map_err(map_wasi_body_error).boxed_unsync();
        body =
            RequestLimitBody::new_policy(body, self.policy.body_limit, &self.policy).boxed_unsync();

        Ok(http::Request::from_parts(parts, body))
    }
}

impl WasiHttpHooks for WasiHttpHost {
    fn send_request(
        &mut self,
        request: hyper::Request<HyperOutgoingBody>,
        config: OutgoingRequestConfig,
    ) -> HttpResult<HostFutureIncomingResponse> {
        let host = self.clone();
        let task = wasmtime_wasi::runtime::spawn(async move {
            Ok::<Result<IncomingResponse, ErrorCode>, wasmtime::Error>(
                host.send_inner(request, config).await,
            )
        });
        Ok(HostFutureIncomingResponse::pending(task))
    }

    fn is_forbidden_header(&mut self, name: &HeaderName) -> bool {
        self.policy.is_forbidden_request_header(name)
    }
}

/// Builder for [`WasiHttpHost`].
#[derive(Default)]
pub struct WasiHttpHostBuilder {
    transport: Option<TransportConfig>,
    policy: Option<ExactOriginPolicy>,
}

impl WasiHttpHostBuilder {
    /// Use a built native `aioduct` transport for outbound host requests.
    pub fn transport<T>(mut self, transport: T) -> Self
    where
        T: WasiHostTransport,
    {
        self.transport = Some(TransportConfig::Built(Arc::new(transport)));
        self
    }

    /// Use an `aioduct` Tokio transport builder for outbound host requests.
    #[cfg(feature = "tokio")]
    pub fn transport_builder(mut self, transport: TokioTransportBuilder) -> Self {
        self.transport = Some(TransportConfig::TokioBuilder(Box::new(transport)));
        self
    }

    /// Set the host-owned request policy.
    pub fn policy(mut self, policy: ExactOriginPolicy) -> Self {
        self.policy = Some(policy);
        self
    }

    /// Build the Wasmtime WASI HTTP hook.
    pub fn build(self) -> Result<WasiHttpHost, BuildError> {
        let policy = self.policy.ok_or(BuildError::MissingPolicy)?;
        policy.validate_config()?;
        let transport = match self.transport {
            Some(TransportConfig::Built(transport)) => transport,
            #[cfg(feature = "tokio")]
            Some(TransportConfig::TokioBuilder(builder)) => Arc::new((*builder).build()?),
            None => default_transport()?,
        };
        Ok(WasiHttpHost { transport, policy })
    }
}

enum TransportConfig {
    Built(Arc<dyn WasiHostTransport>),
    #[cfg(feature = "tokio")]
    TokioBuilder(Box<TokioTransportBuilder>),
}

#[cfg(feature = "tokio")]
fn default_transport() -> Result<Arc<dyn WasiHostTransport>, BuildError> {
    Ok(Arc::new(aioduct::TokioClient::builder().build()?))
}

#[cfg(not(feature = "tokio"))]
fn default_transport() -> Result<Arc<dyn WasiHostTransport>, BuildError> {
    Err(BuildError::MissingTransport)
}

/// Exact-origin host policy for WASI HTTP requests.
#[derive(Clone)]
pub struct ExactOriginPolicy {
    origin: Origin,
    origin_uri: Uri,
    forbid_sensitive_headers: bool,
    injected_headers: HeaderMap,
    header_limit: Option<usize>,
    body_limit: Option<u64>,
    deadline: Option<Instant>,
    rejection_observer: Option<RejectionObserver>,
}

impl ExactOriginPolicy {
    /// Create a policy for one allowed origin, for example `https://api.local:8443`.
    pub fn new(origin: &str) -> Result<Self, PolicyError> {
        let uri: Uri = origin
            .parse()
            .map_err(|error| PolicyError::InvalidOrigin(format!("{error}")))?;
        if let Some(path_and_query) = uri.path_and_query()
            && path_and_query.as_str() != "/"
        {
            return Err(PolicyError::OriginMustNotContainPath);
        }
        let parsed = Origin::from_uri(&uri)?;
        let origin_uri = origin_uri(&uri)?;
        Ok(Self {
            origin: parsed,
            origin_uri,
            forbid_sensitive_headers: false,
            injected_headers: HeaderMap::new(),
            header_limit: None,
            body_limit: None,
            deadline: None,
            rejection_observer: None,
        })
    }

    /// Forbid guest-supplied sensitive headers such as `authorization` and `cookie`.
    pub fn forbid_sensitive_headers(mut self) -> Self {
        self.forbid_sensitive_headers = true;
        self
    }

    /// Inject a host-owned header after request validation.
    pub fn inject_header(mut self, name: HeaderName, value: HeaderValue) -> Self {
        self.injected_headers.insert(name, value);
        self
    }

    /// Set the maximum request and response header section size in bytes.
    pub fn header_limit(mut self, limit: usize) -> Self {
        self.header_limit = Some(limit);
        self
    }

    /// Set the maximum request and response body size in bytes.
    pub fn body_limit(mut self, limit: u64) -> Self {
        self.body_limit = Some(limit);
        self
    }

    /// Set an absolute host-side deadline for the whole HTTP exchange.
    pub fn deadline(mut self, deadline: Instant) -> Self {
        self.deadline = Some(deadline);
        self
    }

    /// Observe redacted, low-cardinality host-side rejection reasons.
    pub fn on_rejection(
        mut self,
        observer: impl Fn(RejectionReason) + Send + Sync + 'static,
    ) -> Self {
        self.rejection_observer = Some(Arc::new(observer));
        self
    }

    fn validate_config(&self) -> Result<(), PolicyError> {
        for name in self.injected_headers.keys() {
            if DEFAULT_FORBIDDEN_HEADERS.contains(name) {
                return Err(PolicyError::InjectedForbiddenHeader(name.clone()));
            }
        }
        Ok(())
    }

    fn validate_origin(&self, uri: &Uri, use_tls: bool) -> Result<(), ErrorCode> {
        let request_origin = Origin::from_uri(uri).map_err(|_| ErrorCode::HttpRequestUriInvalid)?;
        let scheme_is_tls = request_origin.scheme == "https";
        if scheme_is_tls != use_tls {
            return Err(ErrorCode::HttpRequestUriInvalid);
        }
        if request_origin != self.origin {
            return Err(self.request_denied(RejectionReason::OriginMismatch));
        }
        Ok(())
    }

    fn check_request_headers(&self, headers: &HeaderMap) -> Result<(), ErrorCode> {
        for (name, value) in headers {
            if self.is_forbidden_request_header(name)
                || (self.forbid_sensitive_headers && value.is_sensitive())
            {
                return Err(self.request_denied(RejectionReason::ProtectedHeader));
            }
        }
        Ok(())
    }

    fn check_request_header_limit(&self, headers: &HeaderMap) -> Result<(), ErrorCode> {
        let Some(limit) = self.header_limit else {
            return Ok(());
        };
        if header_section_size(headers) > limit {
            self.notify_rejection(RejectionReason::HeaderLimit);
            return Err(ErrorCode::HttpRequestHeaderSectionSize(Some(limit_to_u32(
                limit,
            ))));
        }
        Ok(())
    }

    fn check_request_body_known_limit(
        &self,
        headers: &HeaderMap,
        body: &HyperOutgoingBody,
    ) -> Result<(), ErrorCode> {
        let Some(limit) = self.body_limit else {
            return Ok(());
        };
        if let Some(content_length) = headers.get(CONTENT_LENGTH)
            && let Ok(value) = content_length.to_str()
            && let Ok(length) = value.parse::<u64>()
            && length > limit
        {
            self.notify_rejection(RejectionReason::BodyLimit);
            return Err(ErrorCode::HttpRequestBodySize(Some(limit)));
        }

        let hint = body.size_hint();
        if hint.lower() > limit
            || hint
                .upper()
                .is_some_and(|upper| upper == hint.lower() && upper > limit)
        {
            self.notify_rejection(RejectionReason::BodyLimit);
            return Err(ErrorCode::HttpRequestBodySize(Some(limit)));
        }

        Ok(())
    }

    fn check_response_header_limit(&self, headers: &HeaderMap) -> Result<(), ErrorCode> {
        let Some(limit) = self.header_limit else {
            return Ok(());
        };
        if header_section_size(headers) > limit {
            self.notify_rejection(RejectionReason::HeaderLimit);
            return Err(ErrorCode::HttpResponseHeaderSectionSize(Some(
                limit_to_u32(limit),
            )));
        }
        Ok(())
    }

    fn is_forbidden_request_header(&self, name: &HeaderName) -> bool {
        DEFAULT_FORBIDDEN_HEADERS.contains(name)
            || (self.forbid_sensitive_headers
                && (is_sensitive_header_name(name) || self.injected_headers.contains_key(name)))
    }

    fn request_denied(&self, reason: RejectionReason) -> ErrorCode {
        self.notify_rejection(reason);
        ErrorCode::HttpRequestDenied
    }

    fn notify_rejection(&self, reason: RejectionReason) {
        if let Some(observer) = &self.rejection_observer {
            observer(reason);
        }
    }

    fn deadline_remaining(&self) -> Result<Option<Duration>, ErrorCode> {
        let Some(deadline) = self.deadline else {
            return Ok(None);
        };
        let now = Instant::now();
        if deadline <= now {
            self.notify_rejection(RejectionReason::Deadline);
            return Err(ErrorCode::HttpResponseTimeout);
        }
        Ok(Some(deadline.duration_since(now)))
    }

    fn cap_with_deadline(&self, duration: Duration) -> Result<Duration, ErrorCode> {
        match self.deadline_remaining()? {
            Some(remaining) => Ok(duration.min(remaining)),
            None => Ok(duration),
        }
    }

    fn map_forward_error(&self, error: aioduct::Error) -> ErrorCode {
        if self.deadline_expired() && timeout_code_from_aioduct_error(&error).is_some() {
            self.notify_rejection(RejectionReason::Deadline);
        }
        map_aioduct_error(error)
    }

    fn deadline_expired(&self) -> bool {
        match self.deadline {
            Some(deadline) => Instant::now() >= deadline,
            None => false,
        }
    }
}

/// Build errors for [`WasiHttpHost`].
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum BuildError {
    /// No policy was configured.
    #[error("missing WASI HTTP host policy")]
    MissingPolicy,

    /// No default transport is available for the enabled feature set.
    #[error("missing WASI HTTP host transport")]
    MissingTransport,

    /// The configured policy is invalid.
    #[error(transparent)]
    Policy(#[from] PolicyError),

    /// The native transport could not be built.
    #[error(transparent)]
    Transport(#[from] aioduct::Error),

    /// The local-runtime host transport worker could not be started.
    #[cfg(feature = "compio")]
    #[error("failed to start WASI HTTP local transport worker")]
    WorkerThread(#[source] std::io::Error),

    /// The local-runtime host transport worker exited before it was ready.
    #[cfg(feature = "compio")]
    #[error("WASI HTTP local transport worker exited during startup")]
    WorkerStartup,
}

/// Policy construction errors.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PolicyError {
    /// The origin string is not a valid URI.
    #[error("invalid origin: {0}")]
    InvalidOrigin(String),

    /// The origin scheme is not supported.
    #[error("origin scheme must be http or https, got {0}")]
    UnsupportedScheme(String),

    /// The origin is missing an authority.
    #[error("origin must include an authority")]
    MissingAuthority,

    /// Origins must not include path or query components.
    #[error("origin must not include a path or query")]
    OriginMustNotContainPath,

    /// A host-injected header is forbidden by WASI HTTP.
    #[error("injected header is forbidden by WASI HTTP: {0}")]
    InjectedForbiddenHeader(HeaderName),
}

/// Low-cardinality reason for a host-side request rejection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum RejectionReason {
    /// The request URI did not match the allowed origin.
    OriginMismatch,
    /// The request tried to supply a protected or sensitive header.
    ProtectedHeader,
    /// The request or response exceeded the configured header limit.
    HeaderLimit,
    /// The request or response exceeded the configured body limit.
    BodyLimit,
    /// The host deadline expired.
    Deadline,
}

impl RejectionReason {
    /// Return a stable, low-cardinality string for diagnostics and metrics.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::OriginMismatch => "origin_mismatch",
            Self::ProtectedHeader => "protected_header",
            Self::HeaderLimit => "header_limit",
            Self::BodyLimit => "body_limit",
            Self::Deadline => "deadline",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Origin {
    scheme: String,
    host: String,
    port: u16,
}

impl Origin {
    fn from_uri(uri: &Uri) -> Result<Self, PolicyError> {
        let scheme = uri
            .scheme_str()
            .ok_or_else(|| PolicyError::InvalidOrigin("missing scheme".into()))?;
        if !matches!(scheme, "http" | "https") {
            return Err(PolicyError::UnsupportedScheme(scheme.into()));
        }
        let authority = uri.authority().ok_or(PolicyError::MissingAuthority)?;
        let port = authority
            .port_u16()
            .or_else(|| default_port(scheme))
            .ok_or_else(|| PolicyError::InvalidOrigin("missing port".into()))?;
        Ok(Self {
            scheme: scheme.into(),
            host: authority.host().to_ascii_lowercase(),
            port,
        })
    }
}

fn default_port(scheme: &str) -> Option<u16> {
    match scheme {
        "http" => Some(80),
        "https" => Some(443),
        _ => None,
    }
}

fn origin_uri(uri: &Uri) -> Result<Uri, PolicyError> {
    let mut parts = http::uri::Parts::default();
    parts.scheme = uri.scheme().cloned();
    parts.authority = uri.authority().cloned();
    parts.path_and_query = Some(http::uri::PathAndQuery::from_static("/"));
    Uri::from_parts(parts).map_err(|error| PolicyError::InvalidOrigin(format!("{error}")))
}

fn is_sensitive_header_name(name: &HeaderName) -> bool {
    name == AUTHORIZATION || name == COOKIE || name == PROXY_AUTHORIZATION
}

fn header_section_size(headers: &HeaderMap) -> usize {
    headers
        .iter()
        .map(|(name, value)| name.as_str().len() + value.as_bytes().len())
        .sum()
}

fn limit_to_u32(limit: usize) -> u32 {
    u32::try_from(limit).unwrap_or(u32::MAX)
}

fn map_wasi_body_error(code: ErrorCode) -> aioduct::Error {
    aioduct::Error::Other(Box::new(WasiOutgoingBodyError { code }))
}

fn map_aioduct_error(error: aioduct::Error) -> ErrorCode {
    if let Some(code) = timeout_code_from_aioduct_error(&error) {
        return code;
    }
    if let Some(code) = request_trailer_policy_error_code_from_error(&error) {
        return code;
    }

    match error {
        aioduct::Error::InvalidUrl(_) => ErrorCode::HttpRequestUriInvalid,
        aioduct::Error::HttpsOnly(_) => ErrorCode::HttpRequestDenied,
        aioduct::Error::Tls(_) => ErrorCode::TlsProtocolError,
        aioduct::Error::Hyper(error) => {
            if let Some(code) = request_trailer_policy_error_code_from_error(&error) {
                code
            } else if let Some(limit) = request_body_limit_from_error(&error) {
                ErrorCode::HttpRequestBodySize(Some(limit))
            } else if let Some(code) = wasi_body_error_from_error(&error) {
                code
            } else {
                ErrorCode::HttpProtocolError
            }
        }
        aioduct::Error::Pool(_) => ErrorCode::ConnectionLimitReached,
        aioduct::Error::Io(error) => io_error_code(&error),
        aioduct::Error::Other(source) => {
            if let Some(code) = request_trailer_policy_error_code_from_error(source.as_ref()) {
                code
            } else if let Some(limit) = request_body_limit_from_error(source.as_ref()) {
                ErrorCode::HttpRequestBodySize(Some(limit))
            } else if let Some(code) = wasi_body_error_from_error(source.as_ref()) {
                code
            } else {
                ErrorCode::InternalError(Some("transport".into()))
            }
        }
        aioduct::Error::RemoteAddr { source, .. } => {
            if let Some(error) = source.downcast_ref::<std::io::Error>() {
                io_error_code(error)
            } else {
                ErrorCode::DestinationUnavailable
            }
        }
        _ => ErrorCode::InternalError(Some("transport".into())),
    }
}

fn timeout_code_from_aioduct_error(error: &aioduct::Error) -> Option<ErrorCode> {
    match error {
        aioduct::Error::Timeout => Some(ErrorCode::HttpResponseTimeout),
        aioduct::Error::ConnectTimeout => Some(ErrorCode::ConnectionTimeout),
        aioduct::Error::ReadTimeout => Some(ErrorCode::ConnectionReadTimeout),
        aioduct::Error::WriteTimeout => Some(ErrorCode::ConnectionWriteTimeout),
        aioduct::Error::Hyper(error) => timeout_code_from_error(error),
        aioduct::Error::Other(source) => timeout_code_from_error(source.as_ref()),
        aioduct::Error::RemoteAddr { source, .. } => timeout_code_from_error(source.as_ref()),
        _ => None,
    }
}

fn timeout_code_from_error(error: &(dyn StdError + 'static)) -> Option<ErrorCode> {
    let mut current = Some(error);
    while let Some(error) = current {
        if let Some(error) = error.downcast_ref::<aioduct::Error>()
            && let Some(code) = timeout_code_from_aioduct_error(error)
        {
            return Some(code);
        }
        if let Some(error) = error.downcast_ref::<std::io::Error>()
            && error.kind() == std::io::ErrorKind::TimedOut
        {
            return Some(ErrorCode::ConnectionTimeout);
        }
        current = error.source();
    }
    None
}

fn io_error_code(error: &std::io::Error) -> ErrorCode {
    match error.kind() {
        std::io::ErrorKind::ConnectionRefused => ErrorCode::ConnectionRefused,
        std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::BrokenPipe => {
            ErrorCode::ConnectionTerminated
        }
        std::io::ErrorKind::TimedOut => ErrorCode::ConnectionTimeout,
        std::io::ErrorKind::NotFound => ErrorCode::DestinationNotFound,
        _ => ErrorCode::DestinationUnavailable,
    }
}

fn request_body_limit_from_error(error: &(dyn StdError + 'static)) -> Option<u64> {
    let mut current = Some(error);
    while let Some(error) = current {
        if let Some(limit) = error.downcast_ref::<RequestBodyLimitExceeded>() {
            return Some(limit.limit);
        }
        if let Some(aioduct::Error::Other(source)) = error.downcast_ref::<aioduct::Error>()
            && let Some(limit) = source.downcast_ref::<RequestBodyLimitExceeded>()
        {
            return Some(limit.limit);
        }
        current = error.source();
    }
    None
}

fn request_trailer_policy_error_code_from_error(
    error: &(dyn StdError + 'static),
) -> Option<ErrorCode> {
    let mut current = Some(error);
    while let Some(error) = current {
        if let Some(error) = error.downcast_ref::<RequestTrailerPolicyError>() {
            return Some(error.to_error_code());
        }
        if let Some(aioduct::Error::Other(source)) = error.downcast_ref::<aioduct::Error>()
            && let Some(error) = source.downcast_ref::<RequestTrailerPolicyError>()
        {
            return Some(error.to_error_code());
        }
        current = error.source();
    }
    None
}

fn wasi_body_error_from_error(error: &(dyn StdError + 'static)) -> Option<ErrorCode> {
    let mut current = Some(error);
    while let Some(error) = current {
        if let Some(error) = error.downcast_ref::<WasiOutgoingBodyError>() {
            return Some(error.code.clone());
        }
        if let Some(aioduct::Error::Other(source)) = error.downcast_ref::<aioduct::Error>()
            && let Some(error) = source.downcast_ref::<WasiOutgoingBodyError>()
        {
            return Some(error.code.clone());
        }
        current = error.source();
    }
    None
}

#[derive(Debug)]
struct WasiOutgoingBodyError {
    code: ErrorCode,
}

impl fmt::Display for WasiOutgoingBodyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "WASI outgoing body error: {:?}", self.code)
    }
}

impl std::error::Error for WasiOutgoingBodyError {}

#[derive(Debug)]
struct RequestBodyLimitExceeded {
    limit: u64,
}

impl fmt::Display for RequestBodyLimitExceeded {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "WASI request body exceeded limit {}", self.limit)
    }
}

impl std::error::Error for RequestBodyLimitExceeded {}

#[derive(Debug)]
enum RequestTrailerPolicyError {
    ProtectedHeader,
    HeaderLimit { limit: usize },
}

impl RequestTrailerPolicyError {
    fn to_error_code(&self) -> ErrorCode {
        match self {
            Self::ProtectedHeader => ErrorCode::HttpRequestDenied,
            Self::HeaderLimit { limit } => {
                ErrorCode::HttpRequestTrailerSectionSize(Some(limit_to_u32(*limit)))
            }
        }
    }
}

impl fmt::Display for RequestTrailerPolicyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ProtectedHeader => write!(f, "WASI request trailers contained protected header"),
            Self::HeaderLimit { limit } => {
                write!(f, "WASI request trailers exceeded header limit {limit}")
            }
        }
    }
}

impl std::error::Error for RequestTrailerPolicyError {}

#[derive(Clone)]
struct RequestTrailerPolicy {
    forbid_sensitive_headers: bool,
    injected_headers: HeaderMap,
    header_limit: Option<usize>,
}

impl RequestTrailerPolicy {
    fn from_policy(policy: &ExactOriginPolicy) -> Self {
        Self {
            forbid_sensitive_headers: policy.forbid_sensitive_headers,
            injected_headers: policy.injected_headers.clone(),
            header_limit: policy.header_limit,
        }
    }

    fn check(
        &self,
        trailers: &HeaderMap,
        observer: &Option<RejectionObserver>,
        rejected: &mut bool,
    ) -> Result<(), aioduct::Error> {
        for (name, value) in trailers {
            if DEFAULT_FORBIDDEN_HEADERS.contains(name)
                || self.injected_headers.contains_key(name)
                || (self.forbid_sensitive_headers
                    && (is_sensitive_header_name(name) || value.is_sensitive()))
            {
                notify_rejection_once(observer, rejected, RejectionReason::ProtectedHeader);
                return Err(aioduct::Error::Other(Box::new(
                    RequestTrailerPolicyError::ProtectedHeader,
                )));
            }
        }

        if let Some(limit) = self.header_limit
            && header_section_size(trailers) > limit
        {
            notify_rejection_once(observer, rejected, RejectionReason::HeaderLimit);
            return Err(aioduct::Error::Other(Box::new(
                RequestTrailerPolicyError::HeaderLimit { limit },
            )));
        }

        Ok(())
    }
}

fn notify_rejection_once(
    observer: &Option<RejectionObserver>,
    rejected: &mut bool,
    reason: RejectionReason,
) {
    if *rejected {
        return;
    }
    *rejected = true;
    if let Some(observer) = observer {
        observer(reason);
    }
}

pin_project! {
    struct RequestLimitBody<B> {
        #[pin]
        inner: B,
        body_limit: Option<u64>,
        seen: u64,
        trailer_policy: Option<RequestTrailerPolicy>,
        rejection_observer: Option<RejectionObserver>,
        rejected: bool,
    }
}

impl<B> RequestLimitBody<B> {
    fn new_policy(inner: B, body_limit: Option<u64>, policy: &ExactOriginPolicy) -> Self {
        Self {
            inner,
            body_limit,
            seen: 0,
            trailer_policy: Some(RequestTrailerPolicy::from_policy(policy)),
            rejection_observer: policy.rejection_observer.clone(),
            rejected: false,
        }
    }
}

impl<B> Body for RequestLimitBody<B>
where
    B: Body<Data = Bytes, Error = aioduct::Error>,
{
    type Data = Bytes;
    type Error = aioduct::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        let mut this = self.project();
        match this.inner.as_mut().poll_frame(cx) {
            Poll::Ready(Some(Ok(frame))) => {
                if let Some(data) = frame.data_ref()
                    && let Some(limit) = this.body_limit
                {
                    let len = u64::try_from(data.len()).unwrap_or(u64::MAX);
                    if this.seen.saturating_add(len) > *limit {
                        notify_rejection_once(
                            this.rejection_observer,
                            this.rejected,
                            RejectionReason::BodyLimit,
                        );
                        return Poll::Ready(Some(Err(aioduct::Error::Other(Box::new(
                            RequestBodyLimitExceeded { limit: *limit },
                        )))));
                    }
                    *this.seen = this.seen.saturating_add(len);
                }
                if let Some(trailers) = frame.trailers_ref()
                    && let Some(policy) = this.trailer_policy
                    && let Err(error) =
                        policy.check(trailers, this.rejection_observer, this.rejected)
                {
                    return Poll::Ready(Some(Err(error)));
                }
                Poll::Ready(Some(Ok(frame)))
            }
            other => other,
        }
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> http_body::SizeHint {
        self.inner.size_hint()
    }
}

pin_project! {
    struct ResponseLimitBody<B> {
        #[pin]
        inner: B,
        body_limit: Option<u64>,
        header_limit: Option<usize>,
        seen: u64,
        rejection_observer: Option<RejectionObserver>,
        rejected: bool,
    }
}

impl<B> ResponseLimitBody<B> {
    fn new_policy(
        inner: B,
        body_limit: Option<u64>,
        header_limit: Option<usize>,
        rejection_observer: Option<RejectionObserver>,
    ) -> Self {
        Self {
            inner,
            body_limit,
            header_limit,
            seen: 0,
            rejection_observer,
            rejected: false,
        }
    }
}

impl<B> Body for ResponseLimitBody<B>
where
    B: Body<Data = Bytes, Error = ErrorCode>,
{
    type Data = Bytes;
    type Error = ErrorCode;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        let mut this = self.project();
        match this.inner.as_mut().poll_frame(cx) {
            Poll::Ready(Some(Ok(frame))) => {
                if let Some(data) = frame.data_ref()
                    && let Some(limit) = this.body_limit
                {
                    let len = u64::try_from(data.len()).unwrap_or(u64::MAX);
                    if this.seen.saturating_add(len) > *limit {
                        notify_rejection_once(
                            this.rejection_observer,
                            this.rejected,
                            RejectionReason::BodyLimit,
                        );
                        return Poll::Ready(Some(Err(ErrorCode::HttpResponseBodySize(Some(
                            *limit,
                        )))));
                    }
                    *this.seen = this.seen.saturating_add(len);
                }
                if let Some(trailers) = frame.trailers_ref()
                    && let Some(limit) = this.header_limit
                    && header_section_size(trailers) > *limit
                {
                    notify_rejection_once(
                        this.rejection_observer,
                        this.rejected,
                        RejectionReason::HeaderLimit,
                    );
                    return Poll::Ready(Some(Err(ErrorCode::HttpResponseTrailerSectionSize(
                        Some(limit_to_u32(*limit)),
                    ))));
                }
                Poll::Ready(Some(Ok(frame)))
            }
            other => other,
        }
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> http_body::SizeHint {
        self.inner.size_hint()
    }
}

pin_project! {
    struct DeadlineBody<B> {
        #[pin]
        inner: B,
        deadline: Instant,
        rejection_observer: Option<RejectionObserver>,
        rejected: bool,
        #[pin]
        timer: Option<async_io::Timer>,
    }
}

impl<B> DeadlineBody<B> {
    fn new(inner: B, deadline: Instant, rejection_observer: Option<RejectionObserver>) -> Self {
        Self {
            inner,
            deadline,
            rejection_observer,
            rejected: false,
            timer: None,
        }
    }
}

impl<B> Body for DeadlineBody<B>
where
    B: Body<Data = Bytes, Error = ErrorCode>,
{
    type Data = Bytes;
    type Error = ErrorCode;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        let mut this = self.project();
        if Instant::now() >= *this.deadline {
            notify_rejection_once(
                this.rejection_observer,
                this.rejected,
                RejectionReason::Deadline,
            );
            return Poll::Ready(Some(Err(ErrorCode::HttpResponseTimeout)));
        }

        match this.inner.as_mut().poll_frame(cx) {
            Poll::Ready(result) => Poll::Ready(result),
            Poll::Pending => {
                if this.timer.as_ref().get_ref().is_none() {
                    this.timer.set(Some(async_io::Timer::at(*this.deadline)));
                }
                if let Some(timer) = this.timer.as_mut().as_pin_mut()
                    && let Poll::Ready(_) = timer.poll(cx)
                {
                    this.timer.set(None);
                    notify_rejection_once(
                        this.rejection_observer,
                        this.rejected,
                        RejectionReason::Deadline,
                    );
                    return Poll::Ready(Some(Err(ErrorCode::HttpResponseTimeout)));
                }
                Poll::Pending
            }
        }
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> http_body::SizeHint {
        self.inner.size_hint()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use http::header::AUTHORIZATION;
    use http_body_util::{Empty, Full, StreamBody};
    use std::marker::PhantomData;
    use std::sync::Mutex;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use wasmtime_wasi::p2::{InputStream, Pollable, StreamError};
    use wasmtime_wasi_http::p2::body::HostIncomingBody;

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

    #[test]
    fn exact_origin_rejects_path() {
        assert!(matches!(
            ExactOriginPolicy::new("https://example.com/path"),
            Err(PolicyError::OriginMustNotContainPath)
        ));
    }

    #[test]
    fn builder_rejects_forbidden_injected_header() {
        let policy = ExactOriginPolicy::new("http://example.com")
            .expect("policy should build")
            .inject_header(http::header::HOST, HeaderValue::from_static("example.com"));
        assert!(matches!(
            WasiHttpHost::builder().policy(policy).build(),
            Err(BuildError::Policy(PolicyError::InjectedForbiddenHeader(_)))
        ));
    }

    #[cfg(any(
        all(not(feature = "tokio"), feature = "smol"),
        all(not(feature = "tokio"), not(feature = "smol"), feature = "compio")
    ))]
    #[test]
    fn builder_requires_explicit_transport_without_tokio_default() {
        let policy = ExactOriginPolicy::new("http://example.com").expect("policy should build");
        assert!(matches!(
            WasiHttpHost::builder().policy(policy).build(),
            Err(BuildError::MissingTransport)
        ));
    }

    #[tokio::test]
    async fn origin_mismatch_is_denied_before_transport() {
        let policy = ExactOriginPolicy::new("http://127.0.0.1:1").expect("policy should build");
        let host = test_host(policy);
        let err = host
            .send_inner(request("http://127.0.0.1:2/".into()), config(false))
            .await
            .expect_err("origin mismatch should be rejected");
        assert!(matches!(err, ErrorCode::HttpRequestDenied));
    }

    #[tokio::test]
    async fn rejection_observer_receives_low_cardinality_reason() {
        let reasons = Arc::new(Mutex::new(Vec::new()));
        let observed = reasons.clone();
        let policy = ExactOriginPolicy::new("http://127.0.0.1:1")
            .expect("policy should build")
            .on_rejection(move |reason| {
                observed.lock().expect("observer lock").push(reason);
            });
        let host = test_host(policy);
        let err = host
            .send_inner(request("http://127.0.0.1:2/".into()), config(false))
            .await
            .expect_err("origin mismatch should be rejected");
        assert!(matches!(err, ErrorCode::HttpRequestDenied));
        let captured = reasons.lock().expect("observer lock");
        assert_eq!(captured.as_slice(), &[RejectionReason::OriginMismatch]);
        assert_eq!(captured[0].as_str(), "origin_mismatch");
    }

    #[tokio::test]
    async fn forbidden_sensitive_header_is_denied() {
        let policy = ExactOriginPolicy::new("http://127.0.0.1:1")
            .expect("policy should build")
            .forbid_sensitive_headers();
        let host = test_host(policy);
        let req = hyper::Request::builder()
            .uri("http://127.0.0.1:1/")
            .header(AUTHORIZATION, "Bearer guest")
            .body(empty_body())
            .expect("request should build");
        let err = host
            .send_inner(req, config(false))
            .await
            .expect_err("sensitive header should be rejected");
        assert!(matches!(err, ErrorCode::HttpRequestDenied));
    }

    #[tokio::test]
    async fn host_injects_secret_header_after_validation() {
        let response = b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\nok";
        let (addr, seen) = raw_server(response).await;
        let policy = ExactOriginPolicy::new(&format!("http://{addr}"))
            .expect("policy should build")
            .forbid_sensitive_headers()
            .inject_header(AUTHORIZATION, HeaderValue::from_static("Bearer secret"));
        let host = test_host(policy);
        let incoming = host
            .send_inner(request(format!("http://{addr}/")), config(false))
            .await
            .expect("request should succeed");
        assert_eq!(incoming.resp.status(), http::StatusCode::OK);
        let text = seen.await.expect("server should capture request");
        assert!(
            text.to_ascii_lowercase()
                .contains("authorization: bearer secret")
        );
    }

    #[tokio::test]
    async fn response_header_limit_is_enforced() {
        let response = b"HTTP/1.1 200 OK\r\nx-large: abcdefghijklmnop\r\ncontent-length: 0\r\n\r\n";
        let (addr, _seen) = raw_server(response).await;
        let policy = ExactOriginPolicy::new(&format!("http://{addr}"))
            .expect("policy should build")
            .header_limit(8);
        let host = test_host(policy);
        let err = host
            .send_inner(request(format!("http://{addr}/")), config(false))
            .await
            .expect_err("response headers should exceed limit");
        assert!(matches!(
            err,
            ErrorCode::HttpResponseHeaderSectionSize(Some(8))
        ));
    }

    #[tokio::test]
    async fn request_trailer_injected_header_is_denied() {
        let reasons = Arc::new(Mutex::new(Vec::new()));
        let observed = reasons.clone();
        let policy = ExactOriginPolicy::new("http://example.com")
            .expect("policy should build")
            .inject_header(AUTHORIZATION, HeaderValue::from_static("Bearer secret"))
            .on_rejection(move |reason| {
                observed.lock().expect("observer lock").push(reason);
            });
        let host = WasiHttpHost::builder()
            .transport(CollectingTransport)
            .policy(policy)
            .build()
            .expect("host should build");

        let mut trailers = HeaderMap::new();
        trailers.insert(AUTHORIZATION, HeaderValue::from_static("Bearer guest"));
        let req = hyper::Request::builder()
            .method(http::Method::POST)
            .uri("http://example.com/")
            .body(request_trailers_body(trailers))
            .expect("request should build");
        let err = host
            .send_inner(req, config(false))
            .await
            .expect_err("injected header trailer should be rejected");

        assert!(matches!(err, ErrorCode::HttpRequestDenied));
        let captured = reasons.lock().expect("observer lock");
        assert_eq!(captured.as_slice(), &[RejectionReason::ProtectedHeader]);
    }

    #[tokio::test]
    async fn request_trailer_header_limit_is_enforced() {
        let reasons = Arc::new(Mutex::new(Vec::new()));
        let observed = reasons.clone();
        let policy = ExactOriginPolicy::new("http://example.com")
            .expect("policy should build")
            .header_limit(8)
            .on_rejection(move |reason| {
                observed.lock().expect("observer lock").push(reason);
            });
        let host = WasiHttpHost::builder()
            .transport(CollectingTransport)
            .policy(policy)
            .build()
            .expect("host should build");

        let mut trailers = HeaderMap::new();
        trailers.insert("x-large", HeaderValue::from_static("abcdefghijklmnop"));
        let req = hyper::Request::builder()
            .method(http::Method::POST)
            .uri("http://example.com/")
            .body(request_trailers_body(trailers))
            .expect("request should build");
        let err = host
            .send_inner(req, config(false))
            .await
            .expect_err("oversized request trailers should be rejected");

        assert!(matches!(
            err,
            ErrorCode::HttpRequestTrailerSectionSize(Some(8))
        ));
        let captured = reasons.lock().expect("observer lock");
        assert_eq!(captured.as_slice(), &[RejectionReason::HeaderLimit]);
    }

    #[tokio::test]
    async fn response_trailer_header_limit_is_enforced() {
        let reasons = Arc::new(Mutex::new(Vec::new()));
        let observed = reasons.clone();
        let policy = ExactOriginPolicy::new("http://example.com")
            .expect("policy should build")
            .header_limit(8)
            .on_rejection(move |reason| {
                observed.lock().expect("observer lock").push(reason);
            });
        let mut trailers = HeaderMap::new();
        trailers.insert("x-large", HeaderValue::from_static("abcdefghijklmnop"));
        let host = WasiHttpHost::builder()
            .transport(TrailerResponseTransport { trailers })
            .policy(policy)
            .build()
            .expect("host should build");

        let incoming = host
            .send_inner(request("http://example.com/".into()), config(false))
            .await
            .expect("response headers should succeed");
        let err = incoming
            .resp
            .into_body()
            .collect()
            .await
            .expect_err("oversized response trailers should be rejected");

        assert!(matches!(
            err,
            ErrorCode::HttpResponseTrailerSectionSize(Some(8))
        ));
        let captured = reasons.lock().expect("observer lock");
        assert_eq!(captured.as_slice(), &[RejectionReason::HeaderLimit]);
    }

    #[tokio::test]
    async fn request_body_limit_is_mapped_to_wasi_error() {
        let response = b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\n\r\n";
        let (addr, seen) = raw_server(response).await;
        let policy = ExactOriginPolicy::new(&format!("http://{addr}"))
            .expect("policy should build")
            .body_limit(2);
        let host = test_host(policy);
        let req = hyper::Request::builder()
            .method(http::Method::POST)
            .uri(format!("http://{addr}/"))
            .body(full_body(b"abcd"))
            .expect("request should build");
        let err = host
            .send_inner(req, config(false))
            .await
            .expect_err("request body should exceed limit");
        match err {
            ErrorCode::HttpRequestBodySize(Some(2)) => {}
            other => panic!("expected request body limit error, got {other:?}"),
        }
        assert!(
            tokio::time::timeout(Duration::from_millis(20), seen)
                .await
                .is_err(),
            "known oversized body should be rejected before opening upstream connection"
        );
    }

    #[tokio::test]
    async fn streaming_request_body_limit_notifies_rejection() {
        let reasons = Arc::new(Mutex::new(Vec::new()));
        let observed = reasons.clone();
        let policy = ExactOriginPolicy::new("http://example.com")
            .expect("policy should build")
            .body_limit(2)
            .on_rejection(move |reason| {
                observed.lock().expect("observer lock").push(reason);
            });
        let body: aioduct::body::RequestBodySend = full_body(b"abcd")
            .map_err(map_wasi_body_error)
            .boxed_unsync();
        let err = RequestLimitBody::new_policy(body, policy.body_limit, &policy)
            .collect()
            .await
            .expect_err("request body should exceed limit");
        assert!(request_body_limit_from_error(&err).is_some());
        let captured = reasons.lock().expect("observer lock");
        assert_eq!(captured.as_slice(), &[RejectionReason::BodyLimit]);
    }

    #[tokio::test]
    async fn response_body_limit_notifies_rejection() {
        let reasons = Arc::new(Mutex::new(Vec::new()));
        let observed = reasons.clone();
        let observer: RejectionObserver = Arc::new(move |reason| {
            observed.lock().expect("observer lock").push(reason);
        });
        let body: HyperIncomingBody = Full::new(Bytes::from_static(b"abcd"))
            .map_err(|never| match never {})
            .boxed_unsync();
        let err = ResponseLimitBody::new_policy(body, Some(2), None, Some(observer))
            .collect()
            .await
            .expect_err("response body should exceed limit");
        assert!(matches!(err, ErrorCode::HttpResponseBodySize(Some(2))));
        let captured = reasons.lock().expect("observer lock");
        assert_eq!(captured.as_slice(), &[RejectionReason::BodyLimit]);
    }

    #[tokio::test]
    async fn deadline_body_notifies_rejection() {
        let reasons = Arc::new(Mutex::new(Vec::new()));
        let observed = reasons.clone();
        let observer: RejectionObserver = Arc::new(move |reason| {
            observed.lock().expect("observer lock").push(reason);
        });
        let body: HyperIncomingBody = Empty::<Bytes>::new()
            .map_err(|never| match never {})
            .boxed_unsync();
        let err = DeadlineBody::new(
            body,
            Instant::now() - Duration::from_millis(1),
            Some(observer),
        )
        .collect()
        .await
        .expect_err("deadline should expire");
        assert!(matches!(err, ErrorCode::HttpResponseTimeout));
        let captured = reasons.lock().expect("observer lock");
        assert_eq!(captured.as_slice(), &[RejectionReason::Deadline]);
    }

    #[tokio::test]
    async fn deadline_body_wakes_pending_body() {
        let reasons = Arc::new(Mutex::new(Vec::new()));
        let observed = reasons.clone();
        let observer: RejectionObserver = Arc::new(move |reason| {
            observed.lock().expect("observer lock").push(reason);
        });
        let err = DeadlineBody::new(
            pending_incoming_body(),
            Instant::now() + Duration::from_millis(10),
            Some(observer),
        )
        .collect()
        .await
        .expect_err("deadline should wake stalled response body");
        assert!(matches!(err, ErrorCode::HttpResponseTimeout));
        let captured = reasons.lock().expect("observer lock");
        assert_eq!(captured.as_slice(), &[RejectionReason::Deadline]);
    }

    #[tokio::test]
    async fn wasmtime_body_wrapper_preserves_host_deadline_mapping() {
        let reasons = Arc::new(Mutex::new(Vec::new()));
        let observed = reasons.clone();
        let policy = ExactOriginPolicy::new("http://example.com")
            .expect("policy should build")
            .deadline(Instant::now() + Duration::from_millis(10))
            .on_rejection(move |reason| {
                observed.lock().expect("observer lock").push(reason);
            });
        let host = WasiHttpHost::builder()
            .transport(PendingResponseTransport)
            .policy(policy)
            .build()
            .expect("host should build");
        let cfg = config(false);
        let guest_between_bytes_timeout = cfg.between_bytes_timeout;
        let incoming = host
            .send_inner(request("http://example.com/".to_string()), cfg)
            .await
            .expect("host should return response headers");

        assert_eq!(incoming.between_bytes_timeout, guest_between_bytes_timeout);

        let IncomingResponse {
            resp,
            worker,
            between_bytes_timeout,
        } = incoming;
        let mut body = HostIncomingBody::new(resp.into_body(), between_bytes_timeout);
        if let Some(worker) = worker {
            body.retain_worker(worker);
        }
        let mut stream = body.take_stream().expect("body stream should be available");
        stream.ready().await;
        let err = stream.read(1).expect_err("deadline should surface");
        match err {
            StreamError::LastOperationFailed(error) => {
                assert!(matches!(
                    error.downcast_ref::<ErrorCode>(),
                    Some(ErrorCode::HttpResponseTimeout)
                ));
            }
            other => panic!("expected last operation failure, got {other:?}"),
        }
        let captured = reasons.lock().expect("observer lock");
        assert_eq!(captured.as_slice(), &[RejectionReason::Deadline]);
    }

    #[test]
    fn wasi_body_error_mapping_preserves_error_code() {
        let err = map_wasi_body_error(ErrorCode::ConnectionWriteTimeout);
        assert!(matches!(
            map_aioduct_error(err),
            ErrorCode::ConnectionWriteTimeout
        ));
    }

    #[tokio::test]
    async fn wasi_body_error_mapping_preserves_hyper_wrapped_error() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("listener should bind");
        let addr = listener.local_addr().expect("listener should have address");
        tokio::spawn(async move {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            let mut buf = [0_u8; 1024];
            let _ = stream.read(&mut buf).await;
            tokio::time::sleep(Duration::from_millis(100)).await;
        });

        let policy =
            ExactOriginPolicy::new(&format!("http://{addr}")).expect("policy should build");
        let host = test_host(policy);
        let req = hyper::Request::builder()
            .method(http::Method::POST)
            .uri(format!("http://{addr}/"))
            .body(failing_body(ErrorCode::ConnectionWriteTimeout))
            .expect("request should build");
        let err = host
            .send_inner(req, config(false))
            .await
            .expect_err("request body error should be preserved");
        assert!(matches!(err, ErrorCode::ConnectionWriteTimeout));
    }

    #[tokio::test]
    async fn native_forward_deadline_timeout_notifies_rejection() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("listener should bind");
        let addr = listener.local_addr().expect("listener should have address");
        tokio::spawn(async move {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            let mut buf = [0_u8; 1024];
            let _ = stream.read(&mut buf).await;
            tokio::time::sleep(Duration::from_millis(100)).await;
            let _ = stream
                .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\n\r\n")
                .await;
        });

        let reasons = Arc::new(Mutex::new(Vec::new()));
        let observed = reasons.clone();
        let policy = ExactOriginPolicy::new(&format!("http://{addr}"))
            .expect("policy should build")
            .deadline(Instant::now() + Duration::from_millis(10))
            .on_rejection(move |reason| {
                observed.lock().expect("observer lock").push(reason);
            });
        let host = test_host(policy);
        let err = host
            .send_inner(request(format!("http://{addr}/")), config(false))
            .await
            .expect_err("host deadline should time out native forward");
        assert!(matches!(
            err,
            ErrorCode::HttpResponseTimeout | ErrorCode::ConnectionReadTimeout
        ));
        let captured = reasons.lock().expect("observer lock");
        assert_eq!(captured.as_slice(), &[RejectionReason::Deadline]);
    }

    #[tokio::test]
    async fn deadline_upload_timeout_notifies_rejection() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("listener should bind");
        let addr = listener.local_addr().expect("listener should have address");
        tokio::spawn(async move {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            let mut buf = [0_u8; 1024];
            let _ = stream.read(&mut buf).await;
            tokio::time::sleep(Duration::from_millis(100)).await;
        });

        let reasons = Arc::new(Mutex::new(Vec::new()));
        let observed = reasons.clone();
        let policy = ExactOriginPolicy::new(&format!("http://{addr}"))
            .expect("policy should build")
            .deadline(Instant::now() + Duration::from_millis(10))
            .on_rejection(move |reason| {
                observed.lock().expect("observer lock").push(reason);
            });
        let host = test_host(policy);
        let req = hyper::Request::builder()
            .method(http::Method::POST)
            .uri(format!("http://{addr}/"))
            .body(pending_body())
            .expect("request should build");
        let err = host
            .send_inner(req, config(false))
            .await
            .expect_err("host deadline should time out stalled upload");
        assert!(matches!(
            err,
            ErrorCode::HttpResponseTimeout | ErrorCode::ConnectionWriteTimeout
        ));
        let captured = reasons.lock().expect("observer lock");
        assert_eq!(captured.as_slice(), &[RejectionReason::Deadline]);
    }

    #[tokio::test]
    async fn first_byte_timeout_maps_to_connection_read_timeout() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("listener should bind");
        let addr = listener.local_addr().expect("listener should have address");
        tokio::spawn(async move {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            let mut buf = [0_u8; 1024];
            let _ = stream.read(&mut buf).await;
            tokio::time::sleep(Duration::from_millis(100)).await;
            let _ = stream
                .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\n\r\n")
                .await;
        });

        let policy =
            ExactOriginPolicy::new(&format!("http://{addr}")).expect("policy should build");
        let host = test_host(policy);
        let mut cfg = config(false);
        cfg.first_byte_timeout = Duration::from_millis(10);
        let err = host
            .send_inner(request(format!("http://{addr}/")), cfg)
            .await
            .expect_err("first byte should time out");
        assert!(matches!(err, ErrorCode::ConnectionReadTimeout));
    }

    #[cfg(feature = "smol")]
    #[tokio::test]
    async fn smol_transport_services_wasi_http_request() {
        let response = b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\nok";
        let (addr, seen) = raw_server(response).await;
        let policy = ExactOriginPolicy::new(&format!("http://{addr}"))
            .expect("policy should build")
            .inject_header(
                AUTHORIZATION,
                HeaderValue::from_static("Bearer smol-secret"),
            );
        let transport = aioduct::SmolClient::builder()
            .build()
            .expect("smol transport should build");
        let host = WasiHttpHost::builder()
            .transport(transport)
            .policy(policy)
            .build()
            .expect("host should build");
        let incoming = host
            .send_inner(request(format!("http://{addr}/")), config(false))
            .await
            .expect("smol transport should forward request");
        assert_eq!(incoming.resp.status(), http::StatusCode::OK);
        let text = seen.await.expect("server should capture request");
        assert!(
            text.to_ascii_lowercase()
                .contains("authorization: bearer smol-secret")
        );
    }

    #[cfg(feature = "compio")]
    #[tokio::test]
    async fn compio_transport_services_wasi_http_request() {
        let response = b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\nok";
        let (addr, seen) = raw_server(response).await;
        let policy = ExactOriginPolicy::new(&format!("http://{addr}"))
            .expect("policy should build")
            .inject_header(
                AUTHORIZATION,
                HeaderValue::from_static("Bearer compio-secret"),
            );
        let transport = CompioHostTransport::new().expect("compio host transport should start");
        let host = WasiHttpHost::builder()
            .transport(transport)
            .policy(policy)
            .build()
            .expect("host should build");
        let incoming = host
            .send_inner(request(format!("http://{addr}/")), config(false))
            .await
            .expect("compio transport should forward request");
        assert_eq!(incoming.resp.status(), http::StatusCode::OK);
        let text = seen.await.expect("server should capture request");
        assert!(
            text.to_ascii_lowercase()
                .contains("authorization: bearer compio-secret")
        );
    }

    #[cfg(feature = "tokio")]
    #[tokio::test]
    async fn custom_ca_allows_self_signed_tls() {
        let (addr, cert_der, _counter) =
            aioduct_test_server::tls::tls_h1_server(&[b"http/1.1"]).await;
        let cert = aioduct::Certificate::from_der(cert_der.to_vec());
        let transport = aioduct::TokioClient::builder()
            .add_root_certificates(&[cert])
            .build()
            .expect("transport should build");
        let policy =
            ExactOriginPolicy::new(&format!("https://localhost:{}", addr.port())).expect("policy");
        let host = WasiHttpHost::builder()
            .transport(transport)
            .policy(policy)
            .build()
            .expect("host should build");
        let incoming = host
            .send_inner(
                request(format!("https://localhost:{}/", addr.port())),
                config(true),
            )
            .await
            .expect("custom CA should trust self-signed server");
        assert_eq!(incoming.resp.status(), http::StatusCode::OK);
    }

    #[cfg(feature = "tokio")]
    #[tokio::test]
    async fn insecure_transport_allows_self_signed_tls() {
        let (addr, _cert_der, _counter) =
            aioduct_test_server::tls::tls_h1_server(&[b"http/1.1"]).await;
        let transport = aioduct::TokioClient::builder().danger_accept_invalid_certs();
        let policy =
            ExactOriginPolicy::new(&format!("https://localhost:{}", addr.port())).expect("policy");
        let host = WasiHttpHost::builder()
            .transport_builder(transport)
            .policy(policy)
            .build()
            .expect("host should build");
        let incoming = host
            .send_inner(
                request(format!("https://localhost:{}/", addr.port())),
                config(true),
            )
            .await
            .expect("insecure mode should accept self-signed server");
        assert_eq!(incoming.resp.status(), http::StatusCode::OK);
    }
}
