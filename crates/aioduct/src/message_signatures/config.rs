use std::collections::HashSet;

use base64::Engine as _;
use http::header::{HeaderMap, HeaderName, HeaderValue};
use http::{Method, Uri};

use super::{
    MessageSignatureBase, MessageSignatureComponent, MessageSignatureError,
    MessageSignatureHeaders, MessageSignatureParams, MessageSignatureSigner,
};

/// Configuration for generating an RFC 9421 request signature base and headers.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct MessageSignatureConfig {
    label: String,
    components: Vec<MessageSignatureComponent>,
    params: MessageSignatureParams,
}

impl MessageSignatureConfig {
    /// Create a signature configuration for the given signature label.
    ///
    /// The label is serialized as a Structured Fields dictionary key in both
    /// `Signature-Input` and `Signature`.
    pub fn new(label: impl Into<String>) -> Result<Self, MessageSignatureError> {
        let label = label.into();
        validate_label(&label)?;
        Ok(Self {
            label,
            components: Vec::new(),
            params: MessageSignatureParams::default(),
        })
    }

    /// Return the signature label.
    pub fn label(&self) -> &str {
        &self.label
    }

    /// Return the ordered covered component list.
    pub fn components(&self) -> &[MessageSignatureComponent] {
        &self.components
    }

    /// Return the configured signature metadata parameters.
    pub fn params(&self) -> &MessageSignatureParams {
        &self.params
    }

    /// Add one covered component.
    pub fn component(mut self, component: MessageSignatureComponent) -> Self {
        self.components.push(component);
        self
    }

    /// Add covered components in order.
    pub fn components_iter(
        mut self,
        components: impl IntoIterator<Item = MessageSignatureComponent>,
    ) -> Self {
        self.components.extend(components);
        self
    }

    /// Set the `created` metadata parameter.
    pub fn created(mut self, created: u64) -> Self {
        self.params.created = Some(created);
        self
    }

    /// Set the `expires` metadata parameter.
    pub fn expires(mut self, expires: u64) -> Self {
        self.params.expires = Some(expires);
        self
    }

    /// Set the `nonce` metadata parameter.
    pub fn nonce(mut self, nonce: impl Into<String>) -> Self {
        self.params.nonce = Some(nonce.into());
        self
    }

    /// Set the `alg` metadata parameter.
    pub fn algorithm(mut self, algorithm: impl Into<String>) -> Self {
        self.params.algorithm = Some(algorithm.into());
        self
    }

    /// Set the `keyid` metadata parameter.
    pub fn key_id(mut self, key_id: impl Into<String>) -> Self {
        self.params.key_id = Some(key_id.into());
        self
    }

    /// Set the `tag` metadata parameter.
    pub fn tag(mut self, tag: impl Into<String>) -> Self {
        self.params.tag = Some(tag.into());
        self
    }

    /// Build the RFC 9421 signature base for a request.
    ///
    /// `target_uri` is the full request URI used for scheme, authority, path,
    /// query, and target-uri derived components. `request_target` is the final
    /// request URI form that will be sent on the wire, used for
    /// `@request-target`.
    pub fn signature_base(
        &self,
        method: &Method,
        target_uri: &Uri,
        request_target: &Uri,
        headers: &HeaderMap,
    ) -> Result<MessageSignatureBase, MessageSignatureError> {
        self.validate_components()?;

        let mut lines = Vec::with_capacity(self.components.len() + 1);
        for component in &self.components {
            let identifier = component.identifier();
            let value = component_value(component, method, target_uri, request_target, headers)?;
            match component {
                MessageSignatureComponent::Header { .. } => ensure_header_component_value(&value)?,
                _ => ensure_derived_component_value(&value)?,
            }
            lines.push(format!("{identifier}: {value}"));
        }

        let signature_params = self.signature_params_value()?;
        lines.push(format!("\"@signature-params\": {signature_params}"));
        let value = lines.join("\n");
        if !value.is_ascii() {
            return Err(MessageSignatureError::NonAsciiSignatureBase);
        }
        Ok(MessageSignatureBase::new(value))
    }

    /// Format `Signature-Input` and `Signature` header values from signature bytes.
    pub fn headers_from_signature(
        &self,
        signature: impl AsRef<[u8]>,
    ) -> Result<MessageSignatureHeaders, MessageSignatureError> {
        self.validate_components()?;

        let signature_params = self.signature_params_value()?;
        let signature_input = format!("{}={signature_params}", self.label);
        let signature = format!(
            "{}=:{}:",
            self.label,
            base64::engine::general_purpose::STANDARD.encode(signature.as_ref())
        );

        Ok(MessageSignatureHeaders {
            signature_input: HeaderValue::from_str(&signature_input).map_err(|source| {
                MessageSignatureError::InvalidGeneratedHeader {
                    header: "Signature-Input",
                    source,
                }
            })?,
            signature: HeaderValue::from_str(&signature).map_err(|source| {
                MessageSignatureError::InvalidGeneratedHeader {
                    header: "Signature",
                    source,
                }
            })?,
        })
    }

    /// Build the signature base, sign it, and format signature headers.
    pub fn sign_request(
        &self,
        method: &Method,
        target_uri: &Uri,
        request_target: &Uri,
        headers: &HeaderMap,
        signer: &impl MessageSignatureSigner,
    ) -> Result<MessageSignatureHeaders, MessageSignatureError> {
        let base = self.signature_base(method, target_uri, request_target, headers)?;
        let signature = signer.sign(base.as_bytes())?;
        self.headers_from_signature(signature)
    }

