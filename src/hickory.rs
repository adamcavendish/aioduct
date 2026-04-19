use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;

use hickory_resolver::TokioResolver;

use crate::runtime::Resolve;

/// DNS resolver backed by Hickory DNS (formerly Trust-DNS).
///
/// Requires the `hickory-dns` feature (implies `tokio`).
#[derive(Clone)]
pub struct HickoryResolver {
    resolver: Arc<TokioResolver>,
}

impl HickoryResolver {
    /// Create a resolver using the system's DNS configuration.
    pub fn new() -> io::Result<Self> {
        let resolver = TokioResolver::builder_tokio()
            .map_err(io::Error::other)?
            .build()
            .map_err(io::Error::other)?;
        Ok(Self {
            resolver: Arc::new(resolver),
        })
    }

    /// Create a resolver from explicit config and options.
    pub fn from_config(
        config: hickory_resolver::config::ResolverConfig,
        opts: hickory_resolver::config::ResolverOpts,
    ) -> Self {
        use hickory_resolver::net::runtime::TokioRuntimeProvider;
        let resolver = TokioResolver::builder_with_config(config, TokioRuntimeProvider::default())
            .with_options(opts)
            .build()
            .expect("failed to build HickoryResolver");
        Self {
            resolver: Arc::new(resolver),
        }
    }
}

impl Default for HickoryResolver {
    fn default() -> Self {
        Self::new().expect("failed to create HickoryResolver")
    }
}

impl Resolve for HickoryResolver {
    fn resolve(
        &self,
        host: &str,
        port: u16,
    ) -> Pin<Box<dyn std::future::Future<Output = io::Result<SocketAddr>> + Send>> {
        let resolver = self.resolver.clone();
        let host = host.to_owned();
        Box::pin(async move {
            let lookup = resolver
                .lookup_ip(host.as_str())
                .await
                .map_err(|e| io::Error::other(format!("DNS resolution failed: {e}")))?;
            lookup
                .iter()
                .next()
                .map(|ip| SocketAddr::new(ip, port))
                .ok_or_else(|| {
                    io::Error::new(io::ErrorKind::AddrNotAvailable, "no addresses found")
                })
        })
    }
}
