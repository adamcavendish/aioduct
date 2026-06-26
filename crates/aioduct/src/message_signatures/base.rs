/// A generated RFC 9421 signature base.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct MessageSignatureBase {
    value: String,
}

impl MessageSignatureBase {
    pub(crate) fn new(value: String) -> Self {
        Self { value }
    }

    /// Return the signature base as a string.
    pub fn as_str(&self) -> &str {
        &self.value
    }

    /// Return the signature base bytes that should be passed to a signer.
    pub fn as_bytes(&self) -> &[u8] {
        self.value.as_bytes()
    }

    /// Consume this value into its string representation.
    pub fn into_string(self) -> String {
        self.value
    }
}
