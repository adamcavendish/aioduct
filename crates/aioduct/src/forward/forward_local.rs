use std::sync::Arc;
use std::time::Duration;

use crate::body::RequestBodyLocal;
use crate::client::{BodyReplayability, FreshConnectionRequired, HttpEngineLocal};
use crate::error::{BuilderError, Error};
use crate::message_signatures::{
    AutomaticMessageSignature, MessageSignatureConfig, MessageSignatureLocalAsyncSigner,
    MessageSignatureSigner,
};
use crate::pool::ProtocolHint;
use crate::response::Response;
use crate::runtime::{ConnectorLocal, RuntimeLocal};
use bytes::Bytes;
use http::Uri;
use http::header::{HeaderMap, HeaderName, HeaderValue};
use http_body::Body;

use super::dispatch_plan::{
    ForwardDispatchPlan, ForwardRewrite, capture_downstream_connect_protocol, rewrite_for_upstream,
};
use super::{
    apply_forward_response_content_digest, is_h1_upgrade_request,
    prepare_forward_response_related_request, reject_response_finalization_for_tunnel_or_upgrade,
    sanitize_forward_response_body,
};

type RequestHook = Box<dyn FnOnce(&mut http::request::Parts)>;
type ResponseHook = Box<dyn FnOnce(&mut Response)>;

/// Builder for forwarding an incoming HTTP request on a `!Send` runtime.
///
/// Created via [`HttpEngineLocal::forward_local`]. Mirrors [`super::ForwardBuilderSend`]
/// for completion-based runtimes.
pub struct ForwardBuilderLocal<'a, R: RuntimeLocal, C: ConnectorLocal + Clone, B> {
    client: &'a HttpEngineLocal<R, C>,
    request: http::Request<B>,
    upstream: Option<Uri>,
    strip_prefix: Option<String>,
    preserve_host: bool,
    timeout: Option<Duration>,
    connect_timeout: Option<Duration>,
    first_byte_timeout: Option<Duration>,
    write_timeout: Option<Duration>,
    read_timeout: Option<Duration>,
    extra_headers: HeaderMap,
    remove_headers: Vec<HeaderName>,
    forward_headers: Vec<HeaderName>,
    protocol_hint: ProtocolHint,
    sign_final_request: bool,
    builder_error: Option<BuilderError>,
    on_request: Option<RequestHook>,
    on_response: Option<ResponseHook>,
    force_h1_upgrade: bool,
    downstream_target_uri: Option<Uri>,
    response_content_digest_max_bytes: Option<usize>,
    response_message_signature: Option<AutomaticMessageSignature>,
}

