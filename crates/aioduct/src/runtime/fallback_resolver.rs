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
