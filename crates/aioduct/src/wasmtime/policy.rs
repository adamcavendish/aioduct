use std::fmt;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ::wasmtime_wasi_http::DEFAULT_FORBIDDEN_HEADERS;
use ::wasmtime_wasi_http::p2::bindings::http::types::ErrorCode;
use ::wasmtime_wasi_http::p2::body::HyperOutgoingBody;
use http::header::{
    AUTHORIZATION, CONTENT_LENGTH, COOKIE, HeaderName, HeaderValue, PROXY_AUTHORIZATION,
};
use http::{HeaderMap, Uri};
use http_body::Body;

use super::body::timeout_code_from_aioduct_error;
use super::map_aioduct_error;

pub(crate) type RejectionObserver = Arc<dyn Fn(RejectionReason) + Send + Sync>;

#[derive(Clone, Default)]
pub(crate) struct DeniedHeaderPolicy {
    names: Vec<HeaderName>,
    prefixes: Vec<&'static str>,
}

impl DeniedHeaderPolicy {
    fn deny_name(&mut self, name: HeaderName) {
        self.names.push(name);
    }

    fn deny_prefix(&mut self, prefix: &'static str) {
        self.prefixes.push(prefix);
    }

    pub(crate) fn validate_config(&self) -> Result<(), PolicyError> {
        for prefix in &self.prefixes {
            validate_denied_header_prefix(prefix)?;
        }
        Ok(())
    }

    fn contains(&self, name: &HeaderName) -> bool {
        self.names.iter().any(|denied| denied == name)
            || self
                .prefixes
                .iter()
                .any(|prefix| header_name_starts_with_ignore_ascii_case(name, prefix))
    }
}

/// Exact-origin host policy for WASI HTTP requests.
#[derive(Clone)]
pub struct ExactOriginPolicy {
    origin: Origin,
    pub(crate) origin_uri: Uri,
    forbid_sensitive_headers: bool,
    denied_headers: DeniedHeaderPolicy,
    pub(crate) injected_headers: HeaderMap,
    pub(crate) header_limit: Option<usize>,
    pub(crate) body_limit: Option<u64>,
    pub(crate) deadline: Option<Instant>,
    pub(crate) rejection_observer: Option<RejectionObserver>,
}

impl ExactOriginPolicy {
    /// Create a policy for one allowed origin, for example `https://api.local:8443`.
    pub fn new(origin: &str) -> Result<Self, PolicyError> {
        let uri: Uri = origin
            .parse()
            .map_err(|error| PolicyError::InvalidOrigin(format!("{error}")))?;
        if let Some(path_and_query) = uri.path_and_query()
            && path_and_query.as_str() != "/"
        {
            return Err(PolicyError::OriginMustNotContainPath);
        }
        let parsed = Origin::from_uri(&uri)?;
        let origin_uri = origin_uri(&uri)?;
        Ok(Self {
            origin: parsed,
            origin_uri,
            forbid_sensitive_headers: false,
            denied_headers: DeniedHeaderPolicy::default(),
            injected_headers: HeaderMap::new(),
            header_limit: None,
            body_limit: None,
            deadline: None,
            rejection_observer: None,
        })
    }

    /// Forbid guest-supplied sensitive headers such as `authorization` and `cookie`.
    pub fn forbid_sensitive_headers(mut self) -> Self {
        self.forbid_sensitive_headers = true;
        self
    }

    /// Forbid one guest-supplied request header name.
    pub fn deny_header(mut self, name: HeaderName) -> Self {
        self.denied_headers.deny_name(name);
        self
    }

    /// Forbid multiple guest-supplied request header names.
    pub fn deny_headers<I>(mut self, names: I) -> Self
    where
        I: IntoIterator<Item = HeaderName>,
    {
        for name in names {
            self = self.deny_header(name);
        }
        self
    }

    /// Forbid guest-supplied request headers with an ASCII case-insensitive prefix.
    pub fn deny_header_prefix(mut self, prefix: &'static str) -> Self {
        self.denied_headers.deny_prefix(prefix);
        self
    }

    /// Forbid guest-supplied request headers matching any ASCII case-insensitive prefix.
    pub fn deny_header_prefixes<I>(mut self, prefixes: I) -> Self
    where
        I: IntoIterator<Item = &'static str>,
    {
        for prefix in prefixes {
            self = self.deny_header_prefix(prefix);
        }
        self
    }

    /// Inject a host-owned header after request validation.
    pub fn inject_header(mut self, name: HeaderName, value: HeaderValue) -> Self {
        self.injected_headers.insert(name, value);
        self
    }

    /// Set the maximum request and response header section size in bytes.
    pub fn header_limit(mut self, limit: usize) -> Self {
        self.header_limit = Some(limit);
        self
    }