impl<'a, R: RuntimeLocal, C: ConnectorLocal + Clone, B> ForwardBuilderLocal<'a, R, C, B>
where
    B: Body<Data = Bytes> + 'static,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    pub(crate) fn new(client: &'a HttpEngineLocal<R, C>, request: http::Request<B>) -> Self {
        let protocol_hint = request
            .extensions()
            .get::<ProtocolHint>()
            .copied()
            .unwrap_or(ProtocolHint::Auto);
        Self {
            client,
            request,
            upstream: None,
            strip_prefix: None,
            preserve_host: false,
            timeout: None,
            connect_timeout: None,
            first_byte_timeout: None,
            write_timeout: None,
            read_timeout: None,
            extra_headers: HeaderMap::new(),
            remove_headers: Vec::new(),
            forward_headers: Vec::new(),
            protocol_hint,
            sign_final_request: true,
            builder_error: None,
            on_request: None,
            on_response: None,
            force_h1_upgrade: false,
            downstream_target_uri: None,
            response_content_digest_max_bytes: None,
            response_message_signature: None,
        }
    }

    /// Set the upstream origin to forward to.
    pub fn upstream<U>(mut self, uri: U) -> Self
    where
        U: TryInto<Uri>,
        U::Error: std::fmt::Debug,
    {
        match uri.try_into() {
            Ok(u) => self.upstream = Some(u),
            Err(e) => BuilderError::set_once(
                &mut self.builder_error,
                BuilderError::invalid_url(format!("invalid forward upstream: {e:?}")),
            ),
        }
        self
    }

    /// Strip a path prefix before forwarding.
    pub fn strip_prefix(mut self, prefix: impl Into<String>) -> Self {
        self.strip_prefix = Some(prefix.into());
        self
    }

    /// Preserve the original Host header instead of rewriting it to the upstream.
    pub fn preserve_host(mut self) -> Self {
        self.preserve_host = true;
        self
    }

    /// Set a total timeout for the forwarded request.
    pub fn timeout(mut self, duration: Duration) -> Self {
        self.timeout = Some(duration);
        self
    }

    /// Set a timeout for establishing a forwarded upstream connection.
    pub fn connect_timeout(mut self, duration: Duration) -> Self {
        self.connect_timeout = Some(duration);
        self
    }

    /// Set a timeout for receiving response headers from the upstream.
    pub fn first_byte_timeout(mut self, duration: Duration) -> Self {
        self.first_byte_timeout = Some(duration);
        self
    }

    /// Set a timeout for gaps while streaming the request body upstream.
    pub fn write_timeout(mut self, duration: Duration) -> Self {
        self.write_timeout = Some(duration);
        self
    }

    /// Set a timeout for gaps while streaming the response body downstream.
    pub fn read_timeout(mut self, duration: Duration) -> Self {
        self.read_timeout = Some(duration);
        self
    }

    /// Disable automatic HTTP Message Signatures for this forwarded request.
    pub fn without_message_signature(mut self) -> Self {
        self.sign_final_request = false;
        self
    }

    /// Set the downstream request URI used for related-request response signatures.
    ///
    /// Forwarded origin-form requests do not carry a scheme or authority. Set this
    /// to the original full downstream URI when a response signature covers
    /// related-request `@scheme`, `@authority`, or `@target-uri` components.
    /// The path and query must match the incoming request target.
    pub fn downstream_target_uri<U>(mut self, uri: U) -> Self
    where
        U: TryInto<Uri>,
        U::Error: std::fmt::Debug,
    {
        match uri.try_into() {
            Ok(u) if u.scheme().is_some() && u.authority().is_some() => {
                self.downstream_target_uri = Some(u);
            }
            Ok(_) => BuilderError::set_once(
                &mut self.builder_error,
                BuilderError::invalid_url(
                    "forward: downstream target URI must include scheme and authority",
                ),
            ),
            Err(e) => BuilderError::set_once(
                &mut self.builder_error,
                BuilderError::invalid_url(format!("invalid downstream target URI: {e:?}")),
            ),
        }
        self
    }

    /// Add a header to the upstream request.
    pub fn header(mut self, name: impl Into<HeaderName>, value: impl Into<HeaderValue>) -> Self {
        self.extra_headers.insert(name.into(), value.into());
        self
    }

    /// Forward (copy) a named header from the incoming request to the upstream.
    pub fn forward_header(mut self, name: impl Into<HeaderName>) -> Self {
        self.forward_headers.push(name.into());
        self
    }

    /// Remove a header before forwarding to the upstream.
    pub fn remove_header(mut self, name: impl Into<HeaderName>) -> Self {
        self.remove_headers.push(name.into());
        self
    }

    /// Mutate the request parts just before sending to the upstream.
    pub fn on_request(mut self, f: impl FnOnce(&mut http::request::Parts) + 'static) -> Self {
        self.on_request = Some(Box::new(f));
        self
    }

    /// Mutate the response before returning to the caller.
    pub fn on_response(mut self, f: impl FnOnce(&mut Response) + 'static) -> Self {
        self.on_response = Some(Box::new(f));
        self
    }

    /// Sign the downstream response returned by this forward operation.
    ///
    /// The signature is generated after upstream response hop-by-hop headers are
    /// stripped and after [`on_response`](Self::on_response) runs, so user
    /// response mutations are covered. Related-request components (`;req`) use
    /// the incoming request as received by this builder, not the rewritten
    /// upstream request.
    pub fn response_message_signature(
        mut self,
        config: MessageSignatureConfig,
        signer: impl MessageSignatureSigner,
    ) -> Self {
        self.response_message_signature =
            Some(AutomaticMessageSignature::new(config, Arc::new(signer)));
        self
    }

    /// Generate a SHA-256 `Content-Digest` header for the downstream response.
    ///
    /// The response body is buffered up to `max_bytes` after upstream response
    /// hop-by-hop cleanup and after [`on_response`](Self::on_response) runs. If
    /// the response already has `Content-Digest`, it is preserved and the body is
    /// not buffered. When combined with response message signing, digest
    /// generation runs before signing so `content-digest` can be covered.
    /// Responses that cannot carry content, such as `HEAD`, `204`, `205`, and
    /// `304`, are not assigned synthesized digest fields.
    pub fn response_content_digest(mut self, max_bytes: usize) -> Self {
        self.response_content_digest_max_bytes = Some(max_bytes);
        self
    }

    /// Sign the downstream response with an async signer on local runtimes.
    ///
    /// The returned signing future does not need to be [`Send`].
    pub fn response_message_signature_async_local(
        mut self,
        config: MessageSignatureConfig,
        signer: impl MessageSignatureLocalAsyncSigner,
    ) -> Self {
        self.response_message_signature = Some(AutomaticMessageSignature::new_async_local(
            config,
            Arc::new(signer),
        ));
        self
    }

    /// Marks this as an HTTP/1.1 upgrade request.
    ///
    /// The request must still contain valid `Connection: upgrade` and
    /// `Upgrade` fields; this method cannot infer a stripped protocol value.
    pub fn upgrade(mut self) -> Self {
        self.force_h1_upgrade = true;
        self.forward_headers.push(http::header::CONNECTION);
        self.forward_headers.push(http::header::UPGRADE);
        self
    }

    /// Force HTTP/2 prior knowledge (h2c) on this forward.
    pub fn h2c(mut self) -> Self {
        self.protocol_hint = ProtocolHint::H2c;
        self
    }

    /// Probe h2c, fall back to H1; result cached per-authority.
    pub fn adaptive_h2c(mut self) -> Self {
        self.protocol_hint = ProtocolHint::AdaptiveH2c;
        self
    }

    /// Execute the forwarded request.
    pub async fn send(mut self) -> Result<Response<crate::body::ResponseBodyLocal>, Error> {
        if let Some(error) = self.builder_error.take() {
            return Err(error.into_error());
        }
        let (mut parts, body) = self.request.into_parts();
        let downstream_connect_protocol = capture_downstream_connect_protocol(&mut parts)?;
        let downstream_version = parts.version;
        let downstream_method = parts.method.clone();
        let downstream_h1_upgrade_offer = super::hop_by_hop::h1_upgrade_offer(&parts.headers);
        let downstream_accepts_trailers = super::hop_by_hop::downstream_accepts_response_trailers(
            downstream_version,
            &parts.headers,
        );
        let response_finalization_enabled = self.response_message_signature.is_some()
            || self.response_content_digest_max_bytes.is_some();

        let response_related_request = prepare_forward_response_related_request(
            self.response_message_signature.as_ref(),
            self.downstream_target_uri.as_ref(),
            &parts,
        )?;
        if self.force_h1_upgrade || is_h1_upgrade_request(&parts.headers) {
            self.forward_headers.push(http::header::CONNECTION);
            self.forward_headers.push(http::header::UPGRADE);
            if super::hop_by_hop::is_h2c_upgrade(&parts.headers) {
                self.forward_headers
                    .push(HeaderName::from_static("http2-settings"));
            }
        }

        let upstream = self
            .upstream
            .ok_or_else(|| Error::InvalidUrl("forward: no upstream configured".into()))?;
        let rewritten = rewrite_for_upstream(
            &mut parts,
            ForwardRewrite {
                upstream: &upstream,
                strip_prefix: self.strip_prefix.as_deref(),
                preserve_host: self.preserve_host,
                forward_headers: &self.forward_headers,
                extra_headers: &self.extra_headers,
                remove_headers: &self.remove_headers,
            },
        )?;
        let version_before_hook = parts.version;
        if let Some(hook) = self.on_request {
            hook(&mut parts);
        }
        let version_changed_by_hook = parts.version != version_before_hook;
        if response_finalization_enabled {
            reject_response_finalization_for_tunnel_or_upgrade(&parts, self.force_h1_upgrade)?;
        }

        let plan = ForwardDispatchPlan::finalize(
            &mut parts,
            &rewritten.uri,
            &rewritten.inbound_target,
            &rewritten.trailer_policy,
            self.protocol_hint,
            self.force_h1_upgrade,
            downstream_h1_upgrade_offer,
            version_changed_by_hook,
            false,
            true,
            downstream_connect_protocol.as_deref(),
            downstream_version,
            &downstream_method,
            downstream_accepts_trailers,
            self.preserve_host,
        )?;
        plan.apply(&mut parts, body.is_end_stream())?;
        super::validate_forward_content_length(&mut parts.headers, body.size_hint())?;
        let full_uri = plan.full_uri().clone();
        let response_header_policy = plan.response_header_policy();

        let body_replayability = BodyReplayability::for_forwarded_body(&body);
        let request_trailer_policy = plan.request_trailer_policy();
        let write_timeout = self.write_timeout;

        let boxed_body: RequestBodyLocal = Box::pin(super::TrailerSanitizedBody::new(
            body,
            request_trailer_policy,
        ));
        let boxed_body: RequestBodyLocal = match write_timeout {
            Some(duration) => Box::pin(crate::timeout::WriteTimeoutBody::<_, R>::new(
                boxed_body, duration,
            )),
            None => boxed_body,
        };

        let mut request = http::Request::from_parts(parts, boxed_body);
        if body_replayability == BodyReplayability::OneShot {
            request.extensions_mut().insert(FreshConnectionRequired);
        }
        self.client.core.apply_automatic_content_digest(
            self.client.core.automatic_content_digest,
            request.headers_mut(),
            &crate::digest_fields::ContentDigestBody::Unavailable,
        )?;
        let client = self.client;
        let protocol_hint = plan.protocol_hint();
        let connect_timeout = self.connect_timeout;
        let first_byte_timeout = self.first_byte_timeout;
        let read_timeout = self.read_timeout;
        let sign_final_request = self.sign_final_request;
        let response_message_signature = self.response_message_signature;
        let response_signing_enabled = response_message_signature.is_some();
        let response_content_digest_max_bytes = self.response_content_digest_max_bytes;
        let response_processing_enabled =
            response_signing_enabled || response_content_digest_max_bytes.is_some();
        let response_request_method = request.method().clone();
        let on_response = self.on_response;
        let timeout = self.timeout.or(client.core.timeout);
        let send_fut = Box::pin(async move {
            super::project_deferred_headers_for_signature(&mut request);
            if sign_final_request
                && let Some(signature) = client
                    .core
                    .prepare_final_request_signature(&full_uri, &mut request)?
            {
                let signature_headers = signature.sign_local().await?;
                signature_headers.insert_into(request.headers_mut())?;
            }
            let mut resp = client
                .execute_single_local(
                    request,
                    &full_uri,
                    None,
                    connect_timeout,
                    write_timeout,
                    first_byte_timeout,
                    None,
                    protocol_hint,
                    sign_final_request,
                    body_replayability,
                )
                .await?;

            let upstream_response_version = resp.version();
            let upstream_response_status = resp.status();
            super::hop_by_hop::validate_inbound_response_headers(
                upstream_response_version,
                &response_request_method,
                upstream_response_status,
                resp.headers_mut(),
            )?;

            if response_processing_enabled && resp.status() == http::StatusCode::SWITCHING_PROTOCOLS
            {
                return Err(Error::Unsupported(
                    "automatic forward response finalization does not support HTTP/1.1 switching protocols responses"
                        .to_owned(),
                ));
            }

            resp.set_version(response_header_policy.downstream_version());

            let trailer_policy = response_header_policy.sanitize(
                resp.status(),
                resp.headers_mut(),
                Some(upstream_response_version),
            )?;
            let mut resp =
                sanitize_forward_response_body::<R>(resp, trailer_policy, read_timeout).await?;

            if let Some(hook) = on_response {
                let response_status = resp.status();
                let upgrade_selection =
                    response_header_policy.upgrade_selection(response_status, resp.headers())?;
                resp.run_hook_preserving_dispatch_extensions(hook);
                resp.set_version(response_header_policy.downstream_version());
                response_header_policy
                    .validate_response_hook_status(response_status, resp.status())?;
                response_header_policy.validate_preserved_upgrade_selection(
                    resp.status(),
                    resp.headers(),
                    upgrade_selection.as_ref(),
                )?;
                let trailer_policy =
                    response_header_policy.sanitize(resp.status(), resp.headers_mut(), None)?;
                resp =
                    sanitize_forward_response_body::<R>(resp, trailer_policy, read_timeout).await?;
            }

            let mut resp = apply_forward_response_content_digest::<R>(
                resp,
                response_content_digest_max_bytes,
                &response_request_method,
                read_timeout,
            )
            .await?;

            if let Some(signature) = response_message_signature {
                let status = resp.status();
                let prepared = if let Some(related_request) = response_related_request.as_ref() {
                    signature.prepare_request_response_headers(
                        &related_request.method,
                        &related_request.target_uri,
                        &related_request.request_target,
                        &related_request.headers,
                        status,
                        resp.headers_mut(),
                    )?
                } else {
                    signature.prepare_response_headers(status, resp.headers_mut())?
                };
                let signature_headers = prepared.sign_local().await?;
                signature_headers.insert_into(resp.headers_mut())?;
            }

            Ok(resp)
        });

        let result = if let Some(duration) = timeout {
            crate::timeout::Timeout::WithTimeout {
                future: send_fut,
                sleep: R::sleep(duration),
            }
            .await
        } else {
            send_fut.await
        };
        let resp = result?;

        if let Some(duration) = read_timeout {
            Ok(resp.into_local_with_read_timeout::<R>(duration))
        } else {
            Ok(resp.into_local())
        }
    }
}

