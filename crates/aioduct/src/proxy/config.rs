use http::Uri;
use http::header::{HeaderName, HeaderValue};
use http::uri::{Authority, Scheme};
use std::net::IpAddr;
use std::sync::Arc;

use crate::error::Error;

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub(crate) enum ProxyScheme {
    Http,
    Https,
    Socks4,
    Socks4a,
    Socks5,
    Socks5h,
}

/// Proxy configuration (HTTP or SOCKS5).
#[derive(Clone)]
pub struct ProxyConfig {
    pub(crate) uri: Uri,
    pub(crate) scheme: ProxyScheme,
    pub(crate) auth: Option<ProxyAuth>,
    /// Extra headers sent on the HTTP `CONNECT` request (HTTP/HTTPS proxies).
    pub(crate) connect_headers: Vec<(HeaderName, HeaderValue)>,
}

impl std::fmt::Debug for ProxyConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProxyConfig")
            .field("scheme", &self.scheme)
            .field("auth", &self.auth)
            .field("uri", &ProxyUriDebug(&self.uri))
            .field(
                "connect_headers",
                &self
                    .connect_headers
                    .iter()
                    .map(|(n, _)| n.as_str())
                    .collect::<Vec<_>>(),
            )
            .finish()
    }
}

impl ProxyConfig {
    pub(crate) fn route_identity(&self) -> ProxyRouteIdentity {
        ProxyRouteIdentity::from_configs(std::slice::from_ref(self))
    }
}

#[derive(Clone, Hash, Eq, PartialEq)]
struct ProxyRouteHop {
    scheme: ProxyScheme,
    endpoint: Option<ProxyRouteEndpoint>,
    auth: Option<ProxyAuth>,
    connect_headers: Vec<(HeaderName, Vec<u8>)>,
}

#[derive(Clone, Hash, Eq, PartialEq)]
struct ProxyRouteEndpoint {
    host: String,
    port: ProxyRoutePort,
}

#[derive(Clone, Hash, Eq, PartialEq)]
enum ProxyRoutePort {
    Effective(u16),
    Invalid(String),
}

impl ProxyRouteEndpoint {
    fn from_config(proxy: &ProxyConfig) -> Option<Self> {
        let authority = proxy.uri.authority()?;
        let raw_host = authority.host();
        let host = raw_host
            .strip_prefix('[')
            .and_then(|host| host.strip_suffix(']'))
            .unwrap_or(raw_host);
        let host = host
            .parse::<IpAddr>()
            .map(|address| address.to_string())
            .unwrap_or_else(|_| host.to_ascii_lowercase());
        let port = match proxy.effective_port() {
            Ok(port) => ProxyRoutePort::Effective(port),
            Err(_) => ProxyRoutePort::Invalid(endpoint_without_userinfo(authority).to_owned()),
        };
        Some(Self { host, port })
    }
}

/// Structural identity for a fully resolved proxy route.
///
/// The pool hashes this value for lookup performance, but equality always
/// compares the complete route so a digest collision cannot cross credentials
/// or proxy hops.
#[derive(Clone, Hash, Eq, PartialEq)]
pub(crate) struct ProxyRouteIdentity(Arc<[ProxyRouteHop]>);

impl ProxyRouteIdentity {
    pub(crate) fn from_configs(configs: &[ProxyConfig]) -> Self {
        Self(
            configs
                .iter()
                .map(|proxy| ProxyRouteHop {
                    scheme: proxy.scheme,
                    endpoint: ProxyRouteEndpoint::from_config(proxy),
                    auth: proxy.auth.clone(),
                    connect_headers: if matches!(
                        proxy.scheme,
                        ProxyScheme::Http | ProxyScheme::Https
                    ) {
                        proxy
                            .connect_headers
                            .iter()
                            .map(|(name, value)| (name.clone(), value.as_bytes().to_vec()))
                            .collect()
                    } else {
                        Vec::new()
                    },
                })
                .collect(),
        )
    }
}

impl std::fmt::Debug for ProxyRouteIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProxyRouteIdentity")
            .field("hops", &self.0.len())
            .finish_non_exhaustive()
    }
}

/// Wrapper that exposes only the proxy endpoint when debug-printing a URI.
struct ProxyUriDebug<'a>(&'a Uri);