    /// Set the maximum request and response body size in bytes.
    pub fn body_limit(mut self, limit: u64) -> Self {
        self.body_limit = Some(limit);
        self
    }

    /// Set an absolute host-side deadline for the whole HTTP exchange.
    pub fn deadline(mut self, deadline: Instant) -> Self {
        self.deadline = Some(deadline);
        self
    }

    /// Observe redacted, low-cardinality host-side rejection reasons.
    pub fn on_rejection(
        mut self,
        observer: impl Fn(RejectionReason) + Send + Sync + 'static,
    ) -> Self {
        self.rejection_observer = Some(Arc::new(observer));
        self
    }

    pub(crate) fn validate_config(&self) -> Result<(), PolicyError> {
        for name in self.injected_headers.keys() {
            if DEFAULT_FORBIDDEN_HEADERS.contains(name) {
                return Err(PolicyError::InjectedForbiddenHeader(name.clone()));
            }
        }
        self.denied_headers.validate_config()?;
        Ok(())
    }

    pub(crate) fn validate_origin(&self, uri: &Uri, use_tls: bool) -> Result<(), ErrorCode> {
        let request_origin = Origin::from_uri(uri).map_err(|_| ErrorCode::HttpRequestUriInvalid)?;
        let scheme_is_tls = request_origin.scheme == "https";
        if scheme_is_tls != use_tls {
            return Err(ErrorCode::HttpRequestUriInvalid);
        }
        if request_origin != self.origin {
            return Err(self.request_denied(RejectionReason::OriginMismatch));
        }
        Ok(())
    }

    pub(crate) fn check_request_headers(&self, headers: &HeaderMap) -> Result<(), ErrorCode> {
        for (name, value) in headers {
            if self.is_protected_request_header(name)
                || (self.forbid_sensitive_headers && value.is_sensitive())
            {
                return Err(self.request_denied(RejectionReason::ProtectedHeader));
            }
            if self.is_denied_request_header(name) {
                return Err(self.request_denied(RejectionReason::DeniedHeader));
            }
        }
        Ok(())
    }

    pub(crate) fn check_request_header_limit(&self, headers: &HeaderMap) -> Result<(), ErrorCode> {
        let Some(limit) = self.header_limit else {
            return Ok(());
        };
        if header_section_size(headers) > limit {
            self.notify_rejection(RejectionReason::HeaderLimit);
            return Err(ErrorCode::HttpRequestHeaderSectionSize(Some(limit_to_u32(
                limit,
            ))));
        }
        Ok(())
    }

    pub(crate) fn check_request_body_known_limit(
        &self,
        headers: &HeaderMap,
        body: &HyperOutgoingBody,
    ) -> Result<(), ErrorCode> {
        let Some(limit) = self.body_limit else {
            return Ok(());
        };
        if let Some(content_length) = headers.get(CONTENT_LENGTH)
            && let Ok(value) = content_length.to_str()
            && let Ok(length) = value.parse::<u64>()
            && length > limit
        {
            self.notify_rejection(RejectionReason::BodyLimit);
            return Err(ErrorCode::HttpRequestBodySize(Some(limit)));
        }

        let hint = body.size_hint();
        if hint.lower() > limit
            || hint
                .upper()
                .is_some_and(|upper| upper == hint.lower() && upper > limit)
        {
            self.notify_rejection(RejectionReason::BodyLimit);
            return Err(ErrorCode::HttpRequestBodySize(Some(limit)));
        }

        Ok(())
    }

    pub(crate) fn check_response_header_limit(&self, headers: &HeaderMap) -> Result<(), ErrorCode> {
        let Some(limit) = self.header_limit else {
            return Ok(());
        };
        if header_section_size(headers) > limit {
            self.notify_rejection(RejectionReason::HeaderLimit);
            return Err(ErrorCode::HttpResponseHeaderSectionSize(Some(
                limit_to_u32(limit),
            )));
        }
        Ok(())
    }

    pub(crate) fn is_protected_request_header(&self, name: &HeaderName) -> bool {
        DEFAULT_FORBIDDEN_HEADERS.contains(name)
            || (self.forbid_sensitive_headers
                && (is_sensitive_header_name(name) || self.injected_headers.contains_key(name)))
    }

    pub(crate) fn is_denied_request_header(&self, name: &HeaderName) -> bool {
        self.denied_headers.contains(name)
    }

    pub(crate) fn request_denied(&self, reason: RejectionReason) -> ErrorCode {
        self.notify_rejection(reason);
        ErrorCode::HttpRequestDenied
    }

    fn notify_rejection(&self, reason: RejectionReason) {
        if let Some(observer) = &self.rejection_observer {
            observer(reason);
        }
    }

