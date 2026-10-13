mod body_transforms;
mod consume;
mod response_local;
#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests;

use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use bytes::{Bytes, BytesMut};
use http::header::{CONTENT_LENGTH, HeaderMap, SET_COOKIE};
use http::{StatusCode, Uri, Version};
use http_body_util::BodyExt;

use crate::body::RequestBodySend;
use crate::error::Error;

struct BufferedBody {
    data: Option<Bytes>,
    trailers: Option<HeaderMap>,
}

impl BufferedBody {
    fn new(data: Bytes, trailers: Option<HeaderMap>) -> Self {
        Self {
            data: (!data.is_empty()).then_some(data),
            trailers,
        }
    }
}

impl http_body::Body for BufferedBody {
    type Data = Bytes;
    type Error = Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
    ) -> Poll<Option<Result<http_body::Frame<Self::Data>, Self::Error>>> {
        let this = self.get_mut();
        if let Some(data) = this.data.take() {
            return Poll::Ready(Some(Ok(http_body::Frame::data(data))));
        }
        if let Some(trailers) = this.trailers.take() {
            return Poll::Ready(Some(Ok(http_body::Frame::trailers(trailers))));
        }
        Poll::Ready(None)
    }

    fn is_end_stream(&self) -> bool {
        self.data.is_none() && self.trailers.is_none()
    }

    fn size_hint(&self) -> http_body::SizeHint {
        let mut hint = http_body::SizeHint::new();
        let len = self.data.as_ref().map(|data| data.len()).unwrap_or(0);
        hint.set_exact(len as u64);
        hint
    }
}

pin_project_lite::pin_project! {
    /// `Send`-safe response body for poll-based runtimes (tokio, smol).
    ///
    /// Holds either a raw hyper `Incoming` body (avoids boxing) or a type-erased
    /// boxed body after transformations (decompression, read timeout, bandwidth limiting).
    /// This is the default body type for [`Response`].
    pub struct ResponseBodySend {
        #[pin]
        inner: SendBodyInner,
        owner: Option<crate::body::SendObservationHandle>,
    }
}

pin_project_lite::pin_project! {
    #[project = SendBodyInnerProj]
    enum SendBodyInner {
        Incoming {
            #[pin]
            body: http_body_util::combinators::MapErr<hyper::body::Incoming, fn(hyper::Error) -> Error>,
        },
        Boxed {
            #[pin]
            body: RequestBodySend,
        },
    }
}

impl ResponseBodySend {
    pub(crate) fn channel_receiver(&self) -> Option<Arc<dyn crate::body::SendBodyObserver>> {
        self.owner.as_ref()?.channel_receiver()
    }

    pub(crate) fn take_owner(&mut self) -> Option<crate::body::SendObservationHandle> {
        self.owner.take()
    }

    pub(crate) fn from_incoming(incoming: hyper::body::Incoming) -> Self {
        Self {
            inner: SendBodyInner::Incoming {
                body: incoming.map_err(Error::Hyper as fn(hyper::Error) -> Error),
            },
            owner: None,
        }
    }

    pub(crate) fn from_boxed(body: RequestBodySend) -> Self {
        Self::with_owner(body, None)
    }

    pub(crate) fn with_owner(
        body: RequestBodySend,
        owner: Option<crate::body::SendObservationHandle>,
    ) -> Self {
        Self {
            inner: SendBodyInner::Boxed { body },
            owner,
        }
    }

    pub(crate) fn into_parts(
        self,
    ) -> (RequestBodySend, Option<crate::body::SendObservationHandle>) {
        match self.inner {
            SendBodyInner::Incoming { body } => (body.boxed_unsync(), self.owner),
            SendBodyInner::Boxed { body } => (body, self.owner),
        }
    }

    pub(crate) fn finish_handoff(&mut self) {
        if let Some(owner) = &self.owner {
            owner.handoff();
        }
    }

    pub(crate) fn finish_error(&self, error: &Error) {
        if let Some(owner) = &self.owner {
            owner.fail(error);
        }
    }

    pub(crate) fn set_owner(&mut self, owner: crate::body::SendObservationHandle) {
        debug_assert!(self.owner.is_none(), "response body owner was already set");
        self.owner = Some(owner);
    }
}

impl http_body::Body for ResponseBodySend {
    type Data = Bytes;
    type Error = Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<http_body::Frame<Self::Data>, Self::Error>>> {
        let this = self.project();
        let owner = this.owner;
        let mut inner = this.inner;
        let result = match inner.as_mut().project() {
            SendBodyInnerProj::Incoming { body } => body.poll_frame(cx),
            SendBodyInnerProj::Boxed { body } => body.poll_frame(cx),
        };
        match &result {
            Poll::Ready(Some(Err(error))) => {
                if let Some(owner) = owner.as_ref() {
                    owner.fail(error);
                }
            }
            Poll::Ready(None) => {
                if let Some(owner) = owner.as_ref() {
                    owner.finish(crate::body::BodyEnd::Complete);
                }
            }
            Poll::Ready(Some(Ok(_))) => {
                if let Some(owner) = owner.as_ref()
                    && inner.is_end_stream()
                {
                    owner.finish(crate::body::BodyEnd::Complete);
                }
            }
            Poll::Pending => {}
        }
        result
    }

