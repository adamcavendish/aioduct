use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;

use super::Resolve;

/// Chains two resolvers: tries the primary first, falls back to the
/// secondary when the primary returns an error or an empty address list.
///
/// ```ignore
/// use aioduct::{FallbackResolver, SystemResolver, HickoryResolver};
///
/// // Use hickory-dns if available, fall back to system resolver
/// let resolver = FallbackResolver::new(
///     HickoryResolver::new(),
///     SystemResolver,
/// );
/// ```
pub struct FallbackResolver {
    primary: Arc<dyn Resolve>,
    fallback: Arc<dyn Resolve>,
}

impl FallbackResolver {
    /// Create a new fallback resolver that tries `primary` first, then `fallback`.
    pub fn new(primary: impl Resolve, fallback: impl Resolve) -> Self {
        Self {
            primary: Arc::new(primary),
            fallback: Arc::new(fallback),
        }
    }
}

impl Resolve for FallbackResolver {
    fn resolve(
        &self,
        host: &str,
        port: u16,
    ) -> Pin<Box<dyn Future<Output = io::Result<SocketAddr>> + Send>> {
        let host = host.to_owned();
        let primary = self.primary.clone();
        let fallback = self.fallback.clone();
        Box::pin(async move {
            match primary.resolve_all(&host, port).await {
                Ok(addrs) if !addrs.is_empty() => Ok(addrs[0]),
                _ => match fallback.resolve_all(&host, port).await {
                    Ok(addrs) => addrs.into_iter().next().ok_or_else(|| {
                        io::Error::new(
                            io::ErrorKind::AddrNotAvailable,
                            format!("no addresses resolved for {host}:{port}"),
                        )
                    }),
                    Err(e) => Err(e),
                },
            }
        })
    }

    fn resolve_all(
        &self,
        host: &str,
        port: u16,
    ) -> Pin<Box<dyn Future<Output = io::Result<Vec<SocketAddr>>> + Send>> {
        let host = host.to_owned();
        let primary = self.primary.clone();
        let fallback = self.fallback.clone();
        Box::pin(async move {
            match primary.resolve_all(&host, port).await {
                Ok(addrs) if !addrs.is_empty() => Ok(addrs),
                _ => fallback.resolve_all(&host, port).await,
            }
        })
    }
}

#[cfg(all(test, feature = "tokio"))]
mod tests {
    use std::future::Future;
    use std::io;
    use std::net::SocketAddr;
    use std::pin::Pin;

    use super::FallbackResolver;
    use crate::runtime::Resolve;

    fn mock_resolver(addrs: Vec<SocketAddr>, should_err: bool) -> impl Resolve {
        move |_host: &str,
              _port: u16|
              -> Pin<Box<dyn Future<Output = io::Result<SocketAddr>> + Send>> {
            let addrs = addrs.clone();
            Box::pin(async move {
                if should_err {
                    Err(io::Error::new(
                        io::ErrorKind::AddrNotAvailable,
                        "mock error",
                    ))
                } else {
                    addrs
                        .first()
                        .copied()
                        .ok_or_else(|| io::Error::new(io::ErrorKind::AddrNotAvailable, "empty"))
                }
            })
        }
    }

    #[tokio::test]
    async fn fallback_resolver_primary_ok_skips_fallback() {
        let addr: SocketAddr = "10.0.0.1:8080".parse().unwrap();
        let primary = mock_resolver(vec![addr], false);
        let fallback = mock_resolver(vec![], true);

        let resolver = FallbackResolver::new(primary, fallback);
        let result = resolver.resolve("example.com", 8080).await.unwrap();
        assert_eq!(result, addr);
    }

    #[tokio::test]
    async fn fallback_resolver_primary_err_calls_fallback() {
        let addr: SocketAddr = "10.0.0.2:9090".parse().unwrap();
        let primary = mock_resolver(vec![], true);
        let fallback = mock_resolver(vec![addr], false);

        let resolver = FallbackResolver::new(primary, fallback);
        let result = resolver.resolve("example.com", 9090).await.unwrap();
        assert_eq!(result, addr);
    }

    #[tokio::test]
    async fn fallback_resolver_primary_empty_calls_fallback() {
        let addr: SocketAddr = "10.0.0.2:9090".parse().unwrap();
        let primary = mock_resolver(vec![], false);
        let fallback = mock_resolver(vec![addr], false);

        let resolver = FallbackResolver::new(primary, fallback);
        let result = resolver.resolve("empty.com", 9090).await.unwrap();
        assert_eq!(result, addr);
    }

    #[tokio::test]
    async fn fallback_resolver_both_fail_propagates_error() {
        let primary = mock_resolver(vec![], true);
        let fallback = mock_resolver(vec![], true);

        let resolver = FallbackResolver::new(primary, fallback);
        let err = resolver.resolve("fail.com", 80).await.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AddrNotAvailable);
        assert!(err.to_string().contains("mock error"));
    }

    #[tokio::test]
    async fn fallback_resolver_chaining() {
        let addr: SocketAddr = "10.0.0.3:7070".parse().unwrap();
        let inner = FallbackResolver::new(
            mock_resolver(vec![], true),
            mock_resolver(vec![addr], false),
        );
        let outer = FallbackResolver::new(mock_resolver(vec![], true), inner);
        let result = outer.resolve("chained.com", 7070).await.unwrap();
        assert_eq!(result, addr);
    }

    #[tokio::test]
    async fn fallback_resolver_resolve_returns_first_from_primary() {
        // A primary that returns multiple addresses via resolve_all
        struct MultiAddrResolver(Vec<SocketAddr>);
        impl Resolve for MultiAddrResolver {
            fn resolve(
                &self,
                _host: &str,
                _port: u16,
            ) -> Pin<Box<dyn Future<Output = io::Result<SocketAddr>> + Send>> {
                Box::pin(async { Err(io::Error::other("resolve not used")) })
            }
            fn resolve_all(
                &self,
                _host: &str,
                _port: u16,
            ) -> Pin<Box<dyn Future<Output = io::Result<Vec<SocketAddr>>> + Send>> {
                let addrs = self.0.clone();
                Box::pin(async move { Ok(addrs) })
            }
        }

        let addr1: SocketAddr = "10.0.1.1:80".parse().unwrap();
        let addr2: SocketAddr = "10.0.1.2:80".parse().unwrap();
        let addr3: SocketAddr = "10.0.1.3:80".parse().unwrap();
        let primary = MultiAddrResolver(vec![addr1, addr2, addr3]);
        let fallback = mock_resolver(vec![], true);

        let resolver = FallbackResolver::new(primary, fallback);
        let result = resolver.resolve("multi.com", 80).await.unwrap();
        assert_eq!(result, addr1);
    }
}
