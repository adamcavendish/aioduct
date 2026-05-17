mod body_transforms;
mod consume;
mod response_local;
#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests;

use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use crate::clock::Instant;

use bytes::Bytes;
use http::header::{CONTENT_LENGTH, HeaderMap, SET_COOKIE};
use http::{Method, StatusCode, Uri, Version};
use http_body_util::BodyExt;

use crate::body::RequestBodySend;
use crate::error::Error;
use crate::observer::RequestObserver;

pin_project_lite::pin_project! {
    #[project = ResponseBodySendProj]
    /// `Send`-safe response body for poll-based runtimes (tokio, smol).
    ///
    /// Holds either a raw hyper `Incoming` body (avoids boxing) or a type-erased
    /// boxed body after transformations (decompression, read timeout, bandwidth limiting).
    /// This is the default body type for [`Response`].
    pub enum ResponseBodySend {
        #[allow(missing_docs)]
        Incoming { #[pin] body: http_body_util::combinators::MapErr<hyper::body::Incoming, fn(hyper::Error) -> Error> },
        #[allow(missing_docs)]
        Boxed { #[pin] body: RequestBodySend },
    }
}

impl ResponseBodySend {
    pub(crate) fn from_incoming(incoming: hyper::body::Incoming) -> Self {
        ResponseBodySend::Incoming {
            body: incoming.map_err(Error::Hyper as fn(hyper::Error) -> Error),
        }
    }

    pub(crate) fn from_boxed(body: RequestBodySend) -> Self {
        ResponseBodySend::Boxed { body }
    }

    pub(crate) fn into_boxed(self) -> RequestBodySend {
        match self {
            ResponseBodySend::Incoming { body } => body.boxed_unsync(),
            ResponseBodySend::Boxed { body } => body,
        }
    }
}

impl http_body::Body for ResponseBodySend {
    type Data = Bytes;
    type Error = Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<http_body::Frame<Self::Data>, Self::Error>>> {
        match self.project() {
            ResponseBodySendProj::Incoming { body } => body.poll_frame(cx),
            ResponseBodySendProj::Boxed { body } => body.poll_frame(cx),
        }
    }

    fn is_end_stream(&self) -> bool {
        match self {
            ResponseBodySend::Incoming { body } => body.is_end_stream(),
            ResponseBodySend::Boxed { body } => body.is_end_stream(),
        }
    }

    fn size_hint(&self) -> http_body::SizeHint {
        match self {
            ResponseBodySend::Incoming { body } => body.size_hint(),
            ResponseBodySend::Boxed { body } => body.size_hint(),
        }
    }
}

/// An HTTP response with status, headers, and a streaming body.
///
/// The type parameter `B` controls the body type:
/// - `ResponseBodySend` (default) — for Send runtimes (tokio, smol)
/// - [`ResponseBodyLocal`](crate::body::ResponseBodyLocal) — for Local runtimes (compio)
pub struct Response<B = ResponseBodySend> {
    inner: http::Response<B>,
    url: Uri,
    remote_addr: Option<SocketAddr>,
    tls_info: Option<crate::tls::TlsInfo>,
    #[allow(deprecated)]
    timings: Option<crate::timing::RequestTimings>,
    observer_ctx: Option<BodyObserverCtx>,
}

#[derive(Clone)]
pub(crate) struct BodyObserverCtx {
    pub(crate) observer: Arc<dyn RequestObserver>,
    pub(crate) method: Method,
    pub(crate) uri: Uri,
    pub(crate) response_started: Instant,
}

impl<B> std::fmt::Debug for Response<B> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Response")
            .field("status", &self.inner.status())
            .field("version", &self.inner.version())
            .field("url", &self.url)
            .finish_non_exhaustive()
    }
}

impl Response {
    pub(crate) fn new(inner: http::Response<ResponseBodySend>, url: Uri) -> Self {
        Self {
            inner,
            url,
            remote_addr: None,
            tls_info: None,
            timings: None,
            observer_ctx: None,
        }
    }