    fn is_end_stream(&self) -> bool {
        match &self.inner {
            SendBodyInner::Incoming { body } => body.is_end_stream(),
            SendBodyInner::Boxed { body } => body.is_end_stream(),
        }
    }

    fn size_hint(&self) -> http_body::SizeHint {
        match &self.inner {
            SendBodyInner::Incoming { body } => body.size_hint(),
            SendBodyInner::Boxed { body } => body.size_hint(),
        }
    }
}

impl http_body::Body for SendBodyInner {
    type Data = Bytes;
    type Error = Error;
    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<http_body::Frame<Bytes>, Error>>> {
        match self.project() {
            SendBodyInnerProj::Incoming { body } => body.poll_frame(cx),
            SendBodyInnerProj::Boxed { body } => body.poll_frame(cx),
        }
    }

    fn is_end_stream(&self) -> bool {
        match self {
            Self::Incoming { body } => body.is_end_stream(),
            Self::Boxed { body } => body.is_end_stream(),
        }
    }

    fn size_hint(&self) -> http_body::SizeHint {
        match self {
            Self::Incoming { body } => body.size_hint(),
            Self::Boxed { body } => body.size_hint(),
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
    fragment: Option<String>,
}

struct DispatchOwnedExtensions {
    on_upgrade: Option<hyper::upgrade::OnUpgrade>,
    local_upgrade: Option<crate::upgrade::UpgradeHandleLocal>,
    stream_permit: Option<UpgradeStreamPermit>,
}

#[derive(Clone)]
struct UpgradeStreamPermit(Arc<Mutex<Option<crate::pool::ActiveStreamPermit>>>);

impl UpgradeStreamPermit {
    fn new(permit: crate::pool::ActiveStreamPermit) -> Self {
        Self(Arc::new(Mutex::new(Some(permit))))
    }

    fn retire_transport(&self) {
        if let Some(permit) = self
            .0
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .as_ref()
        {
            permit.retire_transport();
        }
    }

    fn take(self) -> Option<crate::pool::ActiveStreamPermit> {
        self.0
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take()
    }
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
            fragment: None,
        }
    }

    pub(crate) fn from_boxed(inner: http::Response<RequestBodySend>, url: Uri) -> Self {
        let (parts, body) = inner.into_parts();
        Self {
            inner: http::Response::from_parts(parts, ResponseBodySend::from_boxed(body)),
            url,
            remote_addr: None,
            tls_info: None,
            fragment: None,
        }
    }

    pub(crate) fn hold_active_stream_permit(&mut self, permit: crate::pool::ActiveStreamPermit) {
        self.inner
            .extensions_mut()
            .insert(UpgradeStreamPermit::new(permit));
    }
}

// ── Methods available for all body types ─────────────────────────────────────

impl<B> Response<B> {
    pub(crate) fn run_hook_preserving_dispatch_extensions(&mut self, hook: impl FnOnce(&mut Self)) {
        let owned = DispatchOwnedExtensions::take(self.inner.extensions_mut());
        hook(self);
        owned.restore(self.inner.extensions_mut());
    }

    pub(crate) fn map_body<C>(self, map: impl FnOnce(B) -> C) -> Response<C> {
        let (parts, body) = self.inner.into_parts();
        Response {
            inner: http::Response::from_parts(parts, map(body)),
            url: self.url,
            remote_addr: self.remote_addr,
            tls_info: self.tls_info,
            fragment: self.fragment,
        }
    }

    pub(crate) fn take_upgrade_stream_permit(&mut self) -> Option<crate::pool::ActiveStreamPermit> {
        self.inner
            .extensions_mut()
            .remove::<UpgradeStreamPermit>()
            .and_then(UpgradeStreamPermit::take)
    }

    pub(crate) fn set_remote_addr(&mut self, addr: Option<SocketAddr>) {
        self.remote_addr = addr;
    }

    pub(crate) fn set_tls_info(&mut self, info: Option<crate::tls::TlsInfo>) {
        self.tls_info = info;
    }

    /// Returns the final URL of this response, after any redirects.
    pub fn url(&self) -> &Uri {
        &self.url
    }

    /// Returns the original URL fragment preserved across redirects.
    ///
    /// Per RFC 7231 Section 7.1.2, if the Location header in a redirect has no
    /// fragment, the original request's fragment MUST be inherited. This method
    /// returns that fragment, if one was present in the original URL.
    pub fn fragment(&self) -> Option<&str> {
        self.fragment.as_deref()
    }

