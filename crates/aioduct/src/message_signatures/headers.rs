use http::header::{HeaderMap, HeaderName, HeaderValue};

/// Generated `Signature-Input` and `Signature` header values.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct MessageSignatureHeaders {
    /// The `Signature-Input` header value.
    pub signature_input: HeaderValue,
    /// The `Signature` header value.
    pub signature: HeaderValue,
}

impl MessageSignatureHeaders {
    /// Insert the generated headers into a header map.
    pub fn insert_into(self, headers: &mut HeaderMap) {
        headers.insert(
            HeaderName::from_static("signature-input"),
            self.signature_input,
        );
        headers.insert(HeaderName::from_static("signature"), self.signature);
    }
}
