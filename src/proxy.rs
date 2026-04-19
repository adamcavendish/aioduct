use http::Uri;

use crate::error::{Error, Result};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ProxyScheme {
    Http,
    Socks5,
}

/// Proxy configuration (HTTP or SOCKS5).
#[derive(Clone, Debug)]
pub struct ProxyConfig {
    pub(crate) uri: Uri,
    pub(crate) scheme: ProxyScheme,
    pub(crate) auth: Option<ProxyAuth>,
}

#[derive(Clone, Debug)]
pub(crate) struct ProxyAuth {
    pub username: String,
    pub password: String,
}

impl ProxyConfig {
    /// Create a proxy config from an `http://` URI.
    pub fn http(uri: &str) -> Result<Self> {
        let uri: Uri = uri.parse().map_err(|e| Error::InvalidUrl(format!("{e}")))?;
        if uri.scheme_str() != Some("http") {
            return Err(Error::InvalidUrl(
                "proxy URI must use http:// scheme".into(),
            ));
        }
        Ok(Self {
            uri,
            scheme: ProxyScheme::Http,
            auth: None,
        })
    }

    /// Create a proxy config from a `socks5://` URI.
    pub fn socks5(uri: &str) -> Result<Self> {
        let uri: Uri = uri.parse().map_err(|e| Error::InvalidUrl(format!("{e}")))?;
        if uri.scheme_str() != Some("socks5") {
            return Err(Error::InvalidUrl(
                "SOCKS5 proxy URI must use socks5:// scheme".into(),
            ));
        }
        Ok(Self {
            uri,
            scheme: ProxyScheme::Socks5,
            auth: None,
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

    pub(crate) fn authority(&self) -> Result<&http::uri::Authority> {
        self.uri
            .authority()
            .ok_or_else(|| Error::InvalidUrl("proxy URI missing authority".into()))
    }

    pub(crate) fn default_port(&self) -> u16 {
        match self.scheme {
            ProxyScheme::Http => 80,
            ProxyScheme::Socks5 => 1080,
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

/// Proxy settings with separate HTTP/HTTPS proxies and bypass rules.
#[derive(Clone, Debug, Default)]
pub struct ProxySettings {
    pub(crate) http_proxy: Option<ProxyConfig>,
    pub(crate) https_proxy: Option<ProxyConfig>,
    pub(crate) no_proxy: NoProxy,
}

impl ProxySettings {
    /// Read proxy settings from environment variables.
    ///
    /// Reads `HTTP_PROXY` / `http_proxy`, `HTTPS_PROXY` / `https_proxy`,
    /// and `NO_PROXY` / `no_proxy`. The uppercase variant takes precedence.
    pub fn from_env() -> Self {
        let http_proxy = env_proxy("HTTP_PROXY", "http_proxy");
        let https_proxy = env_proxy("HTTPS_PROXY", "https_proxy");
        let no_proxy = NoProxy::from_env();
        Self {
            http_proxy,
            https_proxy,
            no_proxy,
        }
    }

    /// Create settings with a single proxy for both HTTP and HTTPS.
    pub fn all(proxy: ProxyConfig) -> Self {
        Self {
            http_proxy: Some(proxy.clone()),
            https_proxy: Some(proxy),
            no_proxy: NoProxy::default(),
        }
    }

    /// Set the HTTP proxy.
    pub fn http(mut self, proxy: ProxyConfig) -> Self {
        self.http_proxy = Some(proxy);
        self
    }

    /// Set the HTTPS proxy.
    pub fn https(mut self, proxy: ProxyConfig) -> Self {
        self.https_proxy = Some(proxy);
        self
    }

    /// Set the no-proxy bypass rules.
    pub fn no_proxy(mut self, no_proxy: NoProxy) -> Self {
        self.no_proxy = no_proxy;
        self
    }

    pub(crate) fn proxy_for(&self, uri: &Uri) -> Option<&ProxyConfig> {
        if let Some(host) = uri.host() {
            if self.no_proxy.matches(host) {
                return None;
            }
        }
        match uri.scheme_str() {
            Some("https") => self.https_proxy.as_ref(),
            _ => self.http_proxy.as_ref(),
        }
    }
}

/// Rules for bypassing the proxy for certain hosts.
#[derive(Clone, Debug, Default)]
pub struct NoProxy {
    rules: Vec<String>,
}

impl NoProxy {
    /// Parse a comma-separated list of no-proxy rules.
    ///
    /// Each rule can be:
    /// - A hostname: `example.com`
    /// - A domain suffix: `.example.com` (matches any subdomain)
    /// - A wildcard: `*` (matches everything)
    /// - An IP address: `127.0.0.1`
    /// - A CIDR (stored as-is, matched literally against the host string)
    pub fn new(rules: &str) -> Self {
        let rules: Vec<String> = rules
            .split(',')
            .map(|s| s.trim().to_lowercase())
            .filter(|s| !s.is_empty())
            .collect();
        Self { rules }
    }

    fn from_env() -> Self {
        let val = std::env::var("NO_PROXY")
            .or_else(|_| std::env::var("no_proxy"))
            .unwrap_or_default();
        Self::new(&val)
    }

    pub fn matches(&self, host: &str) -> bool {
        let host = host.to_lowercase();
        for rule in &self.rules {
            if rule == "*" {
                return true;
            }
            if rule == &host {
                return true;
            }
            // .example.com matches foo.example.com
            if rule.starts_with('.') && host.ends_with(rule.as_str()) {
                return true;
            }
            // example.com also matches foo.example.com
            if !rule.starts_with('.') && host.ends_with(&format!(".{rule}")) {
                return true;
            }
        }
        false
    }
}

fn env_proxy(upper: &str, lower: &str) -> Option<ProxyConfig> {
    let val = std::env::var(upper)
        .or_else(|_| std::env::var(lower))
        .ok()?;
    if val.is_empty() {
        return None;
    }
    if val.starts_with("socks5://") {
        ProxyConfig::socks5(&val).ok()
    } else {
        ProxyConfig::http(&val).ok()
    }
}
