use super::MessageSignatureError;

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