    pub(crate) fn deadline_remaining(&self) -> Result<Option<Duration>, ErrorCode> {
        let Some(deadline) = self.deadline else {
            return Ok(None);
        };
        let now = Instant::now();
        if deadline <= now {
            self.notify_rejection(RejectionReason::Deadline);
            return Err(ErrorCode::HttpResponseTimeout);
        }
        Ok(Some(deadline.duration_since(now)))
    }

    pub(crate) fn cap_with_deadline(&self, duration: Duration) -> Result<Duration, ErrorCode> {
        match self.deadline_remaining()? {
            Some(remaining) => Ok(duration.min(remaining)),
            None => Ok(duration),
        }
    }

    pub(crate) fn map_forward_error(&self, error: crate::Error) -> ErrorCode {
        if self.deadline_expired() && timeout_code_from_aioduct_error(&error).is_some() {
            self.notify_rejection(RejectionReason::Deadline);
        }
        map_aioduct_error(error)
    }

    pub(crate) fn deadline_expired(&self) -> bool {
        match self.deadline {
            Some(deadline) => Instant::now() >= deadline,
            None => false,
        }
    }
}

/// Build errors for [`crate::wasmtime::WasiHttpHost`].
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum BuildError {
    /// No policy was configured.
    #[error("missing WASI HTTP host policy")]
    MissingPolicy,

    /// No default transport is available for the enabled feature set.
    #[error("missing WASI HTTP host transport")]
    MissingTransport,

    /// The configured policy is invalid.
    #[error(transparent)]
    Policy(#[from] PolicyError),

    /// The native transport could not be built.
    #[error(transparent)]
    Transport(#[from] crate::Error),

    /// The local-runtime host transport worker could not be started.
    #[cfg(feature = "compio")]
    #[error("failed to start WASI HTTP local transport worker")]
    WorkerThread(#[source] std::io::Error),

    /// The local-runtime host transport worker exited before it was ready.
    #[cfg(feature = "compio")]
    #[error("WASI HTTP local transport worker exited during startup")]
    WorkerStartup,
}

/// Policy construction errors.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PolicyError {
    /// The origin string is not a valid URI.
    #[error("invalid origin: {0}")]
    InvalidOrigin(String),

    /// The origin scheme is not supported.
    #[error("origin scheme must be http or https, got {0}")]
    UnsupportedScheme(String),

    /// The origin is missing an authority.
    #[error("origin must include an authority")]
    MissingAuthority,

    /// Origins must not include path or query components.
    #[error("origin must not include a path or query")]
    OriginMustNotContainPath,

    /// A host-injected header is forbidden by WASI HTTP.
    #[error("injected header is forbidden by WASI HTTP: {0}")]
    InjectedForbiddenHeader(HeaderName),

    /// A denied header prefix is empty or not a valid HTTP header-name prefix.
    #[error("denied header prefix is invalid: {0}")]
    InvalidDeniedHeaderPrefix(&'static str),
}

/// Low-cardinality reason for a host-side request rejection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum RejectionReason {
    /// The request URI did not match the allowed origin.
    OriginMismatch,
    /// The request tried to supply a protected or sensitive header.
    ProtectedHeader,
    /// The request tried to supply a host-denied header.
    DeniedHeader,
    /// The request or response exceeded the configured header limit.
    HeaderLimit,
    /// The request or response exceeded the configured body limit.
    BodyLimit,
    /// The host deadline expired.
    Deadline,
}

impl RejectionReason {
    /// Return a stable, low-cardinality string for diagnostics and metrics.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::OriginMismatch => "origin_mismatch",
            Self::ProtectedHeader => "protected_header",
            Self::DeniedHeader => "denied_header",
            Self::HeaderLimit => "header_limit",
            Self::BodyLimit => "body_limit",
            Self::Deadline => "deadline",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Origin {
    scheme: String,
    host: String,
    port: u16,
}

impl Origin {
    fn from_uri(uri: &Uri) -> Result<Self, PolicyError> {
        let scheme = uri
            .scheme_str()
            .ok_or_else(|| PolicyError::InvalidOrigin("missing scheme".into()))?;
        if !matches!(scheme, "http" | "https") {
            return Err(PolicyError::UnsupportedScheme(scheme.into()));
        }
        let authority = uri.authority().ok_or(PolicyError::MissingAuthority)?;
        let port = authority
            .port_u16()
            .or_else(|| default_port(scheme))
            .ok_or_else(|| PolicyError::InvalidOrigin("missing port".into()))?;
        Ok(Self {
            scheme: scheme.into(),
            host: authority.host().to_ascii_lowercase(),
            port,
        })
    }
}

fn default_port(scheme: &str) -> Option<u16> {
    match scheme {
        "http" => Some(80),
        "https" => Some(443),
        _ => None,
    }
}

fn origin_uri(uri: &Uri) -> Result<Uri, PolicyError> {
    let mut parts = http::uri::Parts::default();
    parts.scheme = uri.scheme().cloned();
    parts.authority = uri.authority().cloned();
    parts.path_and_query = Some(http::uri::PathAndQuery::from_static("/"));
    Uri::from_parts(parts).map_err(|error| PolicyError::InvalidOrigin(format!("{error}")))
}

pub(crate) fn is_sensitive_header_name(name: &HeaderName) -> bool {
    name == AUTHORIZATION || name == COOKIE || name == PROXY_AUTHORIZATION
}

fn validate_denied_header_prefix(prefix: &'static str) -> Result<(), PolicyError> {
    if prefix.is_empty() || HeaderName::from_bytes(prefix.as_bytes()).is_err() {
        return Err(PolicyError::InvalidDeniedHeaderPrefix(prefix));
    }
    Ok(())
}

fn header_name_starts_with_ignore_ascii_case(name: &HeaderName, prefix: &str) -> bool {
    let name = name.as_str().as_bytes();
    let prefix = prefix.as_bytes();
    name.len() >= prefix.len() && name[..prefix.len()].eq_ignore_ascii_case(prefix)
}

pub(crate) fn header_section_size(headers: &HeaderMap) -> usize {
    headers
        .iter()
        .map(|(name, value)| name.as_str().len() + value.as_bytes().len())
        .sum()
}

pub(crate) fn limit_to_u32(limit: usize) -> u32 {
    u32::try_from(limit).unwrap_or(u32::MAX)
}

#[derive(Debug)]
pub(crate) enum RequestTrailerPolicyError {
    ProtectedHeader,
    DeniedHeader,
    HeaderLimit { limit: usize },
}

impl RequestTrailerPolicyError {
    pub(crate) fn to_error_code(&self) -> ErrorCode {
        match self {
            Self::ProtectedHeader => ErrorCode::HttpRequestDenied,
            Self::DeniedHeader => ErrorCode::HttpRequestDenied,
            Self::HeaderLimit { limit } => {
                ErrorCode::HttpRequestTrailerSectionSize(Some(limit_to_u32(*limit)))
            }
        }
    }
}

impl fmt::Display for RequestTrailerPolicyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ProtectedHeader => write!(f, "WASI request trailers contained protected header"),
            Self::DeniedHeader => write!(f, "WASI request trailers contained denied header"),
            Self::HeaderLimit { limit } => {
                write!(f, "WASI request trailers exceeded header limit {limit}")
            }
        }
    }
}

