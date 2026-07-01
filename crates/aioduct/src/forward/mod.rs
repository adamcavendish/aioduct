//! Request forwarding for proxy/gateway use cases.

pub(crate) mod forward_local;
mod hop_by_hop;

use std::time::Duration;

use bytes::Bytes;
use http::header::{HOST, HeaderMap, HeaderName, HeaderValue};
use http::uri::{Parts as UriParts, PathAndQuery, Scheme, Uri};
use http_body::Body;
use http_body_util::BodyExt;

use crate::body::RequestBodySend;
use crate::client::HttpEngineSend;
use crate::error::{BuilderError, Error};
use crate::pool::ProtocolHint;
use crate::response::Response;
use crate::runtime::{ConnectorSend, RuntimePoll};

type RequestHook = Box<dyn FnOnce(&mut http::request::Parts) + Send>;
type ResponseHook = Box<dyn FnOnce(&mut Response) + Send>;

/// Builder for forwarding an incoming HTTP request to an upstream server.
///
/// Created via [`HttpEngineSend::forward`]. Strips hop-by-hop headers, rewrites the URI
/// to target the upstream, and streams the body through without buffering.
/// Skips all client middleware (redirects, cookies, cache, decompression).
pub struct ForwardBuilder<'a, R: RuntimePoll, C: ConnectorSend, B> {
    client: &'a HttpEngineSend<R, C>,
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
}

