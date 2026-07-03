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
    /// A covered Dictionary Structured Field member is missing.
    #[error("covered structured field `{field}` does not contain dictionary key `{key}`")]
    MissingDictionaryKey {
        /// The covered Dictionary Structured Field name.
        field: HeaderName,
        /// The selected Dictionary member key.
        key: String,
    },
    /// A covered Structured Field value is malformed.
    #[error("covered structured field `{0}` is malformed")]
    MalformedStructuredField(HeaderName),
    /// A covered Structured Field uses `;sf` without a configured top-level type.
    #[error("covered structured field `{0}` has no configured Structured Fields type")]
    UnknownStructuredFieldType(HeaderName),
    /// An existing signature header is not a valid Structured Fields dictionary.
    #[error("existing `{0}` header is malformed")]
    MalformedSignatureHeader(&'static str),
    /// An existing signature header contains the same signature label more than once.
    #[error("existing `{header}` header contains duplicate signature label `{label}`")]
    DuplicateSignatureLabel {
        /// The signature header name.
        header: &'static str,
        /// The duplicate dictionary member label.
        label: String,
    },
    /// Existing `Signature-Input` and `Signature` dictionaries cover different labels.
    #[error("existing `Signature-Input` and `Signature` labels do not match")]
    MismatchedSignatureLabels,
    /// The requested signature label is not present.
    #[error("signature label `{0}` is not present")]
    MissingSignatureLabel(String),
    /// A covered component is not supported by the current implementation.
    #[error("covered component {0} is not supported")]
    UnsupportedComponent(String),
    /// A verification policy required a component that was not covered.
    #[error("signature does not cover required component {0}")]
    MissingRequiredComponent(String),
    /// A verification policy required a metadata parameter that is missing.
    #[error("signature metadata parameter `{0}` is missing")]
    MissingSignatureParameter(&'static str),
    /// A covered `Content-Digest` field is malformed.
    #[error("covered `Content-Digest` header is malformed")]
    MalformedContentDigest,
    /// A covered `Content-Digest` field does not include a supported digest algorithm.
    #[error("covered `Content-Digest` header does not include a supported digest algorithm")]
    UnsupportedContentDigestAlgorithm,
    /// The supplied body bytes do not match the covered `Content-Digest` field.
    #[error("covered `Content-Digest` header does not match the supplied body")]
    ContentDigestMismatch,
    /// An `Accept-Signature` requested metadata parameter cannot be fulfilled.
    #[error("Accept-Signature requested metadata parameter `{0}` cannot be fulfilled")]
    UnfulfillableAcceptSignatureParameter(&'static str),
    /// A verification policy needs a validation time but none was configured.
    #[error("signature verification policy needs a validation time")]
    MissingValidationTime,
    /// The signature's algorithm metadata is not accepted by policy.
    #[error("signature algorithm is not accepted")]
    UnacceptableAlgorithm(Option<String>),
    /// The signature's key identifier metadata is not accepted by policy.
    #[error("signature key id is not accepted")]
    UnknownKeyId(Option<String>),
    /// The signature expired before the policy validation time.
    #[error("signature expired at {expires}, before validation time {now}")]
    SignatureExpired {
        /// The `expires` metadata value.
        expires: u64,
        /// The validation time.
        now: u64,
    },
    /// The signature creation time is after the policy validation time.
    #[error("signature was created at {created}, after validation time {now}")]
    SignatureCreatedInFuture {
        /// The `created` metadata value.
        created: u64,
        /// The validation time.
        now: u64,
    },
    /// The signature is older than the policy's maximum age.
    #[error(
        "signature created at {created} is older than maximum age {max_age} at validation time {now}"
    )]
    SignatureTooOld {
        /// The `created` metadata value.
        created: u64,
        /// The validation time.
        now: u64,
        /// The maximum accepted age in seconds.
        max_age: u64,
    },
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
    /// The caller-provided verifier rejected the signature.
    #[error("message signature verification failed")]
    VerificationFailed,
    /// The caller-provided verifier failed.
    #[error("message verifier failed: {0}")]
    Verifier(String),
}
