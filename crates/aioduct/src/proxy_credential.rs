use std::sync::Arc;

/// Trait for resolving proxy credentials from external sources.
///
/// Implementations can look up credentials from environment variables,
/// platform keychains, credential files, or other stores.
///
/// The `key` parameter is typically the proxy's `host:port` string,
/// used to match credentials to the correct proxy server.
pub trait CredentialResolver: Send + Sync + 'static {
    /// Try to resolve credentials for a proxy identified by `key`.
    ///
    /// Returns `(username, password)` if credentials are found, or `None`
    /// if this resolver does not have credentials for the given key.
    fn resolve(&self, key: &str) -> Option<(String, String)>;
}

/// A credential resolver that chains multiple resolvers together.
///
/// Resolvers are tried in order until one returns credentials.
/// This allows stacking platform-specific resolvers with fallbacks.
#[derive(Default)]
pub struct CompositeResolver {
    resolvers: Vec<Arc<dyn CredentialResolver>>,
}

impl CompositeResolver {
    /// Create a new empty composite resolver.
    pub fn new() -> Self {
        Self {
            resolvers: Vec::new(),
        }
    }

    /// Add a resolver to the chain. Resolvers are tried in the order they are added.
    pub fn push(mut self, resolver: impl CredentialResolver) -> Self {
        self.resolvers.push(Arc::new(resolver));
        self
    }
}

impl CredentialResolver for CompositeResolver {
    fn resolve(&self, key: &str) -> Option<(String, String)> {
        for resolver in &self.resolvers {
            if let Some(creds) = resolver.resolve(key) {
                return Some(creds);
            }
        }
        None
    }
}

/// A credential resolver that reads from environment variables.
///
/// Reads `AIODUCT_PROXY_USER` and `AIODUCT_PROXY_PASS` environment variables
/// globally — the `key` parameter is currently **ignored**. All proxies receive
/// the same credentials. Per-proxy credential lookup (e.g. via platform
/// keychains) is planned for future resolvers.
///
/// Returns `None` if either variable is not set or is empty.
#[derive(Clone, Copy, Debug, Default)]
pub struct EnvCredentialResolver;

impl CredentialResolver for EnvCredentialResolver {
    fn resolve(&self, _key: &str) -> Option<(String, String)> {
        let user = std::env::var("AIODUCT_PROXY_USER").ok()?;
        let pass = std::env::var("AIODUCT_PROXY_PASS").ok()?;
        if user.is_empty() {
            return None;
        }
        Some((user, pass))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Serializes env var mutations to prevent flakiness under `--test-threads > 1`.
    static ENV_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn env_resolver_reads_vars() {
        let _guard = ENV_MUTEX.lock().unwrap();
        unsafe {
            std::env::set_var("AIODUCT_PROXY_USER", "testuser");
            std::env::set_var("AIODUCT_PROXY_PASS", "testpass");
        }
        let resolver = EnvCredentialResolver;
        let result = resolver.resolve("proxy:8080");
        assert_eq!(
            result,
            Some(("testuser".to_string(), "testpass".to_string()))
        );
        unsafe {
            std::env::remove_var("AIODUCT_PROXY_USER");
            std::env::remove_var("AIODUCT_PROXY_PASS");
        }
    }

    #[test]
    fn env_resolver_returns_none_when_vars_missing() {
        let _guard = ENV_MUTEX.lock().unwrap();
        unsafe {
            std::env::remove_var("AIODUCT_PROXY_USER");
            std::env::remove_var("AIODUCT_PROXY_PASS");
        }
        let resolver = EnvCredentialResolver;
        assert_eq!(resolver.resolve("proxy:8080"), None);
    }

    #[test]
    fn composite_resolver_tries_in_order() {
        let _guard = ENV_MUTEX.lock().unwrap();
        let r1 = EnvCredentialResolver; // won't match (env vars not set)
        struct StaticResolver(&'static str, &'static str);
        impl CredentialResolver for StaticResolver {
            fn resolve(&self, _key: &str) -> Option<(String, String)> {
                Some((self.0.to_string(), self.1.to_string()))
            }
        }
        let r2 = StaticResolver("chain_user", "chain_pass");

        let composite = CompositeResolver::new().push(r1).push(r2);
        unsafe {
            std::env::remove_var("AIODUCT_PROXY_USER");
            std::env::remove_var("AIODUCT_PROXY_PASS");
        }
        assert_eq!(
            composite.resolve("proxy:8080"),
            Some(("chain_user".to_string(), "chain_pass".to_string()))
        );
    }

    #[test]
    fn composite_resolver_returns_none_when_all_fail() {
        struct NoCredResolver;
        impl CredentialResolver for NoCredResolver {
            fn resolve(&self, _key: &str) -> Option<(String, String)> {
                None
            }
        }
        let composite = CompositeResolver::new().push(NoCredResolver);
        assert_eq!(composite.resolve("proxy:8080"), None);
    }

    #[test]
    fn composite_resolver_empty_returns_none() {
        let composite = CompositeResolver::new();
        assert_eq!(composite.resolve("proxy:8080"), None);
    }

    #[test]
    fn composite_resolver_stops_at_first_match() {
        use std::sync::atomic::{AtomicU32, Ordering};

        let count1 = Arc::new(AtomicU32::new(0));
        let count2 = Arc::new(AtomicU32::new(0));
        let count3 = Arc::new(AtomicU32::new(0));

        struct CountingResolver {
            name: &'static str,
            count: Arc<AtomicU32>,
        }
        impl CredentialResolver for CountingResolver {
            fn resolve(&self, _key: &str) -> Option<(String, String)> {
                self.count.fetch_add(1, Ordering::SeqCst);
                if self.name == "second" {
                    Some((self.name.to_string(), "pw".to_string()))
                } else {
                    None
                }
            }
        }

        let r1 = CountingResolver {
            name: "first",
            count: Arc::clone(&count1),
        };
        let r2 = CountingResolver {
            name: "second",
            count: Arc::clone(&count2),
        };
        let r3 = CountingResolver {
            name: "third",
            count: Arc::clone(&count3),
        };
        let composite = CompositeResolver::new().push(r1).push(r2).push(r3);
        let result = composite.resolve("proxy:8080");
        assert!(result.is_some());
        assert_eq!(count1.load(Ordering::SeqCst), 1);
        assert_eq!(count2.load(Ordering::SeqCst), 1);
        assert_eq!(count3.load(Ordering::SeqCst), 0);
    }
}
