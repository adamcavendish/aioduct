mod accept_signature;
#[cfg(not(target_arch = "wasm32"))]
mod automatic;
mod base;
mod component;
mod config;
mod context;
mod error;
mod headers;
mod params;
mod parsed;
mod signer;
mod verification;

pub use accept_signature::{AcceptSignature, AcceptSignatureEntry, AcceptSignatureFulfillment};
#[cfg(not(target_arch = "wasm32"))]
pub(crate) use automatic::{AutomaticMessageSignature, PreparedAutomaticMessageSignature};
pub use base::MessageSignatureBase;
pub use component::{
    MessageSignatureComponent, MessageSignatureComponentParameter,
    MessageSignatureStructuredFieldType,
};
pub use config::MessageSignatureConfig;
pub(crate) use context::MessageSignatureContext;
pub use error::MessageSignatureError;
pub use headers::MessageSignatureHeaders;
pub use params::{AcceptSignatureParams, MessageSignatureParams};
pub use parsed::MessageSignature;
pub use signer::{
    MessageSignatureAsyncSigner, MessageSignatureAsyncSigningFuture,
    MessageSignatureLocalAsyncSigner, MessageSignatureLocalAsyncSigningFuture,
    MessageSignatureSigner,
};
pub use verification::{
    MessageSignatureRequestContext, MessageSignatureResponseContext,
    MessageSignatureVerificationInput, MessageSignatureVerificationPolicy,
    MessageSignatureVerifier,
};

#[cfg(test)]
mod rfc9421_tests;
#[cfg(test)]
mod tests;