#[cfg(all(test, feature = "compio"))]
mod tests {
    use super::*;
    use crate::client::HttpEngineLocal;
    use crate::runtime::compio_rt::{CompioRuntime, TcpConnector};

    fn test_client() -> HttpEngineLocal<CompioRuntime, TcpConnector> {
        HttpEngineLocal::new()
    }

    fn dummy_request(path: &str) -> http::Request<http_body_util::Empty<Bytes>> {
        http::Request::builder()
            .uri(path)
            .header(http::header::HOST, "downstream.test")
            .body(http_body_util::Empty::new())
            .unwrap()
    }

    #[test]
    fn strip_prefix_sets_field() {
        let client = test_client();
        let req = dummy_request("/api/users");
        let builder = ForwardBuilderLocal::new(&client, req).strip_prefix("/api");
        assert_eq!(builder.strip_prefix.as_deref(), Some("/api"));
    }

    #[test]
    fn preserve_host_sets_flag() {
        let client = test_client();
        let req = dummy_request("/path");
        let builder = ForwardBuilderLocal::new(&client, req).preserve_host();
        assert!(builder.preserve_host);
    }

    #[test]
    fn timeout_sets_duration() {
        let client = test_client();
        let req = dummy_request("/path");
        let builder = ForwardBuilderLocal::new(&client, req).timeout(Duration::from_secs(5));
        assert_eq!(builder.timeout, Some(Duration::from_secs(5)));
    }

