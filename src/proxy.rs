use std::sync::Arc;

use http::Uri;

use crate::error::{Error, Result};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ProxyScheme {
    Http,
    Socks4,
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

    /// Create a proxy config from a `socks4://` or `socks4a://` URI.
    pub fn socks4(uri: &str) -> Result<Self> {
        let uri: Uri = uri.parse().map_err(|e| Error::InvalidUrl(format!("{e}")))?;
        match uri.scheme_str() {
            Some("socks4") | Some("socks4a") => {}
            _ => {
                return Err(Error::InvalidUrl(
                    "SOCKS4 proxy URI must use socks4:// or socks4a:// scheme".into(),
                ));
            }
        }
        Ok(Self {
            uri,
            scheme: ProxyScheme::Socks4,
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
            ProxyScheme::Socks4 => 1080,
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
#[derive(Clone, Default)]
pub struct ProxySettings {
    pub(crate) http_proxy: Option<ProxyConfig>,
    pub(crate) https_proxy: Option<ProxyConfig>,
    pub(crate) no_proxy: NoProxy,
    pub(crate) custom: Option<Arc<dyn CustomProxy>>,
}

impl std::fmt::Debug for ProxySettings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProxySettings")
            .field("http_proxy", &self.http_proxy)
            .field("https_proxy", &self.https_proxy)
            .field("no_proxy", &self.no_proxy)
            .field("custom", &self.custom.as_ref().map(|_| ".."))
            .finish()
    }
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
            custom: None,
        }
    }

    /// Create settings with a single proxy for both HTTP and HTTPS.
    pub fn all(proxy: ProxyConfig) -> Self {
        Self {
            http_proxy: Some(proxy.clone()),
            https_proxy: Some(proxy),
            no_proxy: NoProxy::default(),
            custom: None,
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

    /// Set a custom proxy selection function.
    ///
    /// The closure receives the request URI and returns `Some(ProxyConfig)` to
    /// proxy through the given server, or `None` for a direct connection.
    /// This takes priority over `http`/`https` proxy settings.
    pub fn custom(
        mut self,
        f: impl Fn(&Uri) -> Option<ProxyConfig> + Send + Sync + 'static,
    ) -> Self {
        self.custom = Some(Arc::new(f));
        self
    }

    pub(crate) fn proxy_for(&self, uri: &Uri) -> Option<ProxyConfig> {
        if let Some(ref custom) = self.custom {
            return custom.proxy_for(uri);
        }
        if let Some(host) = uri.host() {
            if self.no_proxy.matches(host) {
                return None;
            }
        }
        match uri.scheme_str() {
            Some("https") => self.https_proxy.clone(),
            _ => self.http_proxy.clone(),
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
    } else if val.starts_with("socks4://") || val.starts_with("socks4a://") {
        ProxyConfig::socks4(&val).ok()
    } else {
        ProxyConfig::http(&val).ok()
    }
}

/// Trait for custom proxy selection logic.
pub trait CustomProxy: Send + Sync + 'static {
    /// Given a request URI, return a proxy config or `None` for direct connection.
    fn proxy_for(&self, uri: &Uri) -> Option<ProxyConfig>;
}

impl<F> CustomProxy for F
where
    F: Fn(&Uri) -> Option<ProxyConfig> + Send + Sync + 'static,
{
    fn proxy_for(&self, uri: &Uri) -> Option<ProxyConfig> {
        (self)(uri)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_proxy_wildcard_matches_everything() {
        let np = NoProxy::new("*");
        assert!(np.matches("anything.example.com"));
        assert!(np.matches("127.0.0.1"));
    }

    #[test]
    fn no_proxy_exact_match() {
        let np = NoProxy::new("example.com");
        assert!(np.matches("example.com"));
        assert!(!np.matches("other.com"));
    }

    #[test]
    fn no_proxy_suffix_with_leading_dot() {
        let np = NoProxy::new(".example.com");
        assert!(np.matches("sub.example.com"));
        assert!(np.matches("deep.sub.example.com"));
        assert!(!np.matches("example.com"));
    }

    #[test]
    fn no_proxy_suffix_without_leading_dot() {
        let np = NoProxy::new("example.com");
        assert!(np.matches("sub.example.com"));
        assert!(np.matches("example.com"));
    }

    #[test]
    fn no_proxy_case_insensitive() {
        let np = NoProxy::new("Example.COM");
        assert!(np.matches("EXAMPLE.com"));
        assert!(np.matches("example.com"));
    }

    #[test]
    fn no_proxy_multiple_rules() {
        let np = NoProxy::new("a.com, b.com, .c.com");
        assert!(np.matches("a.com"));
        assert!(np.matches("b.com"));
        assert!(np.matches("sub.c.com"));
        assert!(!np.matches("d.com"));
    }

    #[test]
    fn no_proxy_ip_address() {
        let np = NoProxy::new("127.0.0.1");
        assert!(np.matches("127.0.0.1"));
        assert!(!np.matches("127.0.0.2"));
    }

    #[test]
    fn no_proxy_empty_matches_nothing() {
        let np = NoProxy::new("");
        assert!(!np.matches("anything"));
    }

    #[test]
    fn proxy_config_http_valid() {
        let cfg = ProxyConfig::http("http://proxy:8080").unwrap();
        assert_eq!(cfg.scheme, ProxyScheme::Http);
        assert_eq!(cfg.default_port(), 80);
    }

    #[test]
    fn proxy_config_http_wrong_scheme() {
        assert!(ProxyConfig::http("https://proxy:8080").is_err());
    }

    #[test]
    fn proxy_config_socks5_valid() {
        let cfg = ProxyConfig::socks5("socks5://proxy:1080").unwrap();
        assert_eq!(cfg.scheme, ProxyScheme::Socks5);
        assert_eq!(cfg.default_port(), 1080);
    }

    #[test]
    fn proxy_config_socks5_wrong_scheme() {
        assert!(ProxyConfig::socks5("http://proxy:1080").is_err());
    }

    #[test]
    fn proxy_config_socks4_valid() {
        let cfg = ProxyConfig::socks4("socks4://proxy:1080").unwrap();
        assert_eq!(cfg.scheme, ProxyScheme::Socks4);
        assert_eq!(cfg.default_port(), 1080);
    }

    #[test]
    fn proxy_config_socks4a_valid() {
        let cfg = ProxyConfig::socks4("socks4a://proxy:1080").unwrap();
        assert_eq!(cfg.scheme, ProxyScheme::Socks4);
    }

    #[test]
    fn proxy_config_socks4_wrong_scheme() {
        assert!(ProxyConfig::socks4("http://proxy").is_err());
    }

    #[test]
    fn proxy_config_basic_auth() {
        let cfg = ProxyConfig::http("http://proxy:8080")
            .unwrap()
            .basic_auth("user", "pass");
        let header = cfg.connect_header("target:443");
        assert!(header.is_some());
        assert!(header.unwrap().starts_with("Basic "));
    }

    #[test]
    fn proxy_config_no_auth_connect_header() {
        let cfg = ProxyConfig::http("http://proxy:8080").unwrap();
        assert!(cfg.connect_header("target:443").is_none());
    }

    #[test]
    fn proxy_config_authority() {
        let cfg = ProxyConfig::http("http://proxy:8080").unwrap();
        let auth = cfg.authority().unwrap();
        assert_eq!(auth.to_string(), "proxy:8080");
    }

    #[test]
    fn proxy_settings_all() {
        let proxy = ProxyConfig::http("http://proxy:8080").unwrap();
        let settings = ProxySettings::all(proxy);
        assert!(settings.http_proxy.is_some());
        assert!(settings.https_proxy.is_some());
    }

    #[test]
    fn proxy_settings_builder() {
        let settings = ProxySettings::default()
            .http(ProxyConfig::http("http://h:80").unwrap())
            .https(ProxyConfig::http("http://s:80").unwrap())
            .no_proxy(NoProxy::new("localhost"));
        assert!(settings.http_proxy.is_some());
        assert!(settings.https_proxy.is_some());
        assert!(settings.no_proxy.matches("localhost"));
    }

    #[test]
    fn proxy_for_no_proxy_bypass() {
        let settings = ProxySettings::all(ProxyConfig::http("http://p:80").unwrap())
            .no_proxy(NoProxy::new("localhost"));
        let uri: Uri = "http://localhost/path".parse().unwrap();
        assert!(settings.proxy_for(&uri).is_none());

        let uri: Uri = "http://other.com/path".parse().unwrap();
        assert!(settings.proxy_for(&uri).is_some());
    }

    #[test]
    fn proxy_for_scheme_dispatch() {
        let settings = ProxySettings::default()
            .http(ProxyConfig::http("http://http-proxy:80").unwrap())
            .https(ProxyConfig::http("http://https-proxy:80").unwrap());

        let http_uri: Uri = "http://example.com/".parse().unwrap();
        let https_uri: Uri = "https://example.com/".parse().unwrap();

        let http_proxy = settings.proxy_for(&http_uri).unwrap();
        assert!(http_proxy.uri.to_string().contains("http-proxy"));

        let https_proxy = settings.proxy_for(&https_uri).unwrap();
        assert!(https_proxy.uri.to_string().contains("https-proxy"));
    }

    #[test]
    fn proxy_for_custom_takes_priority() {
        let settings =
            ProxySettings::all(ProxyConfig::http("http://p:80").unwrap()).custom(|_uri: &Uri| None);
        let uri: Uri = "http://example.com/".parse().unwrap();
        assert!(settings.proxy_for(&uri).is_none());
    }
}
