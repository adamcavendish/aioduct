use std::sync::Arc;

use ::wasmtime_wasi_http::p2::bindings::http::types::ErrorCode;
use ::wasmtime_wasi_http::p2::body::{HyperIncomingBody, HyperOutgoingBody};
use ::wasmtime_wasi_http::p2::types::{
    HostFutureIncomingResponse, IncomingResponse, OutgoingRequestConfig,
};
use ::wasmtime_wasi_http::p2::{HttpResult, WasiHttpHooks};
use http::header::HeaderName;
use http_body_util::BodyExt;

#[cfg(feature = "tokio")]
use super::TokioTransportBuilder;
use super::transport::HostForwardOptions;
use super::{
    BuildError, DeadlineBody, ExactOriginPolicy, RejectionReason, RequestLimitBody,
    ResponseLimitBody, WasiHostTransport, map_aioduct_error, map_wasi_body_error,
};

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

    pub(crate) async fn send_inner(
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
    ) -> Result<http::Request<crate::body::RequestBodySend>, ErrorCode> {
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

        let mut body: crate::body::RequestBodySend =
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
        let task = ::wasmtime_wasi::runtime::spawn(async move {
            Ok::<Result<IncomingResponse, ErrorCode>, ::wasmtime::Error>(
                host.send_inner(request, config).await,
            )
        });
        Ok(HostFutureIncomingResponse::pending(task))
    }

    fn is_forbidden_header(&mut self, name: &HeaderName) -> bool {
        self.policy.is_protected_request_header(name) || self.policy.is_denied_request_header(name)
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
    Ok(Arc::new(crate::TokioClient::builder().build()?))
}

#[cfg(not(feature = "tokio"))]
fn default_transport() -> Result<Arc<dyn WasiHostTransport>, BuildError> {
    Err(BuildError::MissingTransport)
}