    pub(crate) fn from_boxed(inner: http::Response<RequestBodySend>, url: Uri) -> Self {
        let (parts, body) = inner.into_parts();
        Self {
            inner: http::Response::from_parts(parts, ResponseBodySend::from_boxed(body)),
            url,
            remote_addr: None,
            tls_info: None,
            timings: None,
            observer_ctx: None,
        }
    }
}

// ── Methods available for all body types ─────────────────────────────────────

impl<B> Response<B> {
    pub(crate) fn set_remote_addr(&mut self, addr: Option<SocketAddr>) {
        self.remote_addr = addr;
    }

    pub(crate) fn set_tls_info(&mut self, info: Option<crate::tls::TlsInfo>) {
        self.tls_info = info;
    }

    pub(crate) fn set_observer_ctx(&mut self, ctx: BodyObserverCtx) {
        self.observer_ctx = Some(ctx);
    }

    #[allow(deprecated)]
    pub(crate) fn set_timings(&mut self, timings: Option<crate::timing::RequestTimings>) {
        self.timings = timings;
    }

    /// Returns the final URL of this response, after any redirects.
    pub fn url(&self) -> &Uri {
        &self.url
    }

    /// Returns the remote socket address of the server.
    pub fn remote_addr(&self) -> Option<SocketAddr> {
        self.remote_addr
    }

    /// Returns TLS handshake info (peer certificate), if the connection used TLS.
    pub fn tls_info(&self) -> Option<&crate::tls::TlsInfo> {
        self.tls_info.as_ref()
    }

    /// Returns per-request timing breakdown (DNS, TCP, TLS, TTFB, total).
    #[deprecated(
        since = "0.2.0",
        note = "Use `RequestObserver` for detailed per-phase timing"
    )]
    #[allow(deprecated)]
    pub fn timings(&self) -> Option<&crate::timing::RequestTimings> {
        self.timings.as_ref()
    }

    /// Returns the HTTP status code.
    pub fn status(&self) -> StatusCode {
        self.inner.status()
    }

    /// Returns the response headers.
    pub fn headers(&self) -> &HeaderMap {
        self.inner.headers()
    }

    /// Returns a mutable reference to the response headers.
    pub fn headers_mut(&mut self) -> &mut HeaderMap {
        self.inner.headers_mut()
    }

    /// Returns a reference to the response extensions.
    pub fn extensions(&self) -> &http::Extensions {
        self.inner.extensions()
    }

    /// Returns a mutable reference to the response extensions.
    pub fn extensions_mut(&mut self) -> &mut http::Extensions {
        self.inner.extensions_mut()
    }

    /// Returns the HTTP version.
    pub fn version(&self) -> Version {
        self.inner.version()
    }

    /// Returns an error if the response status is a client (4xx) or server (5xx) error.
    pub fn error_for_status(self) -> Result<Self, Error> {
        let status = self.inner.status();
        if status.is_client_error() || status.is_server_error() {
            Err(Error::Status(status))
        } else {
            Ok(self)
        }
    }

    /// Returns an error reference if the status is 4xx or 5xx, without consuming the response.
    pub fn error_for_status_ref(&self) -> Result<&Self, Error> {
        let status = self.inner.status();
        if status.is_client_error() || status.is_server_error() {
            Err(Error::Status(status))
        } else {
            Ok(self)
        }
    }

    /// Returns the Content-Length header value, if present.
    pub fn content_length(&self) -> Option<u64> {
        self.inner
            .headers()
            .get(CONTENT_LENGTH)?
            .to_str()
            .ok()?
            .parse()
            .ok()
    }

    /// Parse all `Link` headers from the response (RFC 8288).
    pub fn links(&self) -> Vec<crate::link::Link> {
        crate::link::parse_link_headers(self.inner.headers())
    }

    /// Parse all `Set-Cookie` response headers and return the cookies.
    pub fn cookies(&self) -> Vec<crate::Cookie> {
        let domain = self.url.host().unwrap_or("");
        let path = self.url.path();
        self.inner
            .headers()
            .get_all(SET_COOKIE)
            .iter()
            .filter_map(|val| {
                val.to_str()
                    .ok()
                    .and_then(|s| crate::cookie::parse_set_cookie(s, domain, path))
            })
            .collect()
    }
}

