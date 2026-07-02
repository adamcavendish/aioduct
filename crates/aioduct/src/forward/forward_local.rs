use std::sync::Arc;
use std::time::Duration;

use crate::body::RequestBodyLocal;
use crate::client::HttpEngineLocal;
use crate::error::{BuilderError, Error};
use crate::message_signatures::{
    AutomaticMessageSignature, MessageSignatureConfig, MessageSignatureLocalAsyncSigner,
    MessageSignatureSigner,
};
use crate::pool::ProtocolHint;
use crate::response::Response;
use crate::runtime::{ConnectorLocal, RuntimeLocal};
use bytes::Bytes;
use http::header::{HOST, HeaderMap, HeaderName, HeaderValue};
use http::uri::{Parts as UriParts, PathAndQuery, Scheme, Uri};
use http_body::Body;
use http_body_util::BodyExt;

use super::{
    apply_forward_response_content_digest, hop_by_hop, is_h1_upgrade_request,
    prepare_forward_response_related_request, reject_response_finalization_for_tunnel_or_upgrade,
};

type RequestHook = Box<dyn FnOnce(&mut http::request::Parts)>;
type ResponseHook = Box<dyn FnOnce(&mut Response)>;

/// Builder for forwarding an incoming HTTP request on a `!Send` runtime.
///
/// Created via [`HttpEngineLocal::forward_local`]. Mirrors [`super::ForwardBuilder`]
/// for completion-based runtimes.
pub struct ForwardBuilderLocal<'a, R: RuntimeLocal, C: ConnectorLocal + Clone, B> {
    client: &'a HttpEngineLocal<R, C>,
    request: http::Request<B>,
    upstream: Option<Uri>,
    strip_prefix: Option<String>,
    preserve_host: bool,
    timeout: Option<Duration>,
    extra_headers: HeaderMap,
    remove_headers: Vec<HeaderName>,
    forward_headers: Vec<HeaderName>,
    protocol_hint: ProtocolHint,
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
            extra_headers: HeaderMap::new(),
            remove_headers: Vec::new(),
            forward_headers: Vec::new(),
            protocol_hint,
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
        let response_finalization_enabled = self.response_message_signature.is_some()
            || self.response_content_digest_max_bytes.is_some();

        let response_related_request = prepare_forward_response_related_request(
            self.response_message_signature.as_ref(),
            self.downstream_target_uri.as_ref(),
            self.force_h1_upgrade,
            &parts,
        )?;
        if response_finalization_enabled {
            reject_response_finalization_for_tunnel_or_upgrade(&parts, self.force_h1_upgrade)?;
        }

        let is_h1_upgrade = self.force_h1_upgrade || is_h1_upgrade_request(&parts.headers);

        if is_h1_upgrade {
            self.forward_headers.push(http::header::CONNECTION);
            self.forward_headers.push(http::header::UPGRADE);
            parts.version = http::Version::HTTP_11;
        }

        if parts.method == http::Method::CONNECT
            && parts.extensions.get::<crate::Protocol>().is_some()
        {
            parts.version = http::Version::HTTP_2;
        }
        if self.protocol_hint == ProtocolHint::H2c {
            parts.version = http::Version::HTTP_2;
        }

        let forwarded_values: Vec<(HeaderName, HeaderValue)> = self
            .forward_headers
            .iter()
            .filter_map(|name| parts.headers.get(name).map(|v| (name.clone(), v.clone())))
            .collect();

        hop_by_hop::strip_hop_by_hop(&mut parts.headers);

        let upstream = self
            .upstream
            .ok_or_else(|| Error::InvalidUrl("forward: no upstream configured".into()))?;

        let upstream_scheme = upstream.scheme().cloned().unwrap_or(Scheme::HTTP);
        let upstream_authority = upstream
            .authority()
            .cloned()
            .ok_or_else(|| Error::InvalidUrl("forward: upstream has no authority".into()))?;

        let original_path = parts.uri.path();
        let path_after_strip = match &self.strip_prefix {
            Some(prefix) => {
                let stripped = original_path
                    .strip_prefix(prefix.as_str())
                    .unwrap_or(original_path);
                if stripped.is_empty() || !stripped.starts_with('/') {
                    format!("/{stripped}")
                } else {
                    stripped.to_owned()
                }
            }
            None => original_path.to_owned(),
        };

        let upstream_base = upstream.path().trim_end_matches('/');
        let combined_path = if upstream_base.is_empty() {
            path_after_strip
        } else {
            format!("{upstream_base}{path_after_strip}")
        };

        let path_and_query = if let Some(query) = parts.uri.query() {
            format!("{combined_path}?{query}")
        } else {
            combined_path
        };

        let pq: PathAndQuery = path_and_query
            .parse()
            .map_err(|e| Error::InvalidUrl(format!("forward: invalid path: {e}")))?;

        let mut uri_parts = UriParts::default();
        uri_parts.scheme = Some(upstream_scheme);
        uri_parts.authority = Some(upstream_authority.clone());
        uri_parts.path_and_query = Some(pq);
        let full_uri =
            Uri::from_parts(uri_parts).map_err(|e| Error::InvalidUrl(format!("forward: {e}")))?;

        if !self.preserve_host {
            parts.headers.remove(HOST);
            if let Ok(hv) = upstream_authority.as_str().parse::<HeaderValue>() {
                parts.headers.insert(HOST, hv);
            }
        }

        for (name, value) in forwarded_values {
            parts.headers.insert(name, value);
        }

        for (name, value) in &self.extra_headers {
            parts.headers.insert(name, value.clone());
        }

        for name in &self.remove_headers {
            parts.headers.remove(name);
        }

        if let Some(hook) = self.on_request {
            hook(&mut parts);
        }

        if response_finalization_enabled {
            reject_response_finalization_for_tunnel_or_upgrade(&parts, self.force_h1_upgrade)?;
        }

        // H2 extended CONNECT uses absolute URI.
        // RFC 7540 §8.3: ordinary CONNECT over h2c uses authority form.
        // Other h2c requests use absolute URI.
        // AdaptiveH2c uses path-only form because the dispatch layer may fall back
        // to H1, and absolute-form URIs confuse many origin servers.
        let is_h2_extended_connect = parts.method == http::Method::CONNECT
            && parts.extensions.get::<crate::Protocol>().is_some();
        if is_h2_extended_connect {
            parts.uri = full_uri.clone();
        } else if self.protocol_hint == ProtocolHint::H2c && parts.method == http::Method::CONNECT {
            parts.uri = upstream_authority
                .as_str()
                .parse()
                .map_err(|e| Error::Other(Box::new(e)))?;
        } else if self.protocol_hint == ProtocolHint::H2c {
            parts.uri = full_uri.clone();
        } else {
            let request_uri: Uri = full_uri
                .path_and_query()
                .map(|pq| pq.as_str())
                .unwrap_or("/")
                .parse()
                .map_err(|e| Error::Other(Box::new(e)))?;
            parts.uri = request_uri;
        }

        let boxed_body: RequestBodyLocal = Box::pin(body.map_err(|e| {
            let boxed: Box<dyn std::error::Error + Send + Sync> = e.into();
            Error::Other(boxed)
        }));

        let mut request = http::Request::from_parts(parts, boxed_body);
        self.client.core.apply_automatic_content_digest(
            self.client.core.automatic_content_digest,
            request.headers_mut(),
            &crate::digest_fields::ContentDigestBody::Unavailable,
        )?;
        let client = self.client;
        let protocol_hint = self.protocol_hint;
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
        let send_fut = async move {
            if let Some(signature) = client
                .core
                .prepare_final_request_signature(&full_uri, &mut request)?
            {
                let signature_headers = signature.sign_local().await?;
                signature_headers.insert_into(request.headers_mut())?;
            }

            let mut resp = client
                .execute_single_local(request, &full_uri, None, None, None, None, protocol_hint)
                .await?;

            if response_processing_enabled && resp.status() == http::StatusCode::SWITCHING_PROTOCOLS
            {
                return Err(Error::Unsupported(
                    "automatic forward response finalization does not support HTTP/1.1 switching protocols responses"
                        .to_owned(),
                ));
            }

            if resp.status() != http::StatusCode::SWITCHING_PROTOCOLS && !is_h1_upgrade {
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
                let signature_headers = prepared.sign_local().await?;
                signature_headers.insert_into(resp.headers_mut())?;
            }

            Ok(resp)
        };

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

        Ok(resp.into_local())
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
