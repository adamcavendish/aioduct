use http::header::HeaderName;

use super::MessageSignatureError;
use super::params::serialize_sf_string;

/// A component covered by an RFC 9421 HTTP Message Signature.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
#[non_exhaustive]
pub struct MessageSignatureComponent {
    kind: MessageSignatureComponentKind,
    parameters: Vec<MessageSignatureComponentParameter>,
}

/// A parameter attached to an RFC 9421 covered component identifier.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
#[non_exhaustive]
pub enum MessageSignatureComponentParameter {
    /// The field value is an HTTP Structured Field and should use strict serialization (`;sf`).
    StructuredField,
    /// Select one key from a Dictionary Structured Field (`;key`).
    Key(String),
    /// Use byte-sequence wrapping for field values (`;bs`).
    ByteSequence,
    /// Read the field value from trailer fields (`;tr`).
    Trailer,
    /// Read this component from the related request when signing a response (`;req`).
    RelatedRequest,
    /// The named query parameter for `@query-param` (`;name`).
    Name(String),
}

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub(crate) enum MessageSignatureComponentKind {
    Method,
    Scheme,
    Authority,
    RequestTarget,
    TargetUri,
    Path,
    Query,
    QueryParam,
    Header(HeaderName),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum MessageSignatureComponentTarget {
    Request,
    RequestOrResponse,
}

impl MessageSignatureComponent {
    /// The request method (`@method`).
    pub fn method() -> Self {
        Self::new(MessageSignatureComponentKind::Method)
    }

    /// The target URI scheme (`@scheme`).
    pub fn scheme() -> Self {
        Self::new(MessageSignatureComponentKind::Scheme)
    }

    /// The target URI authority (`@authority`).
    pub fn authority() -> Self {
        Self::new(MessageSignatureComponentKind::Authority)
    }

    /// The actual request target sent on the wire (`@request-target`).
    pub fn request_target() -> Self {
        Self::new(MessageSignatureComponentKind::RequestTarget)
    }

    /// The full target URI (`@target-uri`).
    pub fn target_uri() -> Self {
        Self::new(MessageSignatureComponentKind::TargetUri)
    }

    /// The absolute path component of the target URI (`@path`).
    pub fn path() -> Self {
        Self::new(MessageSignatureComponentKind::Path)
    }

    /// The query component of the target URI (`@query`).
    pub fn query() -> Self {
        Self::new(MessageSignatureComponentKind::Query)
    }

    /// A named query parameter component (`@query-param`).
    pub fn query_param(name: impl Into<String>) -> Result<Self, MessageSignatureError> {
        Self::new(MessageSignatureComponentKind::QueryParam).name(name)
    }

    /// A request or response header field.
    pub fn header(name: HeaderName) -> Self {
        Self::new(MessageSignatureComponentKind::Header(name))
    }

    /// Return the covered component parameters in wire order.
    pub fn parameters(&self) -> &[MessageSignatureComponentParameter] {
        &self.parameters
    }

    /// Attach the `;sf` component parameter.
    pub fn structured_field(mut self) -> Self {
        self.parameters
            .push(MessageSignatureComponentParameter::StructuredField);
        self
    }

    /// Attach the `;key` component parameter.
    pub fn key(mut self, key: impl Into<String>) -> Result<Self, MessageSignatureError> {
        let key = key.into();
        validate_component_string(&key)?;
        self.parameters
            .push(MessageSignatureComponentParameter::Key(key));
        Ok(self)
    }

    /// Attach the `;bs` component parameter.
    pub fn byte_sequence(mut self) -> Self {
        self.parameters
            .push(MessageSignatureComponentParameter::ByteSequence);
        self
    }

    /// Attach the `;tr` component parameter.
    pub fn trailer(mut self) -> Self {
        self.parameters
            .push(MessageSignatureComponentParameter::Trailer);
        self
    }

    /// Attach the `;req` component parameter.
    pub fn related_request(mut self) -> Self {
        self.parameters
            .push(MessageSignatureComponentParameter::RelatedRequest);
        self
    }

    pub(crate) fn kind(&self) -> &MessageSignatureComponentKind {
        &self.kind
    }

    pub(crate) fn target(&self) -> MessageSignatureComponentTarget {
        match &self.kind {
            MessageSignatureComponentKind::Header(_) => {
                MessageSignatureComponentTarget::RequestOrResponse
            }
            _ => MessageSignatureComponentTarget::Request,
        }
    }

    pub(crate) fn has_parameters(&self) -> bool {
        !self.parameters.is_empty()
    }

    pub(crate) fn is_header_field(&self) -> bool {
        matches!(&self.kind, MessageSignatureComponentKind::Header(_))
    }

    pub(crate) fn identifier(&self) -> Result<String, MessageSignatureError> {
        let mut out = match &self.kind {
            MessageSignatureComponentKind::Method => "\"@method\"".to_owned(),
            MessageSignatureComponentKind::Scheme => "\"@scheme\"".to_owned(),
            MessageSignatureComponentKind::Authority => "\"@authority\"".to_owned(),
            MessageSignatureComponentKind::RequestTarget => "\"@request-target\"".to_owned(),
            MessageSignatureComponentKind::TargetUri => "\"@target-uri\"".to_owned(),
            MessageSignatureComponentKind::Path => "\"@path\"".to_owned(),
            MessageSignatureComponentKind::Query => "\"@query\"".to_owned(),
            MessageSignatureComponentKind::QueryParam => "\"@query-param\"".to_owned(),
            MessageSignatureComponentKind::Header(name) => {
                format!("\"{}\"", name.as_str().to_ascii_lowercase())
            }
        };
        for parameter in &self.parameters {
            parameter.write_to(&mut out)?;
        }
        Ok(out)
    }

    fn new(kind: MessageSignatureComponentKind) -> Self {
        Self {
            kind,
            parameters: Vec::new(),
        }
    }

    fn name(mut self, name: impl Into<String>) -> Result<Self, MessageSignatureError> {
        let name = name.into();
        validate_component_string(&name)?;
        self.parameters
            .push(MessageSignatureComponentParameter::Name(name));
        Ok(self)
    }
}

impl MessageSignatureComponentParameter {
    fn write_to(&self, out: &mut String) -> Result<(), MessageSignatureError> {
        match self {
            Self::StructuredField => out.push_str(";sf"),
            Self::Key(key) => {
                out.push_str(";key=");
                out.push_str(&serialize_sf_string(key)?);
            }
            Self::ByteSequence => out.push_str(";bs"),
            Self::Trailer => out.push_str(";tr"),
            Self::RelatedRequest => out.push_str(";req"),
            Self::Name(name) => {
                out.push_str(";name=");
                out.push_str(&serialize_sf_string(name)?);
            }
        }
        Ok(())
    }
}

fn validate_component_string(value: &str) -> Result<(), MessageSignatureError> {
    serialize_sf_string(value).map(|_| ())
}