// ── Body consumption methods available for all body types ───────────────────

impl<B: http_body::Body<Data = Bytes, Error = Error>> Response<B> {
    /// Consume the response body and return it as bytes.
    pub async fn bytes(self) -> Result<Bytes, Error> {
        let observer_ctx = self.observer_ctx;
        let response_started = observer_ctx.as_ref().map(|c| c.response_started);
        let body = self.inner.into_body();
        match body.collect().await {
            Ok(collected) => {
                let bytes = collected.to_bytes();
                if let Some(ctx) = &observer_ctx {
                    let total_bytes = bytes.len() as u64;
                    let transfer_duration = ctx.response_started.elapsed();
                    let throughput = if transfer_duration.as_secs_f64() > 0.0 {
                        (total_bytes as f64 / transfer_duration.as_secs_f64()) as f32
                    } else {
                        0.0
                    };
                    ctx.observer.on_event(&crate::observer::RequestEvent {
                        method: ctx.method.clone(),
                        uri: ctx.uri.clone(),
                        phase: crate::observer::RequestPhase::TransferComplete {
                            direction: crate::observer::TransferDirection::Download,
                            total_bytes,
                            transfer_duration,
                            throughput_bytes_per_sec: throughput,
                        },
                        at: crate::observer::Instant::now(),
                    });
                }
                Ok(bytes)
            }
            Err(e) => {
                if let Some(ctx) = &observer_ctx {
                    ctx.observer.on_event(&crate::observer::RequestEvent {
                        method: ctx.method.clone(),
                        uri: ctx.uri.clone(),
                        phase: crate::observer::RequestPhase::TransferAborted {
                            direction: crate::observer::TransferDirection::Download,
                            bytes_transferred: 0,
                            elapsed: response_started.map(|t| t.elapsed()).unwrap_or_default(),
                            error: e.to_string(),
                        },
                        at: crate::observer::Instant::now(),
                    });
                }
                Err(e)
            }
        }
    }

    /// Consume the response body and return it as a UTF-8 string.
    pub async fn text(self) -> Result<String, Error> {
        #[cfg(feature = "charset")]
        {
            self.text_with_charset("utf-8").await
        }
        #[cfg(not(feature = "charset"))]
        {
            let bytes = self.bytes().await?;
            String::from_utf8(bytes.to_vec()).map_err(|e| Error::Other(Box::new(e)))
        }
    }

    #[cfg(feature = "charset")]
    /// Consume the response body and decode it using the charset from Content-Type,
    /// falling back to the given default encoding.
    pub async fn text_with_charset(self, default_encoding: &str) -> Result<String, Error> {
        let content_type = self
            .headers()
            .get(http::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<mime::Mime>().ok());
        let encoding_name = content_type
            .as_ref()
            .and_then(|mime| mime.get_param("charset"))
            .map(|charset| charset.as_str())
            .unwrap_or(default_encoding);
        let encoding = encoding_rs::Encoding::for_label(encoding_name.as_bytes())
            .unwrap_or(encoding_rs::UTF_8);
        let bytes = self.bytes().await?;
        let (text, _, _) = encoding.decode(&bytes);
        Ok(text.into_owned())
    }

    /// Consume the response body and deserialize it as JSON.
    #[cfg(feature = "json")]
    pub async fn json<T: serde::de::DeserializeOwned>(self) -> Result<T, Error> {
        let bytes = self.bytes().await?;
        serde_json::from_slice(&bytes).map_err(|e| Error::Other(Box::new(e)))
    }

    /// Consume the response body and deserialize it as RFC 9457 Problem Details.
    ///
    /// Checks that the `Content-Type` is `application/problem+json` before
    /// attempting to parse. Returns `None` if the content type does not match.
    #[cfg(feature = "json")]
    pub async fn problem_details(self) -> Option<Result<crate::problem::ProblemDetails, Error>> {
        let is_problem = self
            .inner
            .headers()
            .get(http::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(|ct| {
                let ct = ct.to_lowercase();
                ct.starts_with("application/problem+json")
            })
            .unwrap_or(false);
        if !is_problem {
            return None;
        }
        Some(self.json().await)
    }
}
