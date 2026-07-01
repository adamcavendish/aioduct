use http::header::{HeaderMap, HeaderName, HeaderValue};

use super::component::MessageSignatureComponentTarget;
use super::config::{validate_component_set, validate_label};
use super::headers::{ACCEPT_SIGNATURE, existing_dictionary, reject_duplicate_labels};
use super::params::AcceptSignatureParams;
use super::parsed::parse_accept_signature_member;
use super::structured_fields;
use super::{MessageSignatureComponent, MessageSignatureError};

/// Parsed or generated RFC 9421 `Accept-Signature` requests.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub struct AcceptSignature {
    entries: Vec<AcceptSignatureEntry>,
}

/// One labeled signature request from an RFC 9421 `Accept-Signature` field.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct AcceptSignatureEntry {
    label: String,
    components: Vec<MessageSignatureComponent>,
    params: AcceptSignatureParams,
    parsed_value: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AcceptSignatureTarget {
    Request,
    Response,
    RequestResponse,
}

impl AcceptSignature {
    /// Create an empty `Accept-Signature` field builder.
    pub fn new() -> Self {
        Self::default()
    }

    /// Parse an `Accept-Signature` field value.
    pub fn parse(value: &str) -> Result<Self, MessageSignatureError> {
        let entries = structured_fields::dictionary(value)
            .map_err(|_| MessageSignatureError::MalformedSignatureHeader(ACCEPT_SIGNATURE))?;
        Self::from_dictionary_entries(entries)
    }

    /// Parse combined `Accept-Signature` fields from a header map.
    pub fn from_headers(headers: &HeaderMap) -> Result<Self, MessageSignatureError> {
        Self::from_dictionary_entries(existing_dictionary(headers, ACCEPT_SIGNATURE)?)
    }

    /// Return the requested signature entries in field order.
    pub fn entries(&self) -> &[AcceptSignatureEntry] {
        &self.entries
    }

    /// Add one requested signature entry.
    pub fn entry(mut self, entry: AcceptSignatureEntry) -> Self {
        self.entries.push(entry);
        self
    }

    /// Format this value as an `Accept-Signature` header value.
    pub fn header_value(&self) -> Result<HeaderValue, MessageSignatureError> {
        let entries = self.dictionary_entries()?;
        let value = structured_fields::serialize_dictionary(&entries);
        HeaderValue::from_str(&value).map_err(|source| {
            MessageSignatureError::InvalidGeneratedHeader {
                header: "Accept-Signature",
                source,
            }
        })
    }

    /// Insert this value into a header map as `Accept-Signature`.
    pub fn insert_into(&self, headers: &mut HeaderMap) -> Result<(), MessageSignatureError> {
        headers.insert(
            HeaderName::from_static(ACCEPT_SIGNATURE),
            self.header_value()?,
        );
        Ok(())
    }

    /// Validate that all entries can target a request message.
    pub fn validate_request_target(&self) -> Result<(), MessageSignatureError> {
        self.validate_target(AcceptSignatureTarget::Request)
    }

    /// Validate that all entries can target a response message.
    pub fn validate_response_target(&self) -> Result<(), MessageSignatureError> {
        self.validate_target(AcceptSignatureTarget::Response)
    }

    /// Validate that all entries can target a response with its related request.
    pub fn validate_request_response_target(&self) -> Result<(), MessageSignatureError> {
        self.validate_target(AcceptSignatureTarget::RequestResponse)
    }

    fn from_dictionary_entries(
        dictionary_entries: Vec<(String, String)>,
    ) -> Result<Self, MessageSignatureError> {
        reject_duplicate_labels(ACCEPT_SIGNATURE, &dictionary_entries)?;
        let mut entries = Vec::with_capacity(dictionary_entries.len());
        for (label, member) in dictionary_entries {
            validate_label(&label)?;
            let (components, params) = parse_accept_signature_member(&member)?;
            validate_component_set(&components, false)?;
            entries.push(AcceptSignatureEntry {
                label,
                components,
                params,
                parsed_value: Some(member),
            });
        }
        Ok(Self { entries })
    }

    fn dictionary_entries(&self) -> Result<Vec<(String, String)>, MessageSignatureError> {
        let entries = self
            .entries
            .iter()
            .map(|entry| Ok((entry.label.clone(), entry.member_value()?)))
            .collect::<Result<Vec<_>, MessageSignatureError>>()?;
        reject_duplicate_labels(ACCEPT_SIGNATURE, &entries)?;
        Ok(entries)
    }

    fn validate_target(&self, target: AcceptSignatureTarget) -> Result<(), MessageSignatureError> {
        for entry in &self.entries {
            entry.validate_target(target)?;
        }
        Ok(())
    }
}

impl AcceptSignatureEntry {
    /// Create a signature request entry for the given output signature label.
    pub fn new(label: impl Into<String>) -> Result<Self, MessageSignatureError> {
        let label = label.into();
        validate_label(&label)?;
        Ok(Self {
            label,
            components: Vec::new(),
            params: AcceptSignatureParams::default(),
            parsed_value: None,
        })
    }