impl<'a, R: RuntimePoll, C: ConnectorSend, B> ForwardBuilder<'a, R, C, B>
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
            extra_headers: HeaderMap::new(),
            remove_headers: Vec::new(),
            forward_headers: Vec::new(),
            protocol_hint,
            builder_error: None,
            on_request: None,
            on_response: None,
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

    /// Marks this as an HTTP/1.1 upgrade request, preserving Connection and
    /// Upgrade headers through hop-by-hop stripping.
    ///
    /// Usually unnecessary — H1 upgrades are auto-detected from headers.
    /// Use this if the framework stripped those headers before passing the
    /// request to you.
    pub fn upgrade(mut self) -> Self {
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

        // Detect upgrade requests
        let is_h1_upgrade = parts
            .headers
            .get(http::header::CONNECTION)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.to_ascii_lowercase().contains("upgrade"));

        let is_h2_extended_connect = parts.method == http::Method::CONNECT
            && parts.extensions.get::<hyper::ext::Protocol>().is_some();

        // Auto-preserve upgrade headers and force correct HTTP version
        if is_h1_upgrade {
            self.forward_headers.push(http::header::CONNECTION);
            self.forward_headers.push(http::header::UPGRADE);
            parts.version = http::Version::HTTP_11;
        }
        if is_h2_extended_connect {
            parts.version = http::Version::HTTP_2;
        }
        if self.protocol_hint == ProtocolHint::H2c {
            parts.version = http::Version::HTTP_2;
        }

        // Save headers that were explicitly requested to be forwarded, before
        // hop-by-hop stripping might remove them.
        let forwarded_values: Vec<(HeaderName, HeaderValue)> = self
            .forward_headers
            .iter()
            .filter_map(|name| parts.headers.get(name).map(|v| (name.clone(), v.clone())))
            .collect();

        // 1. Strip hop-by-hop from incoming request headers
        hop_by_hop::strip_hop_by_hop(&mut parts.headers);

        // 2. Build the upstream URI
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

        // Append upstream base path if present
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

        // 3. Set Host header
        if !self.preserve_host {
            parts.headers.remove(HOST);
            if let Ok(hv) = upstream_authority.as_str().parse::<HeaderValue>() {
                parts.headers.insert(HOST, hv);
            }
        }

        // 4. Re-insert explicitly forwarded headers (may have been stripped as hop-by-hop)
        for (name, value) in forwarded_values {
            parts.headers.insert(name, value);
        }

        // 5. Apply extra headers
        for (name, value) in &self.extra_headers {
            parts.headers.insert(name, value.clone());
        }

        // 6. Remove explicit headers
        for name in &self.remove_headers {
            parts.headers.remove(name);
        }

        // 7. Run on_request hook
        if let Some(hook) = self.on_request {
            hook(&mut parts);
        }

        // 8. Build the request URI for hyper.
        // H2 extended CONNECT uses absolute URI.
        // RFC 7540 §8.3: ordinary CONNECT over h2c uses authority form.
        // Other h2c requests use absolute URI.
        // AdaptiveH2c uses path-only form because the dispatch layer may fall back
        // to H1, and absolute-form URIs confuse many origin servers.
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

        // 9. Convert body to RequestBodySend
        let boxed_body: RequestBodySend = body
            .map_frame(|frame| frame)
            .map_err(|e| {
                let boxed: Box<dyn std::error::Error + Send + Sync> = e.into();
                Error::Other(boxed)
            })
            .boxed_unsync();

        let mut request = http::Request::from_parts(parts, boxed_body);
        self.client.core.apply_automatic_content_digest(
            self.client.core.automatic_content_digest,
            request.headers_mut(),
            &crate::digest_fields::ContentDigestBody::Unavailable,
        )?;
        self.client
            .core
            .sign_final_request(&full_uri, &mut request)?;
        let post_signing_headers;
        let stale_headers = if !self.client.core.no_connection_reuse {
            post_signing_headers = request.headers().clone();
            Some(&post_signing_headers)
        } else {
            None
        };

        // 10. Send via execute_single_with_hint (bypasses redirects, cookies, cache, decompression)
        let send_fut = self.client.execute_single_with_hint_send(
            request,
            &full_uri,
            self.protocol_hint,
            None,
            stale_headers,
            None,
            None,
            None,
        );

        let mut resp = if let Some(duration) = self.timeout {
            crate::timeout::Timeout::WithTimeout {
                future: send_fut,
                sleep: R::sleep(duration),
            }
            .await?
        } else if let Some(duration) = self.client.core.timeout {
            crate::timeout::Timeout::WithTimeout {
                future: send_fut,
                sleep: R::sleep(duration),
            }
            .await?
        } else {
            send_fut.await?
        };

        // 11. Strip hop-by-hop from response (skip for upgrade responses)
        if resp.status() != http::StatusCode::SWITCHING_PROTOCOLS && !is_h2_extended_connect {
            let resp_headers = resp.headers_mut();
            hop_by_hop::strip_hop_by_hop(resp_headers);
        }

        // 12. Run on_response hook
        if let Some(hook) = self.on_response {
            hook(&mut resp);
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
        let builder = ForwardBuilder::new(&client, req).strip_prefix("/api");
        assert_eq!(builder.strip_prefix.as_deref(), Some("/api"));
    }

    #[test]
    fn preserve_host_sets_flag() {
        let client = test_client();
        let req = dummy_request("/path");
        let builder = ForwardBuilder::new(&client, req).preserve_host();
        assert!(builder.preserve_host);
    }

    #[test]
    fn timeout_sets_duration() {
        let client = test_client();
        let req = dummy_request("/path");
        let builder = ForwardBuilder::new(&client, req).timeout(Duration::from_secs(5));
        assert_eq!(builder.timeout, Some(Duration::from_secs(5)));
    }

    #[test]
    fn header_adds_to_extra_headers() {
        let client = test_client();
        let req = dummy_request("/path");
        let builder = ForwardBuilder::new(&client, req)
            .header(http::header::ACCEPT, HeaderValue::from_static("text/html"));
        assert_eq!(builder.extra_headers.get("accept").unwrap(), "text/html");
    }

    #[test]
    fn forward_header_adds_to_list() {
        let client = test_client();
        let req = dummy_request("/path");
        let builder = ForwardBuilder::new(&client, req).forward_header(http::header::AUTHORIZATION);
        assert_eq!(builder.forward_headers.len(), 1);
        assert_eq!(builder.forward_headers[0], http::header::AUTHORIZATION);
    }

    #[test]
    fn remove_header_adds_to_list() {
        let client = test_client();
        let req = dummy_request("/path");
        let builder = ForwardBuilder::new(&client, req).remove_header(http::header::COOKIE);
        assert_eq!(builder.remove_headers.len(), 1);
        assert_eq!(builder.remove_headers[0], http::header::COOKIE);
    }

    #[test]
    fn upstream_sets_uri() {
        let client = test_client();
        let req = dummy_request("/path");
        let builder = ForwardBuilder::new(&client, req).upstream("http://backend:8080");
        assert_eq!(
            builder.upstream.unwrap().to_string(),
            "http://backend:8080/"
        );
    }

    #[test]
    fn h2c_sets_protocol_hint() {
        let client = test_client();
        let req = dummy_request("/path");
        let builder = ForwardBuilder::new(&client, req).h2c();
        assert_eq!(builder.protocol_hint, ProtocolHint::H2c);
    }

    #[test]
    fn adaptive_h2c_sets_protocol_hint() {
        let client = test_client();
        let req = dummy_request("/path");
        let builder = ForwardBuilder::new(&client, req).adaptive_h2c();
        assert_eq!(builder.protocol_hint, ProtocolHint::AdaptiveH2c);
    }

    #[test]
    fn upgrade_pushes_connection_and_upgrade_headers() {
        let client = test_client();
        let req = dummy_request("/ws");
        let builder = ForwardBuilder::new(&client, req).upgrade();
        assert_eq!(builder.forward_headers.len(), 2);
        assert_eq!(builder.forward_headers[0], http::header::CONNECTION);
        assert_eq!(builder.forward_headers[1], http::header::UPGRADE);
    }

    #[test]
    fn on_request_hook_is_set() {
        let client = test_client();
        let req = dummy_request("/path");
        let builder = ForwardBuilder::new(&client, req).on_request(|_parts| {});
        assert!(builder.on_request.is_some());
    }

    #[test]
    fn on_response_hook_is_set() {
        let client = test_client();
        let req = dummy_request("/path");
        let builder = ForwardBuilder::new(&client, req).on_response(|_resp| {});
        assert!(builder.on_response.is_some());
    }

    #[test]
    fn chained_builder() {
        let client = test_client();
        let req = dummy_request("/api/users?page=1");
        let builder = ForwardBuilder::new(&client, req)
            .upstream("http://backend:8080")
            .strip_prefix("/api")
            .preserve_host()
            .timeout(Duration::from_secs(30))
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
        assert_eq!(builder.protocol_hint, ProtocolHint::H2c);
        assert_eq!(builder.extra_headers.len(), 1);
        assert_eq!(builder.forward_headers.len(), 1);
        assert_eq!(builder.remove_headers.len(), 1);
    }

    #[tokio::test]
    async fn send_without_upstream_returns_error() {
        let client = test_client();
        let req = dummy_request("/path");
        let result = ForwardBuilder::new(&client, req).send().await;
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
        let result = ForwardBuilder::new(&client, req)
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
        let result = ForwardBuilder::new(&client, req)
            .upstream("http://bad host")
            .send()
            .await;
        match result.unwrap_err() {
            Error::InvalidUrl(msg) => assert!(msg.contains("invalid forward upstream")),
            other => panic!("expected InvalidUrl, got: {other:?}"),
        }
    }
}
