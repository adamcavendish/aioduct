#[cfg(not(target_arch = "wasm32"))]
mod automatic;
mod base;
mod component;
mod config;
mod context;
mod error;
mod headers;
mod params;
mod signer;
mod structured_fields;

#[cfg(not(target_arch = "wasm32"))]
pub(crate) use automatic::AutomaticMessageSignature;
pub use base::MessageSignatureBase;
pub use component::{MessageSignatureComponent, MessageSignatureComponentParameter};
pub use config::MessageSignatureConfig;
pub(crate) use context::MessageSignatureContext;
pub use error::MessageSignatureError;
pub use headers::MessageSignatureHeaders;
pub use params::MessageSignatureParams;
pub use signer::MessageSignatureSigner;

#[cfg(test)]
mod rfc9421_tests;
#[cfg(test)]
mod tests;
