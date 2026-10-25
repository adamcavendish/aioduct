use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use ::wasmtime_wasi_http::p2::body::{HyperIncomingBody, HyperOutgoingBody};
use ::wasmtime_wasi_http::p2::types::IncomingResponse;
use ::wasmtime_wasi_http::{Error as ErrorCode, RequestOptions, WasiHttpHooks};
use http::header::{HOST, HeaderValue};
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
        config: RequestOptions,
    ) -> Result<IncomingResponse, ErrorCode> {
        let native_request = self.prepare_request(request)?;
        // Wasmtime passes unset guest options through to the host hook.
        // Retain the ten-minute defaults previously applied by its P2 handler.
        let default_timeout = Duration::from_secs(600);
        let connect_timeout = self
            .policy
            .cap_with_deadline(config.connect_timeout.unwrap_or(default_timeout))?;
        let first_byte_timeout = self
            .policy
            .cap_with_deadline(config.first_byte_timeout.unwrap_or(default_timeout))?;
        let read_timeout = self
            .policy
            .cap_with_deadline(config.between_bytes_timeout.unwrap_or(default_timeout))?;

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
        })
    }

    fn prepare_request(
        &self,
        request: hyper::Request<HyperOutgoingBody>,
    ) -> Result<http::Request<crate::body::RequestBodySend>, ErrorCode> {
        self.policy.validate_origin(request.uri())?;
        self.policy.check_request_headers(request.headers())?;
        self.policy.check_request_header_limit(request.headers())?;
        self.policy
            .check_request_body_known_limit(request.headers(), request.body())?;

        let (mut parts, body) = request.into_parts();
        if parts.version == http::Version::HTTP_11 && !parts.headers.contains_key(HOST) {
            let authority = parts
                .uri
                .authority()
                .ok_or(ErrorCode::HttpRequestUriInvalid)?;
            let host = HeaderValue::try_from(authority.as_str())
                .map_err(|_| ErrorCode::HttpRequestUriInvalid)?;
            parts.headers.insert(HOST, host);
        }
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
    fn set_host_header(&mut self) -> bool {
        // Validate guest headers before deriving Host from the allowed URI in
        // prepare_request. Wasmtime's default injection would look guest-supplied.
        false
    }

    fn send_request(
        &mut self,
        request: hyper::Request<HyperOutgoingBody>,
        config: Option<RequestOptions>,
        response_done: Box<dyn Future<Output = Result<(), ErrorCode>> + Send>,
    ) -> Box<
        dyn Future<
                Output = Result<
                    (
                        hyper::Response<HyperIncomingBody>,
                        Box<dyn Future<Output = Result<(), ErrorCode>> + Send>,
                    ),
                    ErrorCode,
                >,
            > + Send,
    > {
        let host = self.clone();
        Box::new(async move {
            let IncomingResponse { resp, worker } =
                host.send_inner(request, config.unwrap_or_default()).await?;
            // Wasmtime retains this future with the response body. Keep the
            // Local transport worker owned until that task completes or drops.
            let completion: Box<dyn Future<Output = Result<(), ErrorCode>> + Send> =
                Box::new(async move {
                    Pin::from(response_done).await?;
                    if let Some(worker) = worker {
                        worker.await;
                    }
                    Ok(())
                });
            Ok((resp, completion))
        })
    }

    fn forbidden_header_filter(&mut self) -> ::wasmtime_wasi_http::ForbiddenHeaderFilter {
        let policy = self.policy.clone();
        Arc::new(move |name| {
            policy.is_protected_request_header(name) || policy.is_denied_request_header(name)
        })
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
