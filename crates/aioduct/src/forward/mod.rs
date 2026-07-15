//! Request forwarding for proxy/gateway use cases.

pub(crate) mod dispatch_plan;
pub(crate) mod forward_local;
mod hop_by_hop;

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use http::Uri;
use http::header::{HeaderMap, HeaderName, HeaderValue, UPGRADE};
use http_body::Body;
use http_body_util::BodyExt;

use crate::body::RequestBodySend;
use crate::client::{BodyReplayability, FreshConnectionRequired, HttpEngineSend};
use crate::error::{BuilderError, Error};
use crate::message_signatures::{
    AutomaticMessageSignature, MessageSignatureAsyncSigner, MessageSignatureConfig,
    MessageSignatureSigner,
};
use crate::pool::ProtocolHint;
use crate::response::Response;
use crate::runtime::{ConnectorSend, RuntimePoll};

use dispatch_plan::{ForwardDispatchPlan, ForwardMode, ForwardRewrite, rewrite_for_upstream};

type RequestHook = Box<dyn FnOnce(&mut http::request::Parts) + Send>;
type ResponseHook = Box<dyn FnOnce(&mut Response) + Send>;

#[derive(Clone)]
pub(crate) struct ForwardResponseRelatedRequest {
    pub(crate) method: http::Method,
    pub(crate) target_uri: Uri,
    pub(crate) request_target: Uri,
    pub(crate) headers: HeaderMap,
}

pub(crate) fn is_h1_upgrade_request(headers: &HeaderMap) -> bool {
    headers.contains_key(UPGRADE)
        && headers.get_all(http::header::CONNECTION).iter().any(|v| {
            v.to_str().ok().is_some_and(|v| {
                v.split(',')
                    .any(|token| token.trim().eq_ignore_ascii_case("upgrade"))
            })
        })
}

pub(crate) fn reject_response_finalization_for_tunnel_or_upgrade(
    parts: &http::request::Parts,
    force_h1_upgrade: bool,
) -> Result<(), Error> {
    if parts.method == http::Method::CONNECT {
        return Err(Error::Unsupported(
            "automatic forward response finalization does not support forwarded CONNECT requests"
                .to_owned(),
        ));
    }
    if force_h1_upgrade || is_h1_upgrade_request(&parts.headers) {
        return Err(Error::Unsupported(
            "automatic forward response finalization does not support forwarded HTTP/1.1 upgrade requests"
                .to_owned(),
        ));
    }
    Ok(())
}

pub(crate) fn prepare_forward_response_related_request(
    signature: Option<&AutomaticMessageSignature>,
    downstream_target_uri: Option<&Uri>,
    parts: &http::request::Parts,
) -> Result<Option<ForwardResponseRelatedRequest>, Error> {
    let Some(signature) = signature else {
        return Ok(None);
    };

    let requirements = signature.forward_response_requirements()?;
    if requirements.has_trailer_components {
        return Err(Error::Unsupported(
            "automatic response message signing does not support trailer components".to_owned(),
        ));
    }

    if !requirements.has_related_request_components {
        return Ok(None);
    }

    let request_target = parts.uri.clone();
    let target_uri = downstream_target_uri
        .map(|target_uri| {
            ensure_downstream_target_matches_request(target_uri, &request_target)?;
            Ok::<_, Error>(target_uri.clone())
        })
        .transpose()?
        .unwrap_or_else(|| request_target.clone());

    if requirements.requires_full_downstream_target_uri && !is_full_uri(&target_uri) {
        return Err(Error::Unsupported(
            "automatic response message signing requires downstream_target_uri(...) or an absolute inbound URI for related-request @scheme, @authority, or @target-uri components"
                .to_owned(),
        ));
    }

    Ok(Some(ForwardResponseRelatedRequest {
        method: parts.method.clone(),
        target_uri,
        request_target,
        headers: parts.headers.clone(),
    }))
}

