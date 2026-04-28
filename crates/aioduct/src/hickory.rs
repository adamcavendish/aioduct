use std::io;
use std::net::IpAddr;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;

use hickory_resolver::{
    TokioResolver,
    config::{LookupIpStrategy, ResolverConfig, ResolverOpts},
    net::runtime::TokioRuntimeProvider,
};

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
    ///
    /// If the system configuration cannot be loaded, falls back to
    /// `hickory_resolver`'s default resolver configuration.
    pub fn new() -> io::Result<Self> {
        let mut builder = match TokioResolver::builder_tokio() {
            Ok(builder) => builder,
            Err(err) => {
                #[cfg(feature = "tracing")]
                tracing::debug!(
                    error = %err,
                    "hickory.resolver.system_config_fallback"
                );
                #[cfg(not(feature = "tracing"))]
                let _ = err;
                TokioResolver::builder_with_config(
                    ResolverConfig::default(),
                    TokioRuntimeProvider::default(),
                )
            }
        };
        prefer_ipv4_and_ipv6(builder.options_mut());
        let resolver = builder.build().map_err(io::Error::other)?;
        Ok(Self {
            resolver: Arc::new(resolver),
        })
    }

    /// Create a resolver from explicit config and options.
    pub fn from_config(config: ResolverConfig, opts: ResolverOpts) -> Self {
        let resolver = TokioResolver::builder_with_config(config, TokioRuntimeProvider::default())
            .with_options(opts)
            .build()
            .expect("failed to build HickoryResolver");
        Self {
            resolver: Arc::new(resolver),
        }
    }
}

fn prefer_ipv4_and_ipv6(opts: &mut ResolverOpts) {
    opts.ip_strategy = LookupIpStrategy::Ipv4AndIpv6;
}

fn socket_addrs_from_ips(
    ips: impl IntoIterator<Item = IpAddr>,
    port: u16,
) -> io::Result<Vec<SocketAddr>> {
    let addrs: Vec<_> = ips
        .into_iter()
        .map(|ip| SocketAddr::new(ip, port))
        .collect();
    if addrs.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::AddrNotAvailable,
            "no addresses found",
        ));
    }
    Ok(addrs)
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
        let fut = self.resolve_all(host, port);
        Box::pin(async move {
            let addrs = fut.await?;
            addrs.into_iter().next().ok_or_else(|| {
                io::Error::new(io::ErrorKind::AddrNotAvailable, "no addresses resolved")
            })
        })
    }

    fn resolve_all(
        &self,
        host: &str,
        port: u16,
    ) -> Pin<Box<dyn std::future::Future<Output = io::Result<Vec<SocketAddr>>> + Send>> {
        let resolver = self.resolver.clone();
        let host = host.to_owned();
        Box::pin(async move {
            let lookup = resolver
                .lookup_ip(host.as_str())
                .await
                .map_err(|e| io::Error::other(format!("DNS resolution failed: {e}")))?;
            socket_addrs_from_ips(lookup.iter(), port)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, Ipv6Addr};

    #[test]
    fn prefer_ipv4_and_ipv6_overrides_default_strategy() {
        let mut opts = ResolverOpts::default();
        opts.ip_strategy = LookupIpStrategy::Ipv6Only;

        prefer_ipv4_and_ipv6(&mut opts);

        assert_eq!(opts.ip_strategy, LookupIpStrategy::Ipv4AndIpv6);
    }

    #[test]
    fn socket_addrs_from_ips_preserves_all_addresses() {
        let addrs = socket_addrs_from_ips(
            [
                IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1)),
                IpAddr::V6(Ipv6Addr::LOCALHOST),
            ],
            443,
        )
        .unwrap();

        assert_eq!(addrs.len(), 2);
        assert_eq!(
            addrs[0],
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1)), 443)
        );
        assert_eq!(
            addrs[1],
            SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), 443)
        );
    }

    #[test]
    fn socket_addrs_from_ips_rejects_empty_lookup() {
        let err = socket_addrs_from_ips([], 443).unwrap_err();

        assert_eq!(err.kind(), io::ErrorKind::AddrNotAvailable);
    }
}
