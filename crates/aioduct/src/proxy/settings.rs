use std::sync::Arc;

use http::Uri;

use crate::proxy_credential::CredentialResolver;

use super::{NoProxy, ProxyAuth, ProxyConfig};

/// Proxy settings with separate HTTP/HTTPS proxies and bypass rules.
#[derive(Clone, Default)]
pub struct ProxySettings {
    pub(crate) http_proxy: Option<ProxyConfig>,
    pub(crate) https_proxy: Option<ProxyConfig>,
    pub(crate) no_proxy: NoProxy,
    pub(crate) custom: Option<Arc<dyn CustomProxy>>,
    pub(crate) credential_resolver: Option<Arc<dyn CredentialResolver>>,
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
            credential_resolver: None,
        }
    }

    /// Create settings with a single proxy for both HTTP and HTTPS.
    pub fn all(proxy: ProxyConfig) -> Self {
        Self {
            http_proxy: Some(proxy.clone()),
            https_proxy: Some(proxy),
            no_proxy: NoProxy::default(),
            custom: None,
            credential_resolver: None,
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

    /// Set a credential resolver for looking up proxy authentication.
    pub fn proxy_credential_resolver(mut self, resolver: impl CredentialResolver) -> Self {
        self.credential_resolver = Some(Arc::new(resolver));
        self
    }

    pub(crate) fn proxy_for(&self, uri: &Uri) -> Option<ProxyConfig> {
        if self.bypasses_proxy(uri) {
            return None;
        }
        let mut proxy = if let Some(ref custom) = self.custom {
            custom.proxy_for(uri)
        } else if uses_https_proxy(uri) {
            self.https_proxy.clone()
        } else {
            self.http_proxy.clone()
        }?;
        if proxy.auth.is_none()
            && let Some(ref resolver) = self.credential_resolver
            && let Ok(authority) = proxy.authority()
            && let Some((user, pass)) = resolver.resolve(authority.as_str())
        {
            proxy.auth = Some(ProxyAuth {
                username: user,
                password: pass,
            });
        }
        Some(proxy)
    }

    fn bypasses_proxy(&self, uri: &Uri) -> bool {
        let Some(authority) = uri.authority() else {
            return false;
        };
        let port = match authority.port_u16() {
            Some(port) => Some(port),
            None if authority.as_str() != authority.host() => None,
            None if uri
                .scheme_str()
                .is_some_and(|value| value.eq_ignore_ascii_case("http")) =>
            {
                Some(80)
            }
            None if uri
                .scheme_str()
                .is_some_and(|value| value.eq_ignore_ascii_case("https")) =>
            {
                Some(443)
            }
            None => None,
        };
        self.no_proxy.matches_with_port(authority.host(), port)
    }
}

fn uses_https_proxy(uri: &Uri) -> bool {
    uri.scheme_str()
        .is_some_and(|value| value.eq_ignore_ascii_case("https"))
}

pub(super) fn env_proxy(upper: &str, lower: &str) -> Option<ProxyConfig> {
    let val = std::env::var(upper)
        .or_else(|_| std::env::var(lower))
        .ok()?;
    ProxyConfig::detect_from_url(&val)
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
