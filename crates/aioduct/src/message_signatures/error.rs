use http::header::HeaderName;

/// Errors from RFC 9421 signature-base generation or header formatting.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum MessageSignatureError {
    /// The signature label is not a valid Structured Fields dictionary key.
    #[error("invalid signature label `{0}`")]
    InvalidLabel(String),
    /// The signature has no covered components.
    #[error("message signature requires at least one covered component")]
    EmptyComponents,
    /// The same component identifier appeared more than once.
    #[error("duplicate covered component {0}")]
    DuplicateComponent(String),
    /// The target URI does not have a scheme.
    #[error("target URI does not contain a scheme")]
    MissingScheme,
    /// The target URI does not have an authority.
    #[error("target URI does not contain an authority")]
    MissingAuthority,
    /// A covered header field is missing.
    #[error("covered header `{0}` is missing")]
    MissingHeader(HeaderName),
    /// A covered query parameter is missing.
    #[error("covered query parameter `{0}` is missing")]
    MissingQueryParam(String),
    /// A covered query parameter appeared more than once.
    #[error("covered query parameter `{0}` appears more than once")]
    DuplicateQueryParam(String),
    /// A covered component is not supported by the current implementation.
    #[error("covered component {0} is not supported")]
    UnsupportedComponent(String),
    /// A covered component is not available in the selected message context.
    #[error("covered component {component} is not available in {context} context")]
    ComponentNotAvailable {
        /// The serialized component identifier.
        component: String,
        /// The target message context.
        context: &'static str,
    },
    /// A covered component uses parameters that are not supported yet.
    #[error("covered component {0} uses unsupported component parameters")]
    UnsupportedComponentParameters(String),
    /// A covered header value cannot be represented as an ASCII field value.
    #[error("covered header `{0}` contains a non-ASCII or otherwise unsupported value")]
    UnsupportedHeaderValue(HeaderName),
    /// A covered component value contains a newline.
    #[error("covered component value contains a newline")]
    NewlineInComponentValue,
    /// A covered component value contains an ASCII control character.
    #[error("covered component value contains an ASCII control character")]
    ControlCharacterInComponentValue,
    /// A covered component value starts or ends with whitespace.
    #[error("derived component value starts or ends with whitespace")]
    InvalidDerivedComponentWhitespace,
    /// A Structured Fields string parameter contains unsupported characters.
    #[error("signature parameter `{0}` is not a valid Structured Fields string")]
    InvalidStringParameter(String),
    /// A Structured Fields integer parameter is outside the valid range.
    #[error(
        "signature parameter `{parameter}` value {value} is outside the Structured Fields integer range"
    )]
    InvalidIntegerParameter {
        /// The generated parameter name.
        parameter: &'static str,
        /// The out-of-range value.
        value: u64,
    },
    /// The generated signature base contains non-ASCII characters.
    #[error("generated signature base contains non-ASCII characters")]
    NonAsciiSignatureBase,
    /// A generated header value was rejected by the `http` crate.
    #[error("generated `{header}` header value is invalid: {source}")]
    InvalidGeneratedHeader {
        /// The generated header name.
        header: &'static str,
        /// The header-value parser error.
        source: http::header::InvalidHeaderValue,
    },
    /// The caller-provided signer failed.
    #[error("message signer failed: {0}")]
    Signer(String),
}