impl std::fmt::Debug for ProxyUriDebug<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let uri = self.0;
        write!(f, "{}://", uri.scheme_str().unwrap_or("unknown"))?;
        if let Some(authority) = uri.authority() {
            if authority.as_str().contains('@') {
                write!(f, "<redacted>@")?;
            }
            let host = authority.host();
            if let Some(port) = authority.port() {
                write!(f, "{host}:{port}")?;
            } else {
                write!(f, "{host}")?;
            }
        }
        Ok(())
    }
}

#[derive(Clone, Hash, Eq, PartialEq)]
pub(crate) struct ProxyAuth {
    pub username: String,
    pub password: String,
}

impl std::fmt::Debug for ProxyAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProxyAuth")
            .field("username", &"[redacted]")
            .field("password", &"[redacted]")
            .finish()
    }
}

/// Extract `user:password@host` credentials from the URI authority section.
///
/// Returns `None` if no userinfo is present. Percent-decodes both the username
/// and password components (e.g. `%40` → `@`).
pub(super) fn extract_uri_auth(uri: &Uri) -> Option<ProxyAuth> {
    let authority = uri.authority()?.as_str();
    let at_pos = authority.rfind('@')?;
    let userinfo = &authority[..at_pos];
    if userinfo.is_empty() {
        return None;
    }
    if let Some(colon_pos) = userinfo.find(':') {
        let username = percent_encoding::percent_decode_str(&userinfo[..colon_pos])
            .decode_utf8_lossy()
            .into_owned();
        let password = percent_encoding::percent_decode_str(&userinfo[colon_pos + 1..])
            .decode_utf8_lossy()
            .into_owned();
        Some(ProxyAuth { username, password })
    } else {
        let username = percent_encoding::percent_decode_str(userinfo)
            .decode_utf8_lossy()
            .into_owned();
        Some(ProxyAuth {
            username,
            password: String::new(),
        })
    }
}

impl ProxyConfig {
    /// Detect proxy type from a URL string, trying each supported scheme.
    ///
    /// If the URL has no scheme, `http://` is prepended. Returns `None` if
    /// the URL cannot be parsed as any supported proxy scheme.
    pub fn detect_from_url(url: &str) -> Option<Self> {
        Self::try_detect_from_url(url).ok()
    }

    pub(crate) fn try_detect_from_url(url: &str) -> Result<Self, Error> {
        if url.is_empty() {
            return Err(Error::InvalidUrl("proxy URL must not be empty".into()));
        }
        let Some((scheme, _)) = url.split_once("://") else {
            let with_scheme = format!("http://{url}");
            return Self::http(&with_scheme);
        };
        if scheme.eq_ignore_ascii_case("http") {
            Self::http(url)
        } else if scheme.eq_ignore_ascii_case("https") {
            Self::https(url)
        } else if scheme.eq_ignore_ascii_case("socks4") || scheme.eq_ignore_ascii_case("socks4a") {
            Self::socks4(url)
        } else if scheme.eq_ignore_ascii_case("socks5") {
            Self::socks5(url)
        } else if scheme.eq_ignore_ascii_case("socks5h") {
            Self::socks5h(url)
        } else {
            Err(Error::InvalidUrl(format!(
                "unsupported proxy URL scheme `{scheme}`"
            )))
        }
    }

    /// Create a proxy config from an `http://` URI.
    pub fn http(uri: &str) -> Result<Self, Error> {
        Self::parse(
            uri,
            &[("http", ProxyScheme::Http)],
            "proxy URI must use http:// scheme",
        )
    }

    /// Create a proxy config from a `socks5://` URI.
    pub fn socks5(uri: &str) -> Result<Self, Error> {
        Self::parse(
            uri,
            &[("socks5", ProxyScheme::Socks5)],
            "SOCKS5 proxy URI must use socks5:// scheme",
        )
    }

    /// Create a proxy config from a `socks4://` or `socks4a://` URI.
    pub fn socks4(uri: &str) -> Result<Self, Error> {
        Self::parse(
            uri,
            &[
                ("socks4", ProxyScheme::Socks4),
                ("socks4a", ProxyScheme::Socks4a),
            ],
            "SOCKS4 proxy URI must use socks4:// or socks4a:// scheme",
        )
    }

