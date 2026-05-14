use std::time::Duration;

use crate::body::RequestBoxBody;
use crate::client::HttpEngineLocal;
use crate::error::Error;
use crate::response::Response;
use crate::runtime::{Connector, RuntimeLocal};
use bytes::Bytes;
use http::header::{HOST, HeaderMap, HeaderName, HeaderValue};
use http::uri::{Parts as UriParts, PathAndQuery, Scheme, Uri};
use http_body::Body;
use http_body_util::BodyExt;

use super::hop_by_hop;

type RequestHook = Box<dyn FnOnce(&mut http::request::Parts)>;
type ResponseHook = Box<dyn FnOnce(&mut Response)>;

/// Builder for forwarding an incoming HTTP request on a `!Send` runtime.
///
/// Created via [`HttpEngineLocal::forward_local`]. Mirrors [`super::ForwardBuilder`]
/// for completion-based runtimes.
pub struct ForwardBuilderLocal<'a, R: RuntimeLocal, C: Connector + Clone, B> {
    client: &'a HttpEngineLocal<R, C>,
    request: http::Request<B>,
    upstream: Option<Uri>,
    strip_prefix: Option<String>,
    preserve_host: bool,
    timeout: Option<Duration>,
    extra_headers: HeaderMap,
    remove_headers: Vec<HeaderName>,
    forward_headers: Vec<HeaderName>,
    on_request: Option<RequestHook>,
    on_response: Option<ResponseHook>,
}

impl<'a, R: RuntimeLocal, C: Connector + Clone, B> ForwardBuilderLocal<'a, R, C, B>
where
    B: Body<Data = Bytes> + 'static,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    pub(crate) fn new(client: &'a HttpEngineLocal<R, C>, request: http::Request<B>) -> Self {
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
            on_request: None,
            on_response: None,
        }
    }

    /// Set the upstream origin to forward to.
    pub fn upstream(mut self, uri: impl TryInto<Uri>) -> Self
    where
        <Uri as TryFrom<Uri>>::Error: std::fmt::Debug,
    {
        if let Ok(u) = uri.try_into() {
            self.upstream = Some(u);
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

    /// Marks this as an HTTP/1.1 upgrade request.
    pub fn upgrade(mut self) -> Self {
        self.forward_headers.push(http::header::CONNECTION);
        self.forward_headers.push(http::header::UPGRADE);
        self
    }

    /// Execute the forwarded request.
    pub async fn send(mut self) -> Result<Response<crate::body::ResponseBoxLocalBody>, Error> {
        let (mut parts, body) = self.request.into_parts();

        let is_h1_upgrade = parts
            .headers
            .get(http::header::CONNECTION)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.to_ascii_lowercase().contains("upgrade"));

        if is_h1_upgrade {
            self.forward_headers.push(http::header::CONNECTION);
            self.forward_headers.push(http::header::UPGRADE);
            parts.version = http::Version::HTTP_11;
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

        let request_uri: Uri = full_uri
            .path_and_query()
            .map(|pq| pq.as_str())
            .unwrap_or("/")
            .parse()
            .map_err(|e| Error::Other(Box::new(e)))?;
        parts.uri = request_uri;

        // Buffer the body into bytes. For !Send runtimes we cannot use
        // UnsyncBoxBody::new (which requires Send), so we collect first.
        let collected = body.collect().await.map_err(|e| {
            let boxed: Box<dyn std::error::Error + Send + Sync> = e.into();
            Error::Other(boxed)
        })?;
        let bytes = collected.to_bytes();
        let boxed_body: RequestBoxBody = http_body_util::Full::new(bytes)
            .map_err(|never| match never {})
            .boxed_unsync();

        let request = http::Request::from_parts(parts, boxed_body);

        let send_fut = self.client.execute_single_local(request, &full_uri, None);

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

        if resp.status() != http::StatusCode::SWITCHING_PROTOCOLS && !is_h1_upgrade {
            let resp_headers = resp.headers_mut();
            hop_by_hop::strip_hop_by_hop(resp_headers);
        }

        if let Some(hook) = self.on_response {
            hook(&mut resp);
        }

        Ok(resp.into_local())
    }
}
