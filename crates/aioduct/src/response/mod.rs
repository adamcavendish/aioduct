mod body_transforms;
mod consume;
mod local;
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

use crate::body::RequestBoxBody;
use crate::error::Error;
use crate::observer::RequestObserver;

pin_project_lite::pin_project! {
    #[project = ResponseBoxSendBodyProj]
    /// `Send`-safe response body for poll-based runtimes (tokio, smol).
    ///
    /// Holds either a raw hyper `Incoming` body (avoids boxing) or a type-erased
    /// boxed body after transformations (decompression, read timeout, bandwidth limiting).
    /// This is the default body type for [`Response`].
    pub enum ResponseBoxSendBody {
        #[allow(missing_docs)]
        Incoming { #[pin] body: http_body_util::combinators::MapErr<hyper::body::Incoming, fn(hyper::Error) -> Error> },
        #[allow(missing_docs)]
        Boxed { #[pin] body: RequestBoxBody },
    }
}

impl ResponseBoxSendBody {
    pub(crate) fn from_incoming(incoming: hyper::body::Incoming) -> Self {
        ResponseBoxSendBody::Incoming {
            body: incoming.map_err(Error::Hyper as fn(hyper::Error) -> Error),
        }
    }

    pub(crate) fn from_boxed(body: RequestBoxBody) -> Self {
        ResponseBoxSendBody::Boxed { body }
    }

    pub(crate) fn into_boxed(self) -> RequestBoxBody {
        match self {
            ResponseBoxSendBody::Incoming { body } => body.boxed_unsync(),
            ResponseBoxSendBody::Boxed { body } => body,
        }
    }
}

impl http_body::Body for ResponseBoxSendBody {
    type Data = Bytes;
    type Error = Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<http_body::Frame<Self::Data>, Self::Error>>> {
        match self.project() {
            ResponseBoxSendBodyProj::Incoming { body } => body.poll_frame(cx),
            ResponseBoxSendBodyProj::Boxed { body } => body.poll_frame(cx),
        }
    }

    fn is_end_stream(&self) -> bool {
        match self {
            ResponseBoxSendBody::Incoming { body } => body.is_end_stream(),
            ResponseBoxSendBody::Boxed { body } => body.is_end_stream(),
        }
    }

    fn size_hint(&self) -> http_body::SizeHint {
        match self {
            ResponseBoxSendBody::Incoming { body } => body.size_hint(),
            ResponseBoxSendBody::Boxed { body } => body.size_hint(),
        }
    }
}

/// An HTTP response with status, headers, and a streaming body.
///
/// The type parameter `B` controls the body type:
/// - `ResponseBoxSendBody` (default) — for Send runtimes (tokio, smol)
/// - [`ResponseBoxLocalBody`](crate::body::ResponseBoxLocalBody) — for Local runtimes (compio)
pub struct Response<B = ResponseBoxSendBody> {
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
    pub(crate) fn new(inner: http::Response<ResponseBoxSendBody>, url: Uri) -> Self {
        Self {
            inner,
            url,
            remote_addr: None,
            tls_info: None,
            timings: None,
            observer_ctx: None,
        }
    }

    pub(crate) fn from_boxed(inner: http::Response<RequestBoxBody>, url: Uri) -> Self {
        let (parts, body) = inner.into_parts();
        Self {
            inner: http::Response::from_parts(parts, ResponseBoxSendBody::from_boxed(body)),
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
        self.inner
            .headers()
            .get_all(SET_COOKIE)
            .iter()
            .filter_map(|val| {
                val.to_str()
                    .ok()
                    .and_then(|s| crate::cookie::parse_set_cookie(s, domain))
            })
            .collect()
    }
}