    /// Create a proxy config from a `socks5h://` URI (proxy resolves DNS).
    pub fn socks5h(uri: &str) -> Result<Self, Error> {
        Self::parse(
            uri,
            &[("socks5h", ProxyScheme::Socks5h)],
            "SOCKS5h proxy URI must use socks5h:// scheme",
        )
    }

    /// Create a proxy config from an `https://` URI (TLS connection to proxy).
    pub fn https(uri: &str) -> Result<Self, Error> {
        Self::parse(
            uri,
            &[("https", ProxyScheme::Https)],
            "HTTPS proxy URI must use https:// scheme",
        )
    }

    fn parse(
        value: &str,
        accepted_schemes: &[(&str, ProxyScheme)],
        wrong_scheme: &str,
    ) -> Result<Self, Error> {
        let uri: Uri = value
            .parse::<Uri>()
            .map_err(|error| Error::InvalidUrl(error.to_string()))?;
        let raw_scheme = uri
            .scheme_str()
            .ok_or_else(|| Error::InvalidUrl(wrong_scheme.to_owned()))?;
        let (canonical_scheme, scheme) = accepted_schemes
            .iter()
            .copied()
            .find(|(candidate, _)| raw_scheme.eq_ignore_ascii_case(candidate))
            .ok_or_else(|| Error::InvalidUrl(wrong_scheme.to_owned()))?;
        validate_proxy_authority(&uri)?;
        let uri = canonicalize_scheme(uri, canonical_scheme)?;
        let auth = extract_uri_auth(&uri);
        Ok(Self {
            uri,
            scheme,
            auth,
            connect_headers: Vec::new(),
        })
    }

    /// Set basic authentication credentials for the proxy.
    pub fn basic_auth(mut self, username: &str, password: &str) -> Self {
        self.auth = Some(ProxyAuth {
            username: username.to_owned(),
            password: password.to_owned(),
        });
        self
    }

    /// Add an extra header to the HTTP `CONNECT` request sent to the proxy.
    ///
    /// Applies to HTTP and HTTPS proxies (which tunnel via `CONNECT`); SOCKS
    /// proxies have no header phase and fail explicitly when used with CONNECT
    /// headers. Useful for proxy auth tokens or routing headers beyond basic
    /// auth. May be called repeatedly.
    pub fn header(mut self, name: HeaderName, value: HeaderValue) -> Self {
        self.connect_headers.push((name, value));
        self
    }

    pub(crate) fn authority(&self) -> Result<&http::uri::Authority, Error> {
        self.uri
            .authority()
            .ok_or_else(|| Error::InvalidUrl("proxy URI missing authority".into()))
    }

    pub(crate) fn default_port(&self) -> u16 {
        match self.scheme {
            ProxyScheme::Http => 80,
            ProxyScheme::Https => 443,
            ProxyScheme::Socks4 | ProxyScheme::Socks4a => 1080,
            ProxyScheme::Socks5 => 1080,
            ProxyScheme::Socks5h => 1080,
        }
    }

    pub(crate) fn effective_port(&self) -> Result<u16, Error> {
        let authority = self.authority()?;
        Ok(explicit_proxy_port(authority)?.unwrap_or_else(|| self.default_port()))
    }

    pub(crate) fn validate_for_use(&self) -> Result<(), Error> {
        self.effective_port()?;
        if !self.connect_headers.is_empty()
            && !matches!(self.scheme, ProxyScheme::Http | ProxyScheme::Https)
        {
            return Err(Error::Unsupported(
                "CONNECT headers are only supported by HTTP and HTTPS proxies".into(),
            ));
        }

        if matches!(self.scheme, ProxyScheme::Http | ProxyScheme::Https) {
            let mut custom_proxy_authorization = false;
            for (name, value) in &self.connect_headers {
                if value.to_str().is_err() {
                    return Err(Error::InvalidHeader(
                        "HTTP CONNECT headers must contain textual field values".into(),
                    ));
                }
                if is_reserved_connect_header(name) {
                    return Err(Error::InvalidHeader(format!(
                        "HTTP CONNECT header `{name}` is controlled by aioduct"
                    )));
                }
                if name == http::header::PROXY_AUTHORIZATION {
                    if self.auth.is_some() || custom_proxy_authorization {
                        return Err(Error::InvalidHeader(
                            "HTTP CONNECT must have only one Proxy-Authorization source".into(),
                        ));
                    }
                    custom_proxy_authorization = true;
                }
            }
        }

        if let Some(auth) = &self.auth {
            match self.scheme {
                ProxyScheme::Socks4 | ProxyScheme::Socks4a
                    if auth.username.as_bytes().contains(&0) =>
                {
                    return Err(Error::Unsupported(
                        "SOCKS4 user IDs cannot contain NUL bytes".into(),
                    ));
                }
                ProxyScheme::Socks5 | ProxyScheme::Socks5h
                    if auth.username.len() > u8::MAX as usize
                        || auth.password.len() > u8::MAX as usize =>
                {
                    return Err(Error::Unsupported(
                        "SOCKS5 usernames and passwords must not exceed 255 bytes".into(),
                    ));
                }
                _ => {}
            }
        }
        Ok(())
    }