impl std::error::Error for RequestTrailerPolicyError {}

#[derive(Clone)]
pub(crate) struct RequestTrailerPolicy {
    forbid_sensitive_headers: bool,
    denied_headers: DeniedHeaderPolicy,
    pub(crate) injected_headers: HeaderMap,
    pub(crate) header_limit: Option<usize>,
}

impl RequestTrailerPolicy {
    pub(crate) fn from_policy(policy: &ExactOriginPolicy) -> Self {
        Self {
            forbid_sensitive_headers: policy.forbid_sensitive_headers,
            denied_headers: policy.denied_headers.clone(),
            injected_headers: policy.injected_headers.clone(),
            header_limit: policy.header_limit,
        }
    }

    pub(crate) fn check(
        &self,
        trailers: &HeaderMap,
        observer: &Option<RejectionObserver>,
        rejected: &mut bool,
    ) -> Result<(), crate::Error> {
        for (name, value) in trailers {
            if DEFAULT_FORBIDDEN_HEADERS.contains(name)
                || self.injected_headers.contains_key(name)
                || (self.forbid_sensitive_headers
                    && (is_sensitive_header_name(name) || value.is_sensitive()))
            {
                notify_rejection_once(observer, rejected, RejectionReason::ProtectedHeader);
                return Err(crate::Error::Other(Box::new(
                    RequestTrailerPolicyError::ProtectedHeader,
                )));
            }
            if self.is_denied_request_header(name) {
                notify_rejection_once(observer, rejected, RejectionReason::DeniedHeader);
                return Err(crate::Error::Other(Box::new(
                    RequestTrailerPolicyError::DeniedHeader,
                )));
            }
        }

        if let Some(limit) = self.header_limit
            && header_section_size(trailers) > limit
        {
            notify_rejection_once(observer, rejected, RejectionReason::HeaderLimit);
            return Err(crate::Error::Other(Box::new(
                RequestTrailerPolicyError::HeaderLimit { limit },
            )));
        }

        Ok(())
    }

    pub(crate) fn is_denied_request_header(&self, name: &HeaderName) -> bool {
        self.denied_headers.contains(name)
    }
}

pub(crate) fn notify_rejection_once(
    observer: &Option<RejectionObserver>,
    rejected: &mut bool,
    reason: RejectionReason,
) {
    if *rejected {
        return;
    }
    *rejected = true;
    if let Some(observer) = observer {
        observer(reason);
    }
}