fn ensure_downstream_target_matches_request(
    downstream_target_uri: &Uri,
    request_target: &Uri,
) -> Result<(), Error> {
    let downstream_path_and_query = path_and_query_or_root(downstream_target_uri);
    let request_path_and_query = path_and_query_or_root(request_target);
    if downstream_path_and_query != request_path_and_query {
        return Err(Error::InvalidUrl(format!(
            "forward: downstream target URI path/query `{downstream_path_and_query}` does not match incoming request target `{request_path_and_query}`"
        )));
    }
    Ok(())
}

fn path_and_query_or_root(uri: &Uri) -> &str {
    uri.path_and_query().map(|pq| pq.as_str()).unwrap_or("/")
}

fn is_full_uri(uri: &Uri) -> bool {
    uri.scheme().is_some() && uri.authority().is_some()
}

pub(crate) async fn apply_forward_response_content_digest(
    resp: Response,
    max_bytes: Option<usize>,
    request_method: &http::Method,
) -> Result<Response, Error> {
    let Some(max_bytes) = max_bytes else {
        return Ok(resp);
    };
    if crate::digest_fields::has_content_digest(resp.headers()) {
        return Ok(resp);
    }
    if response_has_no_content(request_method, resp.status()) {
        return Ok(resp);
    }

    let (mut resp, body) = resp
        .into_buffered_with_limit(max_bytes, "forward response Content-Digest")
        .await?;
    crate::digest_fields::insert_sha256_content_digest(resp.headers_mut(), &body).map_err(
        |source| {
            Error::InvalidHeader(format!(
                "generated response Content-Digest header value is invalid: {source}"
            ))
        },
    )?;
    Ok(resp)
}

fn response_has_no_content(request_method: &http::Method, status: http::StatusCode) -> bool {
    *request_method == http::Method::HEAD
        || status.is_informational()
        || status == http::StatusCode::NO_CONTENT
        || status == http::StatusCode::RESET_CONTENT
        || status == http::StatusCode::NOT_MODIFIED
}

/// Builder for forwarding an incoming HTTP request on a `Send` runtime.
///
/// Created via [`HttpEngineSend::forward`]. Strips hop-by-hop headers, rewrites the URI
/// to target the upstream, and streams the body through without buffering.
/// Skips all client middleware (redirects, cookies, cache, decompression).
pub struct ForwardBuilderSend<'a, R: RuntimePoll, C: ConnectorSend, B> {
    client: &'a HttpEngineSend<R, C>,
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