    pub(crate) fn set_fragment(&mut self, fragment: Option<String>) {
        self.fragment = fragment;
    }

    /// Returns the remote socket address of the server.
    pub fn remote_addr(&self) -> Option<SocketAddr> {
        self.remote_addr
    }

    /// Returns TLS handshake info (peer certificate), if the connection used TLS.
    pub fn tls_info(&self) -> Option<&crate::tls::TlsInfo> {
        self.tls_info.as_ref()
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

    pub(crate) fn body_mut(&mut self) -> &mut B {
        self.inner.body_mut()
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

    pub(crate) fn set_version(&mut self, version: Version) {
        *self.inner.version_mut() = version;
    }

    /// Consume this response and return the underlying `http::Response`.
    pub fn into_http_response(self) -> http::Response<B> {
        if let Some(permit) = self.inner.extensions().get::<UpgradeStreamPermit>() {
            permit.retire_transport();
        }
        self.inner
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
    pub(crate) async fn into_drained_body(self) -> Result<Response, Error> {
        let Response {
            inner,
            url,
            remote_addr,
            tls_info,
            fragment,
        } = self;
        let (parts, body) = inner.into_parts();
        body.collect().await?;
        let empty = http_body_util::Empty::<Bytes>::new()
            .map_err(|never| match never {})
            .boxed_unsync();
        Ok(Response {
            inner: http::Response::from_parts(parts, ResponseBodySend::from_boxed(empty)),
            url,
            remote_addr,
            tls_info,
            fragment,
        })
    }

    pub(crate) async fn into_buffered_with_limit(
        self,
        max_bytes: usize,
        operation: &str,
    ) -> Result<(Response, Bytes), Error> {
        let Response {
            inner,
            url,
            remote_addr,
            tls_info,
            fragment,
        } = self;
        let (parts, body) = inner.into_parts();
        let mut body = std::pin::pin!(body);
        let mut buf = BytesMut::new();
        let mut trailers: Option<HeaderMap> = None;

        loop {
            match body.as_mut().frame().await {
                Some(Ok(frame)) => match frame.into_data() {
                    Ok(data) => {
                        let new_len = buf.len().checked_add(data.len()).ok_or_else(|| {
                            Error::Unsupported(format!(
                                "{operation} response body exceeds the configured buffer limit of {max_bytes} bytes"
                            ))
                        })?;
                        if new_len > max_bytes {
                            return Err(Error::Unsupported(format!(
                                "{operation} response body exceeds the configured buffer limit of {max_bytes} bytes"
                            )));
                        }
                        buf.extend_from_slice(&data);
                    }
                    Err(frame) => {
                        if let Ok(frame_trailers) = frame.into_trailers() {
                            match &mut trailers {
                                Some(existing) => existing.extend(frame_trailers),
                                None => trailers = Some(frame_trailers),
                            }
                        }
                    }
                },
                Some(Err(error)) => {
                    return Err(error);
                }
                None => {
                    let body_bytes = buf.freeze();
                    let buffered_body =
                        BufferedBody::new(body_bytes.clone(), trailers).boxed_unsync();
                    let response = Response {
                        inner: http::Response::from_parts(
                            parts,
                            ResponseBodySend::from_boxed(buffered_body),
                        ),
                        url,
                        remote_addr,
                        tls_info,
                        fragment,
                    };
                    return Ok((response, body_bytes));
                }
            }
        }
    }

    /// Consume the response body and return it as bytes.
    pub async fn bytes(self) -> Result<Bytes, Error> {
        use http_body_util::BodyExt;

        let mut body = std::pin::pin!(self.inner.into_body());
        let mut buf = bytes::BytesMut::new();

        loop {
            match body.as_mut().frame().await {
                Some(Ok(frame)) => {
                    if let Ok(data) = frame.into_data() {
                        buf.extend_from_slice(&data);
                    }
                }
                Some(Err(e)) => return Err(e),
                None => return Ok(buf.freeze()),
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

impl DispatchOwnedExtensions {
    fn take(extensions: &mut http::Extensions) -> Self {
        Self {
            on_upgrade: extensions.remove::<hyper::upgrade::OnUpgrade>(),
            local_upgrade: extensions.remove::<crate::upgrade::UpgradeHandleLocal>(),
            stream_permit: extensions.remove::<UpgradeStreamPermit>(),
        }
    }

    fn restore(self, extensions: &mut http::Extensions) {
        if let Some(on_upgrade) = self.on_upgrade {
            extensions.insert(on_upgrade);
        }
        if let Some(local_upgrade) = self.local_upgrade {
            extensions.insert(local_upgrade);
        }
        if let Some(stream_permit) = self.stream_permit {
            extensions.insert(stream_permit);
        }
    }
}