    #[test]
    fn phase_timeouts_set_fields() {
        let client = test_client();
        let req = dummy_request("/path");
        let builder = ForwardBuilderLocal::new(&client, req)
            .connect_timeout(Duration::from_secs(1))
            .first_byte_timeout(Duration::from_secs(2))
            .write_timeout(Duration::from_secs(3))
            .read_timeout(Duration::from_secs(4));
        assert_eq!(builder.connect_timeout, Some(Duration::from_secs(1)));
        assert_eq!(builder.first_byte_timeout, Some(Duration::from_secs(2)));
        assert_eq!(builder.write_timeout, Some(Duration::from_secs(3)));
        assert_eq!(builder.read_timeout, Some(Duration::from_secs(4)));
    }

    #[test]
    fn without_message_signature_clears_signing_flag() {
        let client = test_client();
        let req = dummy_request("/path");
        let builder = ForwardBuilderLocal::new(&client, req).without_message_signature();
        assert!(!builder.sign_final_request);
    }

    #[test]
    fn header_adds_to_extra_headers() {
        let client = test_client();
        let req = dummy_request("/path");
        let builder = ForwardBuilderLocal::new(&client, req)
            .header(http::header::ACCEPT, HeaderValue::from_static("text/html"));
        assert_eq!(builder.extra_headers.get("accept").unwrap(), "text/html");
    }