impl<'a, R: RuntimePoll, C: ConnectorSend, B> ForwardBuilderSend<'a, R, C, B>
where
    B: Body<Data = Bytes> + Send + 'static,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    pub(crate) fn new(client: &'a HttpEngineSend<R, C>, request: http::Request<B>) -> Self {
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
    ///
    /// The incoming request's path (after optional prefix stripping) and query
    /// string are appended to this origin.
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
    ///
    /// For example, `.strip_prefix("/api")` rewrites `/api/users` → `/users`.
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
    ///
    /// The timer starts after the terminal request bytes reach the transport.
    /// Use [`Self::write_timeout`] or [`Self::timeout`] to bound a stalled
    /// request upload.
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
    ///
    /// Forwarding normally uses the client's message signature configuration.
    /// Host adapters that are proxying untrusted guest requests can disable it
    /// so guest-controlled requests cannot trigger host signing material.
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

    /// Force HTTP/2 prior knowledge (h2c) for this forward.
    ///
    /// Use this for gRPC upstreams over plaintext. The upstream must speak HTTP/2
    /// — this does NOT perform adaptive fallback.
    pub fn h2c(mut self) -> Self {
        self.protocol_hint = ProtocolHint::H2c;
        self
    }

    /// Try HTTP/2 prior knowledge; fall back to HTTP/1.1 if the upstream rejects it.
    ///
    /// The result is cached per-authority — subsequent requests skip the probe.
    pub fn adaptive_h2c(mut self) -> Self {
        self.protocol_hint = ProtocolHint::AdaptiveH2c;
        self
    }

    /// Add a header to the upstream request.
    pub fn header(mut self, name: impl Into<HeaderName>, value: impl Into<HeaderValue>) -> Self {
        self.extra_headers.insert(name.into(), value.into());
        self
    }

    /// Forward (copy) a named header from the incoming request to the upstream.
    ///
    /// If the header is not present on the incoming request, this is a no-op.
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
    ///
    /// This is the escape hatch for any transformation not covered by other
    /// builder methods.
    pub fn on_request(
        mut self,
        f: impl FnOnce(&mut http::request::Parts) + Send + 'static,
    ) -> Self {
        self.on_request = Some(Box::new(f));
        self
    }

    /// Mutate the response before returning to the caller.
    ///
    /// Use `resp.headers_mut()`, `resp.extensions_mut()`, etc.
    pub fn on_response(mut self, f: impl FnOnce(&mut Response) + Send + 'static) -> Self {
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

    /// Sign the downstream response with an async signer on send runtimes.
    ///
    /// The returned signing future must be [`Send`]. Use
    /// [`ForwardBuilderLocal::response_message_signature_async_local`](crate::ForwardBuilderLocal::response_message_signature_async_local)
    /// for local-runtime signing futures that are not `Send`.
    pub fn response_message_signature_async(
        mut self,
        config: MessageSignatureConfig,
        signer: impl MessageSignatureAsyncSigner,
    ) -> Self {
        self.response_message_signature = Some(AutomaticMessageSignature::new_async_send(
            config,
            Arc::new(signer),
        ));
        self
    }

    /// Marks this as an HTTP/1.1 upgrade request, preserving Connection and
    /// Upgrade headers through hop-by-hop stripping.
    ///
    /// Usually unnecessary because H1 upgrades are auto-detected from valid
    /// `Connection: upgrade` and `Upgrade` fields. This method does not invent
    /// a missing upgrade protocol; those fields must still be present.
    pub fn upgrade(mut self) -> Self {
        self.force_h1_upgrade = true;
        self.forward_headers.push(http::header::CONNECTION);
        self.forward_headers.push(http::header::UPGRADE);
        self
    }

    /// Execute the forwarded request.
    pub async fn send(mut self) -> Result<Response, Error> {
        if let Some(error) = self.builder_error.take() {
            return Err(error.into_error());
        }
        let (mut parts, body) = self.request.into_parts();
        let downstream_connect_protocol = capture_downstream_connect_protocol(&mut parts)?;
        let downstream_version = parts.version;
        let downstream_method = parts.method.clone();
        let downstream_h1_upgrade_offer = hop_by_hop::h1_upgrade_offer(&parts.headers);
        let downstream_accepts_trailers =
            hop_by_hop::downstream_accepts_response_trailers(downstream_version, &parts.headers);
        let response_finalization_enabled = self.response_message_signature.is_some()
            || self.response_content_digest_max_bytes.is_some();

        let response_related_request = prepare_forward_response_related_request(
            self.response_message_signature.as_ref(),
            self.downstream_target_uri.as_ref(),
            &parts,
        )?;
        // Preserve inbound upgrade fields across the initial hop-header cleanup.
        if self.force_h1_upgrade || is_h1_upgrade_request(&parts.headers) {
            self.forward_headers.push(http::header::CONNECTION);
            self.forward_headers.push(http::header::UPGRADE);
        }
        if !is_h1_upgrade && !is_h2_extended_connect && self.protocol_hint != ProtocolHint::H2c {
            parts.version = http::Version::HTTP_11;
        }

        let upstream = self
            .upstream
            .ok_or_else(|| Error::InvalidUrl("forward: no upstream configured".into()))?;
        let rewritten_uri = rewrite_for_upstream(
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

        #[cfg(all(feature = "http3", feature = "rustls"))]
        let allow_h3 = self.client.core.h3_endpoint.is_some();
        #[cfg(not(all(feature = "http3", feature = "rustls")))]
        let allow_h3 = false;

        let plan = ForwardDispatchPlan::finalize(
            &mut parts,
            &rewritten_uri,
            self.protocol_hint,
            self.force_h1_upgrade,
            downstream_h1_upgrade_offer,
            version_changed_by_hook,
            allow_h3,
            true,
            downstream_connect_protocol.as_deref(),
            downstream_version,
            &downstream_method,
            downstream_accepts_trailers,
            self.preserve_host,
        )?;
        plan.apply(&mut parts, body.is_end_stream())?;
        let full_uri = plan.full_uri().clone();
        let forward_mode = plan.mode();

        // Convert body to RequestBodySend.
        let connect_timeout = self.connect_timeout;
        let first_byte_timeout = self.first_byte_timeout;
        let write_timeout = self.write_timeout;
        let read_timeout = self.read_timeout;
        let sign_final_request = self.sign_final_request;

        let body_replayability = BodyReplayability::for_forwarded_body(&body);

        let mut boxed_body: RequestBodySend = body
            .map_frame(|frame| frame)
            .map_err(|e| {
                let boxed: Box<dyn std::error::Error + Send + Sync> = e.into();
                Error::Other(boxed)
            })
            .boxed_unsync();
        if let Some(duration) = write_timeout {
            let timeout_body = crate::timeout::WriteTimeoutBody::<_, R>::new(boxed_body, duration);
            boxed_body = timeout_body.map_err(|e| e).boxed_unsync();
        }

        let mut request = http::Request::from_parts(parts, boxed_body);
        if body_replayability == BodyReplayability::OneShot {
            request.extensions_mut().insert(FreshConnectionRequired);
        }
        self.client.core.apply_automatic_content_digest(
            self.client.core.automatic_content_digest,
            request.headers_mut(),
            &crate::digest_fields::ContentDigestBody::Unavailable,
        )?;
        // Sign and send via execute_single_with_hint (bypasses redirects,
        // cookies, cache, decompression). Keep async signing inside the same
        // timeout budget as the forwarded dispatch.
        let client = self.client;
        let protocol_hint = plan.protocol_hint();
        let response_message_signature = self.response_message_signature;
        let response_signing_enabled = response_message_signature.is_some();
        let response_content_digest_max_bytes = self.response_content_digest_max_bytes;
        let response_processing_enabled =
            response_signing_enabled || response_content_digest_max_bytes.is_some();
        let response_request_method = request.method().clone();
        let mut on_response = self.on_response;
        let on_response_before_signing = if response_processing_enabled {
            on_response.take()
        } else {
            None
        };
        let timeout = self.timeout.or(client.core.timeout);
        // Bound the stack footprint of concurrent broker handlers. The
        // forwarding lifecycle includes dispatch and response finalization,
        // but callers should retain only one pointer per in-flight request.
        let send_fut = Box::pin(async move {
            let mut resp = client
                .execute_single_with_hint_send(
                    request,
                    &full_uri,
                    protocol_hint,
                    None,
                    connect_timeout,
                    write_timeout,
                    first_byte_timeout,
                    None,
                    sign_final_request,
                    body_replayability,
                )
                .await?;

            if response_processing_enabled && resp.status() == http::StatusCode::SWITCHING_PROTOCOLS
            {
                return Err(Error::Unsupported(
                    "automatic forward response finalization does not support HTTP/1.1 switching protocols responses"
                        .to_owned(),
                ));
            }

            // Strip hop-by-hop from response (skip for upgrade responses).
            if resp.status() != http::StatusCode::SWITCHING_PROTOCOLS
                && forward_mode != ForwardMode::ExtendedConnect
            {
                let resp_headers = resp.headers_mut();
                hop_by_hop::strip_hop_by_hop(resp_headers);
            }

            if let Some(hook) = on_response_before_signing {
                hook(&mut resp);
            }

            if response_processing_enabled {
                hop_by_hop::strip_hop_by_hop(resp.headers_mut());
            }

            let mut resp = apply_forward_response_content_digest(
                resp,
                response_content_digest_max_bytes,
                &response_request_method,
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
                let signature_headers = prepared.sign_send().await?;
                signature_headers.insert_into(resp.headers_mut())?;
            }

            Ok(resp)
        });

        let mut resp = if let Some(duration) = timeout {
            crate::timeout::Timeout::WithTimeout {
                future: send_fut,
                sleep: R::sleep(duration),
            }
            .await?
        } else {
            send_fut.await?
        };

        if let Some(hook) = on_response {
            hook(&mut resp);
        }

        if let Some(duration) = read_timeout {
            resp = resp.apply_read_timeout::<R>(duration);
        }

        Ok(resp)
    }
}

#[cfg(all(test, feature = "tokio"))]
mod tests {
    use super::*;
    use crate::client::HttpEngineSend;
    use crate::runtime::tokio_rt::{TcpConnector, TokioRuntime};

    fn test_client() -> HttpEngineSend<TokioRuntime, TcpConnector> {
        HttpEngineSend::new()
    }

    fn dummy_request(path: &str) -> http::Request<http_body_util::Empty<Bytes>> {
        http::Request::builder()
            .uri(path)
            .body(http_body_util::Empty::new())
            .unwrap()
    }

    #[test]
    fn strip_prefix_sets_field() {
        let client = test_client();
        let req = dummy_request("/api/users");
        let builder = ForwardBuilderSend::new(&client, req).strip_prefix("/api");
        assert_eq!(builder.strip_prefix.as_deref(), Some("/api"));
    }

    #[test]
    fn preserve_host_sets_flag() {
        let client = test_client();
        let req = dummy_request("/path");
        let builder = ForwardBuilderSend::new(&client, req).preserve_host();
        assert!(builder.preserve_host);
    }

    #[test]
    fn timeout_sets_duration() {
        let client = test_client();
        let req = dummy_request("/path");
        let builder = ForwardBuilderSend::new(&client, req).timeout(Duration::from_secs(5));
        assert_eq!(builder.timeout, Some(Duration::from_secs(5)));
    }

    #[test]
    fn phase_timeouts_set_fields() {
        let client = test_client();
        let req = dummy_request("/path");
        let builder = ForwardBuilderSend::new(&client, req)
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
        let builder = ForwardBuilderSend::new(&client, req).without_message_signature();
        assert!(!builder.sign_final_request);
    }

    #[test]
    fn header_adds_to_extra_headers() {
        let client = test_client();
        let req = dummy_request("/path");
        let builder = ForwardBuilderSend::new(&client, req)
            .header(http::header::ACCEPT, HeaderValue::from_static("text/html"));
        assert_eq!(builder.extra_headers.get("accept").unwrap(), "text/html");
    }

    #[test]
    fn forward_header_adds_to_list() {
        let client = test_client();
        let req = dummy_request("/path");
        let builder =
            ForwardBuilderSend::new(&client, req).forward_header(http::header::AUTHORIZATION);
        assert_eq!(builder.forward_headers.len(), 1);
        assert_eq!(builder.forward_headers[0], http::header::AUTHORIZATION);
    }

    #[test]
    fn remove_header_adds_to_list() {
        let client = test_client();
        let req = dummy_request("/path");
        let builder = ForwardBuilderSend::new(&client, req).remove_header(http::header::COOKIE);
        assert_eq!(builder.remove_headers.len(), 1);
        assert_eq!(builder.remove_headers[0], http::header::COOKIE);
    }

    #[test]
    fn upstream_sets_uri() {
        let client = test_client();
        let req = dummy_request("/path");
        let builder = ForwardBuilderSend::new(&client, req).upstream("http://backend:8080");
        assert_eq!(
            builder.upstream.unwrap().to_string(),
            "http://backend:8080/"
        );
    }

    #[test]
    fn h2c_sets_protocol_hint() {
        let client = test_client();
        let req = dummy_request("/path");
        let builder = ForwardBuilderSend::new(&client, req).h2c();
        assert_eq!(builder.protocol_hint, ProtocolHint::H2c);
    }

    #[test]
    fn adaptive_h2c_sets_protocol_hint() {
        let client = test_client();
        let req = dummy_request("/path");
        let builder = ForwardBuilderSend::new(&client, req).adaptive_h2c();
        assert_eq!(builder.protocol_hint, ProtocolHint::AdaptiveH2c);
    }

    #[test]
    fn upgrade_pushes_connection_and_upgrade_headers() {
        let client = test_client();
        let req = dummy_request("/ws");
        let builder = ForwardBuilderSend::new(&client, req).upgrade();
        assert!(builder.force_h1_upgrade);
        assert_eq!(builder.forward_headers.len(), 2);
        assert_eq!(builder.forward_headers[0], http::header::CONNECTION);
        assert_eq!(builder.forward_headers[1], http::header::UPGRADE);
    }

    #[test]
    fn h1_upgrade_detection_requires_connection_upgrade_token() {
        let mut headers = HeaderMap::new();
        headers.insert(http::header::UPGRADE, HeaderValue::from_static("h2c"));
        assert!(!is_h1_upgrade_request(&headers));

        headers.insert(
            http::header::CONNECTION,
            HeaderValue::from_static("keep-alive"),
        );
        assert!(!is_h1_upgrade_request(&headers));

        headers.insert(
            http::header::CONNECTION,
            HeaderValue::from_static("keep-alive, Upgrade"),
        );
        assert!(is_h1_upgrade_request(&headers));
    }

    #[test]
    fn on_request_hook_is_set() {
        let client = test_client();
        let req = dummy_request("/path");
        let builder = ForwardBuilderSend::new(&client, req).on_request(|_parts| {});
        assert!(builder.on_request.is_some());
    }

    #[test]
    fn on_response_hook_is_set() {
        let client = test_client();
        let req = dummy_request("/path");
        let builder = ForwardBuilderSend::new(&client, req).on_response(|_resp| {});
        assert!(builder.on_response.is_some());
    }

    #[test]
    fn chained_builder() {
        let client = test_client();
        let req = dummy_request("/api/users?page=1");
        let builder = ForwardBuilderSend::new(&client, req)
            .upstream("http://backend:8080")
            .strip_prefix("/api")
            .preserve_host()
            .timeout(Duration::from_secs(30))
            .connect_timeout(Duration::from_secs(1))
            .first_byte_timeout(Duration::from_secs(2))
            .write_timeout(Duration::from_secs(3))
            .read_timeout(Duration::from_secs(4))
            .without_message_signature()
            .h2c()
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
        assert_eq!(builder.protocol_hint, ProtocolHint::H2c);
        assert_eq!(builder.extra_headers.len(), 1);
        assert_eq!(builder.forward_headers.len(), 1);
        assert_eq!(builder.remove_headers.len(), 1);
    }

    #[tokio::test]
    async fn send_without_upstream_returns_error() {
        let client = test_client();
        let req = dummy_request("/path");
        let result = ForwardBuilderSend::new(&client, req).send().await;
        assert!(result.is_err());
        match result.unwrap_err() {
            Error::InvalidUrl(msg) => assert!(msg.contains("no upstream")),
            other => panic!("expected InvalidUrl, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn send_with_upstream_no_authority_returns_error() {
        let client = test_client();
        let req = dummy_request("/path");
        let result = ForwardBuilderSend::new(&client, req)
            .upstream("/just-a-path")
            .send()
            .await;
        match result.unwrap_err() {
            Error::InvalidUrl(msg) => assert!(msg.contains("upstream has no authority")),
            other => panic!("expected InvalidUrl, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn send_with_invalid_upstream_returns_recorded_error() {
        let client = test_client();
        let req = dummy_request("/path");
        let result = ForwardBuilderSend::new(&client, req)
            .upstream("http://bad host")
            .send()
            .await;
        match result.unwrap_err() {
            Error::InvalidUrl(msg) => assert!(msg.contains("invalid forward upstream")),
            other => panic!("expected InvalidUrl, got: {other:?}"),
        }
    }
}
