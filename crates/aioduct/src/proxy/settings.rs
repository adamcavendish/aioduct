use std::net::IpAddr;
use std::sync::Arc;

use http::Uri;
use http::header::PROXY_AUTHORIZATION;

use crate::error::{BuilderError, Error};
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
    http_proxy_error: Option<BuilderError>,
    https_proxy_error: Option<BuilderError>,
}

impl std::fmt::Debug for ProxySettings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProxySettings")
            .field("http_proxy", &self.http_proxy)
            .field("https_proxy", &self.https_proxy)
            .field("no_proxy", &self.no_proxy)
            .field("custom", &self.custom.as_ref().map(|_| ".."))
            .field(
                "http_proxy_error",
                &self.http_proxy_error.as_ref().map(|_| ".."),
            )
            .field(
                "https_proxy_error",
                &self.https_proxy_error.as_ref().map(|_| ".."),
            )
            .finish()
    }
}

impl ProxySettings {
    /// Read proxy settings from environment variables.
    ///
    /// Reads `HTTP_PROXY` / `http_proxy`, `HTTPS_PROXY` / `https_proxy`,
    /// and `NO_PROXY` / `no_proxy`. The uppercase variant takes precedence.
    pub fn from_env() -> Self {
        Self::from_env_variables(
            "HTTP_PROXY",
            "http_proxy",
            "HTTPS_PROXY",
            "https_proxy",
            NoProxy::from_env(),
        )
    }

    pub(super) fn from_env_variables(
        http_upper: &str,
        http_lower: &str,
        https_upper: &str,
        https_lower: &str,
        no_proxy: NoProxy,
    ) -> Self {
        let (http_proxy, http_proxy_error) = capture_env_proxy(env_proxy(http_upper, http_lower));
        let (https_proxy, https_proxy_error) =
            capture_env_proxy(env_proxy(https_upper, https_lower));
        Self {
            http_proxy,
            https_proxy,
            no_proxy,
            custom: None,
            credential_resolver: None,
            http_proxy_error,
            https_proxy_error,
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
            http_proxy_error: None,
            https_proxy_error: None,
        }
    }

    /// Set the HTTP proxy.
    pub fn http(mut self, proxy: ProxyConfig) -> Self {
        self.http_proxy = Some(proxy);
        self.http_proxy_error = None;
        self
    }

    /// Set the HTTPS proxy.
    pub fn https(mut self, proxy: ProxyConfig) -> Self {
        self.https_proxy = Some(proxy);
        self.https_proxy_error = None;
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
        } else {
            if uses_https_proxy(uri) {
                self.https_proxy.clone()
            } else {
                self.http_proxy.clone()
            }
        }?;
        self.resolve_credentials(&mut proxy);
        Some(proxy)
    }

    pub(crate) fn validate_for_uri(&self, uri: &Uri) -> Result<(), Error> {
        if self.custom.is_some() || self.bypasses_proxy(uri) {
            return Ok(());
        }
        let error = if uses_https_proxy(uri) {
            self.https_proxy_error.as_ref()
        } else {
            self.http_proxy_error.as_ref()
        };
        if let Some(error) = error.cloned() {
            return Err(error.into_error());
        }
        Ok(())
    }

    fn bypasses_proxy(&self, uri: &Uri) -> bool {
        let effective_port = uri.port_u16().or_else(|| {
            if uses_https_proxy(uri) {
                Some(443)
            } else if uri
                .scheme_str()
                .is_some_and(|scheme| scheme.eq_ignore_ascii_case("http"))
            {
                Some(80)
            } else {
                None
            }
        });
        uri.host()
            .is_some_and(|host| self.no_proxy.matches_with_port(host, effective_port))
    }

    pub(super) fn resolve_credentials(&self, proxy: &mut ProxyConfig) {
        if proxy.auth.is_none()
            && !proxy
                .connect_headers
                .iter()
                .any(|(name, _)| name == PROXY_AUTHORIZATION)
            && let Some(ref resolver) = self.credential_resolver
            && let Some(key) = credential_resolver_key(proxy)
            && let Some((user, pass)) = resolver.resolve(&key)
        {
            proxy.auth = Some(ProxyAuth {
                username: user,
                password: pass,
            });
        }
    }
}

fn credential_resolver_key(proxy: &ProxyConfig) -> Option<String> {
    let authority = proxy.authority().ok()?;
    let raw_host = authority.host();
    let host = raw_host
        .strip_prefix('[')
        .and_then(|host| host.strip_suffix(']'))
        .unwrap_or(raw_host);
    let host = host
        .parse::<IpAddr>()
        .map(|address| address.to_string())
        .unwrap_or_else(|_| host.to_ascii_lowercase());
    let port = proxy.effective_port().ok()?;

    if host.contains(':') {
        Some(format!("[{host}]:{port}"))
    } else {
        Some(format!("{host}:{port}"))
    }
}

fn capture_env_proxy(
    result: Result<Option<ProxyConfig>, BuilderError>,
) -> (Option<ProxyConfig>, Option<BuilderError>) {
    match result {
        Ok(proxy) => (proxy, None),
        Err(error) => (None, Some(error)),
    }
}

fn uses_https_proxy(uri: &Uri) -> bool {
    uri.scheme_str()
        .is_some_and(|scheme| scheme.eq_ignore_ascii_case("https"))
}

pub(super) fn env_proxy(upper: &str, lower: &str) -> Result<Option<ProxyConfig>, BuilderError> {
    let (name, value) = match std::env::var(upper) {
        Ok(value) => (upper, value),
        Err(std::env::VarError::NotPresent) => match std::env::var(lower) {
            Ok(value) => (lower, value),
            Err(std::env::VarError::NotPresent) => return Ok(None),
            Err(std::env::VarError::NotUnicode(_)) => {
                return Err(BuilderError::invalid_url(format!(
                    "proxy environment variable {lower} is not valid Unicode"
                )));
            }
        },
        Err(std::env::VarError::NotUnicode(_)) => {
            return Err(BuilderError::invalid_url(format!(
                "proxy environment variable {upper} is not valid Unicode"
            )));
        }
    };
    if value.is_empty() {
        return Ok(None);
    }
    ProxyConfig::try_detect_from_url(&value)
        .map(Some)
        .map_err(|error| {
            let detail = match error {
                Error::InvalidUrl(message) => message,
                error => error.to_string(),
            };
            BuilderError::invalid_url(format!(
                "invalid proxy environment variable {name}: {detail}"
            ))
        })
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
