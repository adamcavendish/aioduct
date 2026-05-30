mod chain;
mod config;
mod no_proxy;
mod settings;

pub use chain::ProxyChain;
pub use config::ProxyConfig;
pub use no_proxy::NoProxy;
pub use settings::{CustomProxy, ProxySettings};

pub(crate) use config::ProxyAuth;
#[cfg(any(not(target_arch = "wasm32"), test))]
pub(crate) use config::ProxyScheme;

#[cfg(test)]
mod tests;
