/// System DNS resolver backed by the OS resolver (`getaddrinfo` / `ToSocketAddrs`).
///
/// This is the default resolver when no custom resolver is configured.
/// It can also be used explicitly in resolver chains:
///
/// ```ignore
/// use aioduct::{SystemResolver, StaticResolver};
/// let mut resolver = StaticResolver::with_fallback(SystemResolver);
/// resolver.add("my-svc.local".into(), vec!["10.0.0.1:8080".parse().unwrap()]);
/// ```
///
/// On tokio, uses `tokio::net::lookup_host`.
/// On smol, uses `smol::net::resolve`.
/// On compio, uses `compio_runtime::spawn_blocking` with `std::net::ToSocketAddrs`.
///
/// When multiple runtimes are enabled, tokio takes precedence, then smol,
/// then compio.
#[cfg(feature = "tokio")]
pub use crate::runtime::tokio_rt::DefaultResolver as SystemResolver;

#[cfg(all(feature = "smol", not(feature = "tokio")))]
pub use crate::runtime::smol_rt::DefaultResolver as SystemResolver;

#[cfg(all(feature = "compio", not(any(feature = "tokio", feature = "smol"))))]
pub use crate::runtime::compio_rt::DefaultResolver as SystemResolver;
