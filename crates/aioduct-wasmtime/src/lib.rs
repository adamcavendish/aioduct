//! Host-side Wasmtime WASI HTTP adapter backed by native `aioduct`.
//!
//! This crate is for hosts embedding WASI Preview 2 components with
//! `wasi:http`. Guests keep using `aioduct::WasiClient`; the host installs
//! [`WasiHttpHost`] as the Wasmtime HTTP hook and owns transport trust policy.
//!
//! The default feature set is empty. Enable exactly the native host transport
//! runtime you want to use, such as `tokio`, `smol`, or `compio`, plus a rustls
//! provider when the native transport needs TLS.
//!
//! Runnable host examples live under `examples/wasmtime-host` in the workspace.
//! They show a WASI guest component using `aioduct::WasiClient` while the host
//! validates origin policy, injects a host-owned header, and forwards through
//! Tokio, smol, or compio native transports.

#![deny(missing_docs)]

mod body;
mod host;
mod policy;
mod transport;

pub use host::{WasiHttpHost, WasiHttpHostBuilder};
pub use policy::{BuildError, ExactOriginPolicy, PolicyError, RejectionReason};
#[cfg(feature = "smol")]
pub use transport::SmolTransportBuilder;
#[cfg(feature = "tokio")]
pub use transport::TokioTransportBuilder;
pub use transport::{BoxFuture, HostForwardOptions, HostResponse, WasiHostTransport};
#[cfg(feature = "compio")]
pub use transport::{CompioHostTransport, CompioTransportBuilder};

#[cfg(test)]
pub(crate) use body::request_body_limit_from_error;
pub(crate) use body::{
    DeadlineBody, RequestLimitBody, ResponseLimitBody, map_aioduct_error, map_wasi_body_error,
};
#[cfg(test)]
pub(crate) use policy::RejectionObserver;

#[doc(hidden)]
pub mod sealed {
    /// Marker trait sealing [`WasiHostTransport`](crate::WasiHostTransport).
    pub trait Sealed {}

    impl<R, C> Sealed for aioduct::HttpEngineSend<R, C>
    where
        R: aioduct::RuntimePoll,
        C: aioduct::ConnectorSend,
    {
    }

    #[cfg(feature = "compio")]
    impl Sealed for crate::CompioHostTransport {}
}

#[cfg(test)]
mod tests;
