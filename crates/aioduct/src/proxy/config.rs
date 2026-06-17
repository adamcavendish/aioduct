use http::Uri;
use http::header::{HeaderName, HeaderValue};

use crate::error::Error;

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub(crate) enum ProxyScheme {
    Http,
    Https,
    Socks4,
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
    /// Stable hash of this proxy config for pool-key route segregation.
    pub(crate) fn route_hash(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.scheme.hash(&mut h);
        self.uri.hash(&mut h);
        self.auth.hash(&mut h);
        // Only HTTP/HTTPS proxies send a CONNECT request with headers; SOCKS
        // proxies never emit them, so ignoring headers for non-CONNECT schemes
        // avoids unnecessary pool-key fragmentation.
        if matches!(self.scheme, ProxyScheme::Http | ProxyScheme::Https) {
            for (name, value) in &self.connect_headers {
                name.as_str().hash(&mut h);
                value.as_bytes().hash(&mut h);
            }
        }
        h.finish()
    }
}

/// Wrapper that redacts userinfo when debug-printing a URI.
struct ProxyUriDebug<'a>(&'a Uri);

impl std::fmt::Debug for ProxyUriDebug<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let uri = self.0;
        write!(f, "{}://", uri.scheme_str().unwrap_or("unknown"))?;
        if let Some(authority) = uri.authority() {
            let host = authority.host();
            if let Some(port) = authority.port() {
                write!(f, "<redacted>@{host}:{port}")?;
            } else {
                write!(f, "<redacted>@{host}")?;
            }
        }
        write!(f, "{}", uri.path())?;
        if let Some(query) = uri.query() {
            write!(f, "?{query}")?;
        }
        Ok(())
    }
}

#[derive(Clone, Hash)]
pub(crate) struct ProxyAuth {
    pub username: String,
    pub password: String,
}

impl std::fmt::Debug for ProxyAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProxyAuth")
            .field("username", &self.username)
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
        if url.is_empty() {
            return None;
        }
        // Known scheme prefixes
        if url.starts_with("socks5h://") {
            return Self::socks5h(url).ok();
        }
        if url.starts_with("socks5://") {
            return Self::socks5(url).ok();
        }
        if url.starts_with("socks4://") || url.starts_with("socks4a://") {
            return Self::socks4(url).ok();
        }
        if url.starts_with("https://") {
            return Self::https(url).ok();
        }
        if url.starts_with("http://") {
            return Self::http(url).ok();
        }
        // Bare hostname: try http://
        if !url.contains("://") {
            let with_scheme = format!("http://{url}");
            return Self::http(&with_scheme).ok();
        }
        // Unknown scheme: try each constructor as fallback
        Self::http(url)
            .or_else(|_| Self::https(url))
            .or_else(|_| Self::socks4(url))
            .or_else(|_| Self::socks5(url))
            .or_else(|_| Self::socks5h(url))
            .ok()
    }

    /// Create a proxy config from an `http://` URI.
    pub fn http(uri: &str) -> Result<Self, Error> {
        let uri: Uri = uri.parse().map_err(|e| Error::InvalidUrl(format!("{e}")))?;
        if uri.scheme_str() != Some("http") {
            return Err(Error::InvalidUrl(
                "proxy URI must use http:// scheme".into(),
            ));
        }
        let auth = extract_uri_auth(&uri);
        Ok(Self {
            uri,
            scheme: ProxyScheme::Http,
            auth,
            connect_headers: Vec::new(),
        })
    }

    /// Create a proxy config from a `socks5://` URI.
    pub fn socks5(uri: &str) -> Result<Self, Error> {
        let uri: Uri = uri.parse().map_err(|e| Error::InvalidUrl(format!("{e}")))?;
        if uri.scheme_str() != Some("socks5") {
            return Err(Error::InvalidUrl(
                "SOCKS5 proxy URI must use socks5:// scheme".into(),
            ));
        }
        let auth = extract_uri_auth(&uri);
        Ok(Self {
            uri,
            scheme: ProxyScheme::Socks5,
            auth,
            connect_headers: Vec::new(),
        })
    }

    /// Create a proxy config from a `socks4://` or `socks4a://` URI.
    pub fn socks4(uri: &str) -> Result<Self, Error> {
        let uri: Uri = uri.parse().map_err(|e| Error::InvalidUrl(format!("{e}")))?;
        match uri.scheme_str() {
            Some("socks4") | Some("socks4a") => {}
            _ => {
                return Err(Error::InvalidUrl(
                    "SOCKS4 proxy URI must use socks4:// or socks4a:// scheme".into(),
                ));
            }
        }
        let auth = extract_uri_auth(&uri);
        Ok(Self {
            uri,
            scheme: ProxyScheme::Socks4,
            auth,
            connect_headers: Vec::new(),
        })
    }

    /// Create a proxy config from a `socks5h://` URI (proxy resolves DNS).
    pub fn socks5h(uri: &str) -> Result<Self, Error> {
        let uri: Uri = uri.parse().map_err(|e| Error::InvalidUrl(format!("{e}")))?;
        if uri.scheme_str() != Some("socks5h") {
            return Err(Error::InvalidUrl(
                "SOCKS5h proxy URI must use socks5h:// scheme".into(),
            ));
        }
        let auth = extract_uri_auth(&uri);
        Ok(Self {
            uri,
            scheme: ProxyScheme::Socks5h,
            auth,
            connect_headers: Vec::new(),
        })
    }

    /// Create a proxy config from an `https://` URI (TLS connection to proxy).
    pub fn https(uri: &str) -> Result<Self, Error> {
        let uri: Uri = uri.parse().map_err(|e| Error::InvalidUrl(format!("{e}")))?;
        if uri.scheme_str() != Some("https") {
            return Err(Error::InvalidUrl(
                "HTTPS proxy URI must use https:// scheme".into(),
            ));
        }
        let auth = extract_uri_auth(&uri);
        Ok(Self {
            uri,
            scheme: ProxyScheme::Https,
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
    /// proxies have no header phase and ignore these. Useful for proxy auth
    /// tokens or routing headers beyond basic auth. May be called repeatedly.
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
            ProxyScheme::Socks4 => 1080,
            ProxyScheme::Socks5 => 1080,
            ProxyScheme::Socks5h => 1080,
        }
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