    #[test]
    fn forward_header_adds_to_list() {
        let client = test_client();
        let req = dummy_request("/path");
        let builder =
            ForwardBuilderLocal::new(&client, req).forward_header(http::header::AUTHORIZATION);
        assert_eq!(builder.forward_headers.len(), 1);
        assert_eq!(builder.forward_headers[0], http::header::AUTHORIZATION);
    }

    #[test]
    fn remove_header_adds_to_list() {
        let client = test_client();
        let req = dummy_request("/path");
        let builder = ForwardBuilderLocal::new(&client, req).remove_header(http::header::COOKIE);
        assert_eq!(builder.remove_headers.len(), 1);
        assert_eq!(builder.remove_headers[0], http::header::COOKIE);
    }

    #[test]
    fn upstream_sets_uri() {
        let client = test_client();
        let req = dummy_request("/path");
        let builder = ForwardBuilderLocal::new(&client, req).upstream("http://backend:8080");
        assert_eq!(
            builder.upstream.unwrap().to_string(),
            "http://backend:8080/"
        );
    }

    #[test]
    fn upgrade_pushes_connection_and_upgrade_headers() {
        let client = test_client();
        let req = dummy_request("/ws");
        let builder = ForwardBuilderLocal::new(&client, req).upgrade();
        assert!(builder.force_h1_upgrade);
        assert_eq!(builder.forward_headers.len(), 2);
        assert_eq!(builder.forward_headers[0], http::header::CONNECTION);
        assert_eq!(builder.forward_headers[1], http::header::UPGRADE);
    }

