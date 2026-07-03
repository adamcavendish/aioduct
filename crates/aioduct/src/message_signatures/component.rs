use http::header::HeaderName;

use super::MessageSignatureError;
use super::params::serialize_sf_string;
use crate::structured_fields;

/// A component covered by an RFC 9421 HTTP Message Signature.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
#[non_exhaustive]
pub struct MessageSignatureComponent {
    kind: MessageSignatureComponentKind,
    parameters: Vec<MessageSignatureComponentParameter>,
    structured_field_type: Option<MessageSignatureStructuredFieldType>,
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
    /// The percent-encoded named query parameter for `@query-param` (`;name`).
    Name(String),
}

/// The RFC 9651 top-level type expected for a `;sf` covered field component.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
#[non_exhaustive]
pub enum MessageSignatureStructuredFieldType {
    /// A Dictionary Structured Field.
    Dictionary,
    /// A List Structured Field.
    List,
    /// An Item Structured Field.
    Item,
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
    Status,
    Header(HeaderName),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum MessageSignatureComponentTarget {
    Request,
    Response,
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
    ///
    /// The input is the decoded parameter name. It is serialized using the
    /// percent-encoded form required by RFC 9421's `;name` parameter.
    pub fn query_param(name: impl Into<String>) -> Result<Self, MessageSignatureError> {
        let name = name.into();
        let name = encode_query_param_component(&name);
        Self::new(MessageSignatureComponentKind::QueryParam).name(name)
    }

    /// The response status code (`@status`).
    pub fn status() -> Self {
        Self::new(MessageSignatureComponentKind::Status)
    }

    /// A request or response header field.
    pub fn header(name: HeaderName) -> Self {
        Self::new(MessageSignatureComponentKind::Header(name))
    }

    /// Return the covered component parameters in wire order.
    pub fn parameters(&self) -> &[MessageSignatureComponentParameter] {
        &self.parameters
    }

    /// Attach the `;sf` component parameter with the field's known Structured Field type.
    pub fn structured_field(mut self, field_type: MessageSignatureStructuredFieldType) -> Self {
        self.parameters
            .push(MessageSignatureComponentParameter::StructuredField);
        self.structured_field_type = Some(field_type);
        self
    }

    /// Attach `;sf` for a Dictionary Structured Field.
    pub fn structured_dictionary(self) -> Self {
        self.structured_field(MessageSignatureStructuredFieldType::Dictionary)
    }

    /// Attach `;sf` for a List Structured Field.
    pub fn structured_list(self) -> Self {
        self.structured_field(MessageSignatureStructuredFieldType::List)
    }

    /// Attach `;sf` for an Item Structured Field.
    pub fn structured_item(self) -> Self {
        self.structured_field(MessageSignatureStructuredFieldType::Item)
    }

    /// Attach the `;key` component parameter.
    pub fn key(mut self, key: impl Into<String>) -> Result<Self, MessageSignatureError> {
        let key = key.into();
        validate_component_string(&key)?;
        self.parameters
            .push(MessageSignatureComponentParameter::Key(key));
        if self.has_structured_field_parameter() {
            self.structured_field_type = Some(MessageSignatureStructuredFieldType::Dictionary);
        }
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

    pub(crate) fn parsed_query_param() -> Self {
        Self::new(MessageSignatureComponentKind::QueryParam)
    }

    pub(crate) fn target(&self) -> MessageSignatureComponentTarget {
        match &self.kind {
            MessageSignatureComponentKind::Header(_) => {
                MessageSignatureComponentTarget::RequestOrResponse
            }
            MessageSignatureComponentKind::Status => MessageSignatureComponentTarget::Response,
            _ => MessageSignatureComponentTarget::Request,
        }
    }

    pub(crate) fn related_request_parameter_count(&self) -> usize {
        self.parameters
            .iter()
            .filter(|parameter| {
                matches!(
                    parameter,
                    MessageSignatureComponentParameter::RelatedRequest
                )
            })
            .count()
    }

    pub(crate) fn trailer_parameter_count(&self) -> usize {
        self.parameters
            .iter()
            .filter(|parameter| matches!(parameter, MessageSignatureComponentParameter::Trailer))
            .count()
    }

    pub(crate) fn has_related_request_parameter(&self) -> bool {
        self.related_request_parameter_count() > 0
    }

    pub(crate) fn has_trailer_parameter(&self) -> bool {
        self.trailer_parameter_count() > 0
    }

    pub(crate) fn without_related_request_parameter(&self) -> Self {
        let mut out = self.clone();
        out.parameters.retain(|parameter| {
            !matches!(
                parameter,
                MessageSignatureComponentParameter::RelatedRequest
            )
        });
        out
    }

    pub(crate) fn without_trailer_parameter(&self) -> Self {
        let mut out = self.clone();
        out.parameters
            .retain(|parameter| !matches!(parameter, MessageSignatureComponentParameter::Trailer));
        out
    }

    pub(crate) fn has_parameters(&self) -> bool {
        !self.parameters.is_empty()
    }

    pub(crate) fn query_param_name(&self) -> Option<&str> {
        if !matches!(&self.kind, MessageSignatureComponentKind::QueryParam) {
            return None;
        }
        match self.parameters.as_slice() {
            [MessageSignatureComponentParameter::Name(name)] => Some(name),
            _ => None,
        }
    }

    pub(crate) fn dictionary_key(&self) -> Option<&str> {
        if !self.is_header_field() {
            return None;
        }

        let mut key = None;
        let mut structured_field = false;
        for parameter in &self.parameters {
            match parameter {
                MessageSignatureComponentParameter::StructuredField if !structured_field => {
                    structured_field = true;
                }
                MessageSignatureComponentParameter::Key(value) if key.is_none() => {
                    key = Some(value.as_str());
                }
                MessageSignatureComponentParameter::Trailer
                | MessageSignatureComponentParameter::RelatedRequest => {}
                _ => return None,
            }
        }
        key
    }

    pub(crate) fn structured_field_type(&self) -> Option<MessageSignatureStructuredFieldType> {
        self.structured_field_type
    }

    pub(crate) fn has_structured_field_parameter(&self) -> bool {
        self.parameters.iter().any(|parameter| {
            matches!(
                parameter,
                MessageSignatureComponentParameter::StructuredField
            )
        })
    }

    pub(crate) fn structured_field_type_identity(
        &self,
    ) -> Option<(HeaderName, MessageSignatureStructuredFieldType)> {
        let MessageSignatureComponentKind::Header(name) = &self.kind else {
            return None;
        };
        if self.has_structured_field_parameter() {
            self.structured_field_type
                .map(|field_type| (name.clone(), field_type))
        } else {
            None
        }
    }

    pub(crate) fn set_structured_field_type_for_header(
        &mut self,
        name: &HeaderName,
        field_type: MessageSignatureStructuredFieldType,
    ) {
        if matches!(&self.kind, MessageSignatureComponentKind::Header(header) if header == name)
            && self.has_structured_field_parameter()
        {
            self.structured_field_type = Some(field_type);
        }
    }

    pub(crate) fn dictionary_key_identity(&self) -> Option<(HeaderName, String, bool, bool)> {
        let MessageSignatureComponentKind::Header(name) = &self.kind else {
            return None;
        };
        self.dictionary_key().map(|key| {
            (
                name.clone(),
                key.to_owned(),
                self.has_trailer_parameter(),
                self.has_related_request_parameter(),
            )
        })
    }

    pub(crate) fn with_parsed_parameter(
        mut self,
        parameter: MessageSignatureComponentParameter,
    ) -> Result<Self, MessageSignatureError> {
        match &parameter {
            MessageSignatureComponentParameter::Key(value)
            | MessageSignatureComponentParameter::Name(value) => validate_component_string(value)?,
            MessageSignatureComponentParameter::StructuredField
            | MessageSignatureComponentParameter::ByteSequence
            | MessageSignatureComponentParameter::Trailer
            | MessageSignatureComponentParameter::RelatedRequest => {}
        }
        self.parameters.push(parameter);
        Ok(self)
    }

    pub(crate) fn has_only_byte_sequence_parameter(&self) -> bool {
        matches!(
            self.parameters.as_slice(),
            [MessageSignatureComponentParameter::ByteSequence]
        )
    }

    pub(crate) fn has_only_structured_field_parameter(&self) -> bool {
        matches!(
            self.parameters.as_slice(),
            [MessageSignatureComponentParameter::StructuredField]
        )
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
            MessageSignatureComponentKind::Status => "\"@status\"".to_owned(),
            MessageSignatureComponentKind::Header(name) => {
                format!("\"{}\"", name.as_str().to_ascii_lowercase())
            }
        };
        for parameter in &self.parameters {
            parameter.write_to(&mut out)?;
        }
        Ok(out)
    }

    pub(crate) fn comparison_key(&self) -> String {
        let name = match &self.kind {
            MessageSignatureComponentKind::Method => "@method".to_owned(),
            MessageSignatureComponentKind::Scheme => "@scheme".to_owned(),
            MessageSignatureComponentKind::Authority => "@authority".to_owned(),
            MessageSignatureComponentKind::RequestTarget => "@request-target".to_owned(),
            MessageSignatureComponentKind::TargetUri => "@target-uri".to_owned(),
            MessageSignatureComponentKind::Path => "@path".to_owned(),
            MessageSignatureComponentKind::Query => "@query".to_owned(),
            MessageSignatureComponentKind::QueryParam => "@query-param".to_owned(),
            MessageSignatureComponentKind::Status => "@status".to_owned(),
            MessageSignatureComponentKind::Header(name) => name.as_str().to_ascii_lowercase(),
        };
        let mut parameters = self
            .parameters
            .iter()
            .map(MessageSignatureComponentParameter::comparison_key)
            .collect::<Vec<_>>();
        parameters.sort();
        if parameters.is_empty() {
            name
        } else {
            format!("{};{}", name, parameters.join(";"))
        }
    }

    fn new(kind: MessageSignatureComponentKind) -> Self {
        Self {
            kind,
            parameters: Vec::new(),
            structured_field_type: None,
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

impl MessageSignatureStructuredFieldType {
    pub(crate) fn into_structured_field_type(self) -> structured_fields::StructuredFieldType {
        match self {
            Self::Dictionary => structured_fields::StructuredFieldType::Dictionary,
            Self::List => structured_fields::StructuredFieldType::List,
            Self::Item => structured_fields::StructuredFieldType::Item,
        }
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

    fn comparison_key(&self) -> String {
        match self {
            Self::StructuredField => "sf".to_owned(),
            Self::Key(key) => format!("key:{}:{key}", key.len()),
            Self::ByteSequence => "bs".to_owned(),
            Self::Trailer => "tr".to_owned(),
            Self::RelatedRequest => "req".to_owned(),
            Self::Name(name) => format!("name:{}:{name}", name.len()),
        }
    }
}

fn validate_component_string(value: &str) -> Result<(), MessageSignatureError> {
    serialize_sf_string(value).map(|_| ())
}

pub(crate) fn encode_query_param_component(value: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";

    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'*' | b'-' | b'.' | b'_') {
            out.push(byte as char);
        } else {
            out.push('%');
            out.push(HEX[(byte >> 4) as usize] as char);
            out.push(HEX[(byte & 0x0f) as usize] as char);
        }
    }
    out
}
