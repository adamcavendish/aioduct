use super::ProxyConfig;

/// An ordered chain of proxy configurations for multi-hop routing.
///
/// The first proxy is the closest to the client; each subsequent proxy is
/// reached through the previous one. Currently supports up to 2 hops.
///
/// # Example
///
/// ```ignore
/// let chain = ProxyChain::new(vec![
///     ProxyConfig::socks5("socks5://internal:1080")?,
///     ProxyConfig::http("http://corp-proxy:8080")?,
/// ]);
/// ```
#[derive(Clone, Debug)]
pub struct ProxyChain {
    pub(crate) proxies: Vec<ProxyConfig>,
}

impl ProxyChain {
    /// Create a chain from an ordered list of proxies.
    pub fn new(proxies: Vec<ProxyConfig>) -> Self {
        Self { proxies }
    }

    /// Create a single-proxy chain (equivalent to using `ProxyConfig` directly).
    pub fn single(proxy: ProxyConfig) -> Self {
        Self {
            proxies: vec![proxy],
        }
    }

    /// Number of hops in the chain.
    pub fn len(&self) -> usize {
        self.proxies.len()
    }

    pub(crate) fn route_identity(&self) -> super::config::ProxyRouteIdentity {
        super::config::ProxyRouteIdentity::from_configs(&self.proxies)
    }

    /// Returns `true` if the chain is empty.
    pub fn is_empty(&self) -> bool {
        self.proxies.is_empty()
    }

    /// The first proxy in the chain.
    pub fn first(&self) -> Option<&ProxyConfig> {
        self.proxies.first()
    }

    /// Iterator over the proxy configurations in order.
    pub fn iter(&self) -> impl Iterator<Item = &ProxyConfig> {
        self.proxies.iter()
    }
}