    #[test]
    fn on_request_hook_is_set() {
        let client = test_client();
        let req = dummy_request("/path");
        let builder = ForwardBuilderLocal::new(&client, req).on_request(|_parts| {});
        assert!(builder.on_request.is_some());
    }

    #[test]
    fn on_response_hook_is_set() {
        let client = test_client();
        let req = dummy_request("/path");
        let builder = ForwardBuilderLocal::new(&client, req).on_response(|_resp| {});
        assert!(builder.on_response.is_some());
    }

    #[test]
    fn chained_builder() {
        let client = test_client();
        let req = dummy_request("/api/users?page=1");
        let builder = ForwardBuilderLocal::new(&client, req)
            .upstream("http://backend:8080")
            .strip_prefix("/api")
            .preserve_host()
            .timeout(Duration::from_secs(30))
            .connect_timeout(Duration::from_secs(1))
            .first_byte_timeout(Duration::from_secs(2))
            .write_timeout(Duration::from_secs(3))
            .read_timeout(Duration::from_secs(4))
            .without_message_signature()
            .header(
                http::header::ACCEPT,
                HeaderValue::from_static("application/json"),
            )
            .forward_header(http::header::AUTHORIZATION)
            .remove_header(http::header::COOKIE);

        assert!(builder.upstream.is_some());
        assert_eq!(builder.strip_prefix.as_deref(), Some("/api"));
        assert!(builder.preserve_host);
        assert_eq!(builder.timeout, Some(Duration::from_secs(30)));
        assert_eq!(builder.connect_timeout, Some(Duration::from_secs(1)));
        assert_eq!(builder.first_byte_timeout, Some(Duration::from_secs(2)));
        assert_eq!(builder.write_timeout, Some(Duration::from_secs(3)));
        assert_eq!(builder.read_timeout, Some(Duration::from_secs(4)));
        assert!(!builder.sign_final_request);
        assert_eq!(builder.extra_headers.len(), 1);
        assert_eq!(builder.forward_headers.len(), 1);
        assert_eq!(builder.remove_headers.len(), 1);
    }

    #[test]
    fn send_without_upstream_returns_error() {
        let client = test_client();
        let req = dummy_request("/path");
        compio_runtime::Runtime::new().unwrap().block_on(async {
            let result = ForwardBuilderLocal::new(&client, req).send().await;
            assert!(result.is_err());
            match result.unwrap_err() {
                crate::error::Error::InvalidUrl(msg) => assert!(msg.contains("no upstream")),
                other => panic!("expected InvalidUrl, got: {other:?}"),
            }
        });
    }

    #[test]
    fn send_with_upstream_no_authority_returns_error() {
        let client = test_client();
        let req = dummy_request("/path");
        compio_runtime::Runtime::new().unwrap().block_on(async {
            let result = ForwardBuilderLocal::new(&client, req)
                .upstream("/just-a-path")
                .send()
                .await;
            match result.unwrap_err() {
                crate::error::Error::InvalidUrl(msg) => {
                    assert!(msg.contains("upstream has no authority"));
                }
                other => panic!("expected InvalidUrl, got: {other:?}"),
            }
        });
    }

    #[test]
    fn send_with_invalid_upstream_returns_recorded_error() {
        let client = test_client();
        let req = dummy_request("/path");
        compio_runtime::Runtime::new().unwrap().block_on(async {
            let result = ForwardBuilderLocal::new(&client, req)
                .upstream("http://bad host")
                .send()
                .await;
            match result.unwrap_err() {
                crate::error::Error::InvalidUrl(msg) => {
                    assert!(msg.contains("invalid forward upstream"));
                }
                other => panic!("expected InvalidUrl, got: {other:?}"),
            }
        });
    }
}
