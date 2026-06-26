use http::header::HeaderName;

/// A component covered by an RFC 9421 HTTP Message Signature.
///
/// This initial surface supports request-side derived components and plain
/// header fields. Component parameters such as `;sf`, `;key`, `;bs`, `;tr`,
/// response `;req`, and trailer coverage are intentionally left for future
/// expansion.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
#[non_exhaustive]
pub enum MessageSignatureComponent {
    /// The request method (`@method`).
    Method,
    /// The target URI scheme (`@scheme`).
    Scheme,
    /// The target URI authority (`@authority`).
    Authority,
    /// The actual request target sent on the wire (`@request-target`).
    RequestTarget,
    /// The full target URI (`@target-uri`).
    TargetUri,
    /// The absolute path component of the target URI (`@path`).
    Path,
    /// The query component of the target URI (`@query`).
    Query,
    /// A request header field.
    Header {
        /// The header name to cover.
        name: HeaderName,
    },
}

impl MessageSignatureComponent {
    pub(crate) fn identifier(&self) -> String {
        match self {
            Self::Method => "\"@method\"".to_owned(),
            Self::Scheme => "\"@scheme\"".to_owned(),
            Self::Authority => "\"@authority\"".to_owned(),
            Self::RequestTarget => "\"@request-target\"".to_owned(),
            Self::TargetUri => "\"@target-uri\"".to_owned(),
            Self::Path => "\"@path\"".to_owned(),
            Self::Query => "\"@query\"".to_owned(),
            Self::Header { name } => format!("\"{}\"", name.as_str().to_ascii_lowercase()),
        }
    }
}
