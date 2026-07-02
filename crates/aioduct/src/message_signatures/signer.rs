use std::future::Future;
use std::pin::Pin;

use super::{MessageSignatureBase, MessageSignatureError};

/// Synchronous signer used by native automatic request signing.
///
/// Async or host-backed signing can use
/// [`MessageSignatureConfig::signature_base`](super::MessageSignatureConfig::signature_base)
/// and
/// [`MessageSignatureConfig::headers_from_signature`](super::MessageSignatureConfig::headers_from_signature)
/// directly: build the base, await the external signer, then format and attach
/// the returned bytes.
pub trait MessageSignatureSigner: Send + Sync + 'static {
    /// Sign the provided signature base bytes and return the raw signature bytes.
    fn sign(&self, signature_base: &[u8]) -> Result<Vec<u8>, MessageSignatureError>;
}

impl<F> MessageSignatureSigner for F
where
    F: Fn(&[u8]) -> Result<Vec<u8>, MessageSignatureError> + Send + Sync + 'static,
{
    fn sign(&self, signature_base: &[u8]) -> Result<Vec<u8>, MessageSignatureError> {
        self(signature_base)
    }
}

/// Boxed future returned by a send-runtime asynchronous message signer.
pub type MessageSignatureAsyncSigningFuture =
    Pin<Box<dyn Future<Output = Result<Vec<u8>, MessageSignatureError>> + Send + 'static>>;

/// Asynchronous signer used by native send-runtime automatic request signing.
///
/// The signer receives an owned [`MessageSignatureBase`]. This keeps request and
/// header state out of the async signing future while still allowing remote KMS,
/// HSM, or other asynchronous signers to inspect the exact bytes being signed.
pub trait MessageSignatureAsyncSigner: Send + Sync + 'static {
    /// Sign the provided signature base and return the raw signature bytes.
    fn sign(&self, signature_base: MessageSignatureBase) -> MessageSignatureAsyncSigningFuture;
}

impl<F, Fut> MessageSignatureAsyncSigner for F
where
    F: Fn(MessageSignatureBase) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<Vec<u8>, MessageSignatureError>> + Send + 'static,
{
    fn sign(&self, signature_base: MessageSignatureBase) -> MessageSignatureAsyncSigningFuture {
        Box::pin(self(signature_base))
    }
}

/// Boxed future returned by a local-runtime asynchronous message signer.
pub type MessageSignatureLocalAsyncSigningFuture =
    Pin<Box<dyn Future<Output = Result<Vec<u8>, MessageSignatureError>> + 'static>>;

/// Asynchronous signer used by native local-runtime automatic request signing.
///
/// The signer state must still be shareable with the client, but the returned
/// future is not required to be [`Send`], matching local-runtime execution.
pub trait MessageSignatureLocalAsyncSigner: Send + Sync + 'static {
    /// Sign the provided signature base and return the raw signature bytes.
    fn sign_local(
        &self,
        signature_base: MessageSignatureBase,
    ) -> MessageSignatureLocalAsyncSigningFuture;
}

impl<F, Fut> MessageSignatureLocalAsyncSigner for F
where
    F: Fn(MessageSignatureBase) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<Vec<u8>, MessageSignatureError>> + 'static,
{
    fn sign_local(
        &self,
        signature_base: MessageSignatureBase,
    ) -> MessageSignatureLocalAsyncSigningFuture {
        Box::pin(self(signature_base))
    }
}