    fn validate_components(&self) -> Result<(), MessageSignatureError> {
        if self.components.is_empty() {
            return Err(MessageSignatureError::EmptyComponents);
        }

        let mut seen = HashSet::new();
        for component in &self.components {
            let identifier = component.identifier();
            if !seen.insert(identifier.clone()) {
                return Err(MessageSignatureError::DuplicateComponent(identifier));
            }
        }
        Ok(())
    }

    fn signature_params_value(&self) -> Result<String, MessageSignatureError> {
        let mut out = String::new();
        out.push('(');
        for (index, component) in self.components.iter().enumerate() {
            if index > 0 {
                out.push(' ');
            }
            out.push_str(&component.identifier());
        }
        out.push(')');
        out.push_str(&self.params.serialize()?);
        Ok(out)
    }
}

fn component_value(
    component: &MessageSignatureComponent,
    method: &Method,
    target_uri: &Uri,
    request_target: &Uri,
    headers: &HeaderMap,
) -> Result<String, MessageSignatureError> {
    match component {
        MessageSignatureComponent::Method => Ok(method.as_str().to_owned()),
        MessageSignatureComponent::Scheme => target_uri
            .scheme_str()
            .map(|scheme| scheme.to_ascii_lowercase())
            .ok_or(MessageSignatureError::MissingScheme),
        MessageSignatureComponent::Authority => canonical_authority(target_uri),
        MessageSignatureComponent::RequestTarget => Ok(request_target.to_string()),
        MessageSignatureComponent::TargetUri => Ok(target_uri.to_string()),
        MessageSignatureComponent::Path => {
            let path = target_uri.path();
            if path.is_empty() {
                Ok("/".to_owned())
            } else {
                Ok(path.to_owned())
            }
        }
        MessageSignatureComponent::Query => Ok(match target_uri.query() {
            Some(query) => format!("?{query}"),
            None => "?".to_owned(),
        }),
        MessageSignatureComponent::Header { name } => canonical_header_value(headers, name),
    }
}

fn canonical_authority(uri: &Uri) -> Result<String, MessageSignatureError> {
    let scheme = uri.scheme_str().map(|s| s.to_ascii_lowercase());
    let authority = uri
        .authority()
        .ok_or(MessageSignatureError::MissingAuthority)?;
    let host = authority.host().to_ascii_lowercase();
    let port = authority.port_u16();
    let default_port = matches!(
        (scheme.as_deref(), port),
        (Some("http"), Some(80)) | (Some("https"), Some(443))
    );
    if default_port {
        Ok(host)
    } else if let Some(port) = port {
        Ok(format!("{host}:{port}"))
    } else {
        Ok(host)
    }
}

fn canonical_header_value(
    headers: &HeaderMap,
    name: &HeaderName,
) -> Result<String, MessageSignatureError> {
    let values = headers.get_all(name);
    let mut out = Vec::new();
    for value in values {
        let value = value
            .to_str()
            .map_err(|_| MessageSignatureError::UnsupportedHeaderValue(name.clone()))?;
        let value = normalize_field_value(value);
        if value.contains('\n') || value.contains('\r') {
            return Err(MessageSignatureError::NewlineInComponentValue);
        }
        out.push(value);
    }
    if out.is_empty() {
        return Err(MessageSignatureError::MissingHeader(name.clone()));
    }
    Ok(out.join(", "))
}

fn normalize_field_value(value: &str) -> String {
    value.trim_matches(|c| c == ' ' || c == '\t').to_owned()
}

fn ensure_header_component_value(value: &str) -> Result<(), MessageSignatureError> {
    if value.contains('\n') || value.contains('\r') {
        return Err(MessageSignatureError::NewlineInComponentValue);
    }
    if value.chars().any(|c| c.is_ascii_control() && c != '\t') {
        return Err(MessageSignatureError::ControlCharacterInComponentValue);
    }
    Ok(())
}

fn ensure_derived_component_value(value: &str) -> Result<(), MessageSignatureError> {
    if value.contains('\n') || value.contains('\r') {
        return Err(MessageSignatureError::NewlineInComponentValue);
    }
    if value.chars().any(|c| c.is_ascii_control()) {
        return Err(MessageSignatureError::ControlCharacterInComponentValue);
    }
    if value.chars().next().is_some_and(char::is_whitespace)
        || value.chars().last().is_some_and(char::is_whitespace)
    {
        return Err(MessageSignatureError::InvalidDerivedComponentWhitespace);
    }
    Ok(())
}

fn validate_label(label: &str) -> Result<(), MessageSignatureError> {
    let mut chars = label.chars();
    let Some(first) = chars.next() else {
        return Err(MessageSignatureError::InvalidLabel(label.to_owned()));
    };
    if !(first.is_ascii_lowercase() || first == '*') {
        return Err(MessageSignatureError::InvalidLabel(label.to_owned()));
    }
    if chars.any(|c| {
        !(c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '_' | '-' | '.' | '*'))
    }) {
        return Err(MessageSignatureError::InvalidLabel(label.to_owned()));
    }
    Ok(())
}