    pub(crate) fn connect_header(&self, _target_authority: &str) -> Option<String> {
        self.auth.as_ref().map(|auth| {
            use base64::engine::{Engine, general_purpose::STANDARD};
            let credentials = format!("{}:{}", auth.username, auth.password);
            let encoded = STANDARD.encode(credentials);
            format!("Basic {encoded}")
        })
    }
}

fn canonicalize_scheme(uri: Uri, canonical_scheme: &str) -> Result<Uri, Error> {
    if uri.scheme_str() == Some(canonical_scheme) {
        return Ok(uri);
    }
    let mut parts = uri.into_parts();
    parts.scheme = Some(
        canonical_scheme
            .parse::<Scheme>()
            .map_err(|error| Error::InvalidUrl(error.to_string()))?,
    );
    Uri::from_parts(parts).map_err(|error| Error::InvalidUrl(error.to_string()))
}

fn validate_proxy_authority(uri: &Uri) -> Result<(), Error> {
    let authority = uri
        .authority()
        .ok_or_else(|| Error::InvalidUrl("proxy URI missing authority".into()))?;
    explicit_proxy_port(authority).map(|_| ())
}

pub(super) fn explicit_proxy_port(authority: &Authority) -> Result<Option<u16>, Error> {
    let endpoint = endpoint_without_userinfo(authority);
    let raw_port = if let Some(bracketed) = endpoint.strip_prefix('[') {
        let closing = bracketed
            .find(']')
            .ok_or_else(|| invalid_proxy_port(endpoint))?;
        if closing == 0 {
            return Err(invalid_proxy_port(endpoint));
        }
        match &bracketed[closing + 1..] {
            "" => None,
            suffix if suffix.starts_with(':') => Some(&suffix[1..]),
            _ => return Err(invalid_proxy_port(endpoint)),
        }
    } else {
        if endpoint.is_empty() || endpoint.contains('[') || endpoint.contains(']') {
            return Err(invalid_proxy_port(endpoint));
        }
        match endpoint.rsplit_once(':') {
            Some((host, port)) if !host.is_empty() && !host.contains(':') => Some(port),
            Some(_) => return Err(invalid_proxy_port(endpoint)),
            None => None,
        }
    };

    let Some(raw_port) = raw_port else {
        return Ok(None);
    };
    if raw_port.is_empty() || !raw_port.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(invalid_proxy_port(endpoint));
    }
    let port = raw_port
        .parse::<u16>()
        .map_err(|_| invalid_proxy_port(endpoint))?;
    if authority.port_u16() != Some(port) {
        return Err(invalid_proxy_port(endpoint));
    }
    Ok(Some(port))
}

fn endpoint_without_userinfo(authority: &Authority) -> &str {
    authority
        .as_str()
        .rsplit_once('@')
        .map_or_else(|| authority.as_str(), |(_, endpoint)| endpoint)
}

fn invalid_proxy_port(endpoint: &str) -> Error {
    Error::InvalidUrl(format!(
        "invalid proxy endpoint port in authority `{endpoint}`"
    ))
}

fn is_reserved_connect_header(name: &HeaderName) -> bool {
    matches!(
        name.as_str(),
        "authorization"
            | "connection"
            | "content-length"
            | "expect"
            | "host"
            | "http2-settings"
            | "keep-alive"
            | "proxy-connection"
            | "te"
            | "trailer"
            | "transfer-encoding"
            | "upgrade"
    )
}
