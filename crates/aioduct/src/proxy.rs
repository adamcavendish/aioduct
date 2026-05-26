use std::sync::Arc;

use http::Uri;

use crate::error::Error;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ProxyScheme {
    Http,
    Https,
    Socks4,
    Socks5,
    Socks5h,
}

/// Proxy configuration (HTTP or SOCKS5).
#[derive(Clone, Debug)]
pub struct ProxyConfig {
    pub(crate) uri: Uri,
    pub(crate) scheme: ProxyScheme,
    pub(crate) auth: Option<ProxyAuth>,
}

#[derive(Clone)]
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

impl ProxyConfig {
    /// Create a proxy config from an `http://` URI.
    pub fn http(uri: &str) -> Result<Self, Error> {
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
    pub fn socks5(uri: &str) -> Result<Self, Error> {
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
        Ok(Self {
            uri,
            scheme: ProxyScheme::Socks4,
            auth: None,
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
        Ok(Self {
            uri,
            scheme: ProxyScheme::Socks5h,
            auth: None,
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
        Ok(Self {
            uri,
            scheme: ProxyScheme::Https,
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
        if let Some(host) = uri.host()
            && self.no_proxy.matches_with_port(host, uri.port_u16())
        {
            return None;
        }
        if let Some(ref custom) = self.custom {
            return custom.proxy_for(uri);
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

    /// Returns `true` if the given host (and optional port) matches any bypass rule.
    pub fn matches(&self, host: &str) -> bool {
        self.matches_with_port(host, None)
    }

    /// Returns `true` if the given host:port matches any bypass rule.
    pub(crate) fn matches_with_port(&self, host: &str, port: Option<u16>) -> bool {
        let host = host.to_lowercase();
        for rule in &self.rules {
            if rule == "*" {
                return true;
            }

            let (rule_host, rule_port) = if let Some((h, p)) = rule.rsplit_once(':') {
                if let Ok(port_num) = p.parse::<u16>() {
                    (h, Some(port_num))
                } else {
                    (rule.as_str(), None)
                }
            } else {
                (rule.as_str(), None)
            };

            if rule_port.is_some() && rule_port != port {
                continue;
            }

            if rule_host == host {
                return true;
            }
            if rule_host.starts_with('.') && host.ends_with(rule_host) {
                return true;
            }
            if !rule_host.starts_with('.') && host.ends_with(&format!(".{rule_host}")) {
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
    if val.starts_with("socks5h://") {
        ProxyConfig::socks5h(&val).ok()
    } else if val.starts_with("socks5://") {
        ProxyConfig::socks5(&val).ok()
    } else if val.starts_with("socks4://") || val.starts_with("socks4a://") {
        ProxyConfig::socks4(&val).ok()
    } else if val.starts_with("https://") {
        ProxyConfig::https(&val).ok()
    } else {
        let val = if !val.contains("://") {
            format!("http://{val}")
        } else {
            val
        };
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
    fn no_proxy_with_port_matches_specific_port() {
        let np = NoProxy::new("example.com:8080");
        assert!(np.matches_with_port("example.com", Some(8080)));
        assert!(!np.matches_with_port("example.com", Some(9090)));
        assert!(!np.matches_with_port("example.com", None));
    }

    #[test]
    fn no_proxy_without_port_matches_any_port() {
        let np = NoProxy::new("example.com");
        assert!(np.matches_with_port("example.com", Some(8080)));
        assert!(np.matches_with_port("example.com", Some(443)));
        assert!(np.matches_with_port("example.com", None));
    }

    #[test]
    fn no_proxy_port_with_subdomain() {
        let np = NoProxy::new(".example.com:8080");
        assert!(np.matches_with_port("sub.example.com", Some(8080)));
        assert!(!np.matches_with_port("sub.example.com", Some(9090)));
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
    fn proxy_config_socks5h_valid() {
        let cfg = ProxyConfig::socks5h("socks5h://proxy:1080").unwrap();
        assert_eq!(cfg.scheme, ProxyScheme::Socks5h);
        assert_eq!(cfg.default_port(), 1080);
    }

    #[test]
    fn proxy_config_socks5h_wrong_scheme() {
        assert!(ProxyConfig::socks5h("socks5://proxy:1080").is_err());
        assert!(ProxyConfig::socks5h("http://proxy:1080").is_err());
    }

    #[test]
    fn proxy_config_https_valid() {
        let cfg = ProxyConfig::https("https://proxy:443").unwrap();
        assert_eq!(cfg.scheme, ProxyScheme::Https);
        assert_eq!(cfg.default_port(), 443);
    }

    #[test]
    fn proxy_config_https_wrong_scheme() {
        assert!(ProxyConfig::https("http://proxy:443").is_err());
        assert!(ProxyConfig::https("socks5://proxy:443").is_err());
    }

    #[test]
    fn env_proxy_https_parsed_correctly() {
        // Verify https:// proxy URLs map to ProxyScheme::Https (not Http)
        let cfg = ProxyConfig::https("https://secure-proxy.example.com:443").unwrap();
        assert_eq!(cfg.scheme, ProxyScheme::Https);
    }

    #[test]
    fn env_proxy_socks5h_parsed_correctly() {
        let cfg = ProxyConfig::socks5h("socks5h://dns-proxy.example.com:1080").unwrap();
        assert_eq!(cfg.scheme, ProxyScheme::Socks5h);
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

    #[test]
    fn proxy_config_http_invalid_uri() {
        assert!(ProxyConfig::http("://bad").is_err());
    }

    #[test]
    fn proxy_config_socks5_invalid_uri() {
        assert!(ProxyConfig::socks5("://bad").is_err());
    }

    #[test]
    fn proxy_config_socks4_invalid_uri() {
        assert!(ProxyConfig::socks4("://bad").is_err());
    }

    #[test]
    fn proxy_settings_debug() {
        let settings = ProxySettings::all(ProxyConfig::http("http://p:80").unwrap());
        let dbg = format!("{settings:?}");
        assert!(dbg.contains("ProxySettings"));
        assert!(dbg.contains("http_proxy"));
        assert!(dbg.contains("https_proxy"));
        assert!(dbg.contains("no_proxy"));
    }

    #[test]
    fn proxy_settings_debug_with_custom() {
        let settings =
            ProxySettings::all(ProxyConfig::http("http://p:80").unwrap()).custom(|_: &Uri| None);
        let dbg = format!("{settings:?}");
        assert!(dbg.contains("custom"));
    }

    #[test]
    fn proxy_for_unknown_scheme_uses_http_proxy() {
        let settings = ProxySettings::default()
            .http(ProxyConfig::http("http://http-proxy:80").unwrap())
            .https(ProxyConfig::http("http://https-proxy:80").unwrap());
        let uri: Uri = "ftp://example.com/".parse().unwrap();
        let proxy = settings.proxy_for(&uri).unwrap();
        assert!(proxy.uri.to_string().contains("http-proxy"));
    }

    #[test]
    fn proxy_for_no_host_still_checks_scheme() {
        let settings = ProxySettings::default().http(ProxyConfig::http("http://hp:80").unwrap());
        let uri: Uri = "http://example.com/path".parse().unwrap();
        let proxy = settings.proxy_for(&uri);
        assert!(proxy.is_some());
    }

    #[test]
    fn env_proxy_socks5() {
        unsafe { std::env::set_var("TEST_SOCKS5_UPPER", "socks5://proxy:1080") };
        let result = env_proxy("TEST_SOCKS5_UPPER", "test_socks5_lower");
        assert!(result.is_some());
        assert_eq!(result.unwrap().scheme, ProxyScheme::Socks5);
        unsafe { std::env::remove_var("TEST_SOCKS5_UPPER") };
    }

    #[test]
    fn env_proxy_socks4() {
        unsafe { std::env::set_var("TEST_SOCKS4_UPPER", "socks4://proxy:1080") };
        let result = env_proxy("TEST_SOCKS4_UPPER", "test_socks4_lower");
        assert!(result.is_some());
        assert_eq!(result.unwrap().scheme, ProxyScheme::Socks4);
        unsafe { std::env::remove_var("TEST_SOCKS4_UPPER") };
    }

    #[test]
    fn env_proxy_socks4a() {
        unsafe { std::env::set_var("TEST_SOCKS4A_UPPER", "socks4a://proxy:1080") };
        let result = env_proxy("TEST_SOCKS4A_UPPER", "test_socks4a_lower");
        assert!(result.is_some());
        assert_eq!(result.unwrap().scheme, ProxyScheme::Socks4);
        unsafe { std::env::remove_var("TEST_SOCKS4A_UPPER") };
    }

    #[test]
    fn env_proxy_http() {
        unsafe { std::env::set_var("TEST_HTTP_PROXY_UPPER", "http://proxy:8080") };
        let result = env_proxy("TEST_HTTP_PROXY_UPPER", "test_http_proxy_lower");
        assert!(result.is_some());
        assert_eq!(result.unwrap().scheme, ProxyScheme::Http);
        unsafe { std::env::remove_var("TEST_HTTP_PROXY_UPPER") };
    }

    #[test]
    fn env_proxy_https_scheme() {
        unsafe { std::env::set_var("TEST_HTTPS_PROXY_VAL", "https://secure-proxy:443") };
        let result = env_proxy("TEST_HTTPS_PROXY_VAL", "test_https_proxy_val_lower");
        assert!(result.is_some());
        let cfg = result.unwrap();
        assert_eq!(cfg.scheme, ProxyScheme::Https);
        assert!(cfg.uri.to_string().contains("secure-proxy"));
        unsafe { std::env::remove_var("TEST_HTTPS_PROXY_VAL") };
    }

    #[test]
    fn env_proxy_bare_hostname() {
        unsafe { std::env::set_var("TEST_BARE_HOST_PROXY", "proxy-host:3128") };
        let result = env_proxy("TEST_BARE_HOST_PROXY", "test_bare_host_proxy_lower");
        assert!(result.is_some());
        let cfg = result.unwrap();
        assert_eq!(cfg.scheme, ProxyScheme::Http);
        assert!(cfg.uri.to_string().contains("proxy-host:3128"));
        unsafe { std::env::remove_var("TEST_BARE_HOST_PROXY") };
    }

    #[test]
    fn env_proxy_empty_value() {
        unsafe { std::env::set_var("TEST_EMPTY_PROXY", "") };
        let result = env_proxy("TEST_EMPTY_PROXY", "test_empty_proxy_lower");
        assert!(result.is_none());
        unsafe { std::env::remove_var("TEST_EMPTY_PROXY") };
    }

    #[test]
    fn env_proxy_missing() {
        unsafe { std::env::remove_var("TEST_MISSING_UPPER") };
        unsafe { std::env::remove_var("test_missing_lower") };
        let result = env_proxy("TEST_MISSING_UPPER", "test_missing_lower");
        assert!(result.is_none());
    }

    #[test]
    fn env_proxy_lowercase_fallback() {
        unsafe { std::env::remove_var("TEST_LOWER_UPPER") };
        unsafe { std::env::set_var("test_lower_lower", "http://proxy:80") };
        let result = env_proxy("TEST_LOWER_UPPER", "test_lower_lower");
        assert!(result.is_some());
        unsafe { std::env::remove_var("test_lower_lower") };
    }

    #[test]
    fn proxy_config_authority_missing() {
        let cfg = ProxyConfig {
            uri: "/just-a-path".parse().unwrap(),
            scheme: ProxyScheme::Http,
            auth: None,
        };
        assert!(cfg.authority().is_err());
    }

    #[test]
    fn proxy_settings_default_is_empty() {
        let settings = ProxySettings::default();
        assert!(settings.http_proxy.is_none());
        assert!(settings.https_proxy.is_none());
        let uri: Uri = "http://example.com/".parse().unwrap();
        assert!(settings.proxy_for(&uri).is_none());
    }

    #[test]
    fn custom_proxy_trait_with_closure() {
        let f = |uri: &Uri| -> Option<ProxyConfig> {
            if uri.host() == Some("proxied.com") {
                Some(ProxyConfig::http("http://p:80").unwrap())
            } else {
                None
            }
        };
        assert!(
            f.proxy_for(&"http://proxied.com/".parse().unwrap())
                .is_some()
        );
        assert!(f.proxy_for(&"http://other.com/".parse().unwrap()).is_none());
    }

    #[test]
    fn no_proxy_takes_precedence_over_custom() {
        let settings = ProxySettings::all(ProxyConfig::http("http://p:80").unwrap())
            .no_proxy(NoProxy::new("localhost"))
            .custom(|_uri: &Uri| Some(ProxyConfig::http("http://custom:80").unwrap()));
        let uri: Uri = "http://localhost/path".parse().unwrap();
        assert!(
            settings.proxy_for(&uri).is_none(),
            "no_proxy should bypass even custom proxy"
        );
        let uri: Uri = "http://example.com/path".parse().unwrap();
        assert!(
            settings.proxy_for(&uri).is_some(),
            "non-bypassed host should use custom proxy"
        );
    }
}