    /// Return the requested signature label.
    pub fn label(&self) -> &str {
        &self.label
    }

    /// Return the requested covered components in field order.
    pub fn components(&self) -> &[MessageSignatureComponent] {
        &self.components
    }

    /// Return the requested signature metadata parameters.
    pub fn params(&self) -> &AcceptSignatureParams {
        &self.params
    }

    /// Add one requested covered component.
    pub fn component(mut self, component: MessageSignatureComponent) -> Self {
        self.parsed_value = None;
        self.components.push(component);
        self
    }

    /// Add requested covered components in order.
    pub fn components_iter(
        mut self,
        components: impl IntoIterator<Item = MessageSignatureComponent>,
    ) -> Self {
        self.parsed_value = None;
        self.components.extend(components);
        self
    }

    /// Request that the signer include a generated `created` parameter.
    pub fn created(mut self) -> Self {
        self.parsed_value = None;
        self.params.created = true;
        self
    }

    /// Request that the signer include a generated `expires` parameter.
    pub fn expires(mut self) -> Self {
        self.parsed_value = None;
        self.params.expires = true;
        self
    }

    /// Request that the signer include the given `nonce` parameter.
    pub fn nonce(mut self, nonce: impl Into<String>) -> Self {
        self.parsed_value = None;
        self.params.nonce = Some(nonce.into());
        self
    }

    /// Request that the signer use the given `alg` parameter.
    pub fn algorithm(mut self, algorithm: impl Into<String>) -> Self {
        self.parsed_value = None;
        self.params.algorithm = Some(algorithm.into());
        self
    }

    /// Request that the signer use the given `keyid` parameter.
    pub fn key_id(mut self, key_id: impl Into<String>) -> Self {
        self.parsed_value = None;
        self.params.key_id = Some(key_id.into());
        self
    }

    /// Request that the signer include the given `tag` parameter.
    pub fn tag(mut self, tag: impl Into<String>) -> Self {
        self.parsed_value = None;
        self.params.tag = Some(tag.into());
        self
    }

    /// Validate that this request can target a request message.
    pub fn validate_request_target(&self) -> Result<(), MessageSignatureError> {
        self.validate_target(AcceptSignatureTarget::Request)
    }

    /// Validate that this request can target a response message.
    pub fn validate_response_target(&self) -> Result<(), MessageSignatureError> {
        self.validate_target(AcceptSignatureTarget::Response)
    }

    /// Validate that this request can target a response with its related request.
    pub fn validate_request_response_target(&self) -> Result<(), MessageSignatureError> {
        self.validate_target(AcceptSignatureTarget::RequestResponse)
    }

    fn member_value(&self) -> Result<String, MessageSignatureError> {
        validate_component_set(&self.components, false)?;
        if let Some(ref value) = self.parsed_value {
            return Ok(value.clone());
        }

        let mut value = String::new();
        value.push('(');
        for (index, component) in self.components.iter().enumerate() {
            if index > 0 {
                value.push(' ');
            }
            value.push_str(&component.identifier()?);
        }
        value.push(')');
        value.push_str(&self.params.serialize()?);
        Ok(value)
    }

    fn validate_target(&self, target: AcceptSignatureTarget) -> Result<(), MessageSignatureError> {
        validate_component_set(&self.components, false)?;
        for component in &self.components {
            validate_component_target(component, target)?;
        }
        Ok(())
    }
}

fn validate_component_target(
    component: &MessageSignatureComponent,
    target: AcceptSignatureTarget,
) -> Result<(), MessageSignatureError> {
    let identifier = component.identifier()?;
    if component.related_request_parameter_count() > 1 {
        return Err(MessageSignatureError::UnsupportedComponentParameters(
            identifier,
        ));
    }

    match target {
        AcceptSignatureTarget::Request => {
            if component.has_related_request_parameter() {
                return Err(MessageSignatureError::UnsupportedComponentParameters(
                    identifier,
                ));
            }
            if matches!(
                component.target(),
                MessageSignatureComponentTarget::Response
            ) {
                return Err(MessageSignatureError::ComponentNotAvailable {
                    component: identifier,
                    context: "request",
                });
            }
        }
        AcceptSignatureTarget::Response => {
            if component.has_related_request_parameter() {
                return Err(MessageSignatureError::ComponentNotAvailable {
                    component: identifier,
                    context: "response",
                });
            }
            if matches!(component.target(), MessageSignatureComponentTarget::Request) {
                return Err(MessageSignatureError::ComponentNotAvailable {
                    component: identifier,
                    context: "response",
                });
            }
        }
        AcceptSignatureTarget::RequestResponse => {
            if component.has_related_request_parameter() {
                if matches!(
                    component.target(),
                    MessageSignatureComponentTarget::Response
                ) {
                    return Err(MessageSignatureError::UnsupportedComponentParameters(
                        identifier,
                    ));
                }
            } else if matches!(component.target(), MessageSignatureComponentTarget::Request) {
                return Err(MessageSignatureError::ComponentNotAvailable {
                    component: identifier,
                    context: "response",
                });
            }
        }
    }

    Ok(())
}
