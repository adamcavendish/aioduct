mod chain;
mod config;
#[cfg(not(target_arch = "wasm32"))]
mod dispatch_route;
#[cfg(not(target_arch = "wasm32"))]
mod establishment_plan;
mod no_proxy;
mod settings;

pub use chain::ProxyChain;
pub use config::ProxyConfig;
pub use no_proxy::NoProxy;
pub use settings::{CustomProxy, ProxySettings};

pub(crate) use config::ProxyAuth;
#[cfg(not(target_arch = "wasm32"))]
pub(crate) use config::ProxyRouteIdentity;
#[cfg(any(not(target_arch = "wasm32"), test))]
pub(crate) use config::ProxyScheme;
#[cfg(not(target_arch = "wasm32"))]
pub(crate) use dispatch_route::ProxyDispatchRoute;
#[cfg(not(target_arch = "wasm32"))]
pub(crate) use establishment_plan::{ProxyEndpoint, ProxyEstablishmentPlan, ProxyHopPlan};

#[cfg(test)]
mod tests;
