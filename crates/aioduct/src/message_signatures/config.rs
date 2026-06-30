use std::collections::HashSet;

use base64::Engine as _;
use http::header::{HeaderMap, HeaderValue};
use http::{Method, Uri};

use super::{
    MessageSignatureBase, MessageSignatureComponent, MessageSignatureContext,
    MessageSignatureError, MessageSignatureHeaders, MessageSignatureParams, MessageSignatureSigner,
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
        let context = MessageSignatureContext::request(method, target_uri, request_target, headers);
        self.signature_base_for_context(&context)
    }

    pub(crate) fn signature_base_for_context(
        &self,
        context: &MessageSignatureContext<'_>,
    ) -> Result<MessageSignatureBase, MessageSignatureError> {
        self.validate_components()?;

        let mut lines = Vec::with_capacity(self.components.len() + 1);
        for component in &self.components {
            let identifier = component.identifier()?;
            let value = context.component_value(component)?;
            ensure_component_value(component, &value)?;
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
        signer: &(impl MessageSignatureSigner + ?Sized),
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
            let identifier = component.identifier()?;
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
            out.push_str(&component.identifier()?);
        }
        out.push(')');
        out.push_str(&self.params.serialize()?);
        Ok(out)
    }
}

fn ensure_component_value(
    component: &MessageSignatureComponent,
    value: &str,
) -> Result<(), MessageSignatureError> {
    if value.contains('\n') || value.contains('\r') {
        return Err(MessageSignatureError::NewlineInComponentValue);
    }
    if value
        .chars()
        .any(|c| c.is_ascii_control() && (c != '\t' || !component.is_header_field()))
    {
        return Err(MessageSignatureError::ControlCharacterInComponentValue);
    }
    if !component.is_header_field()
        && (value.chars().next().is_some_and(char::is_whitespace)
            || value.chars().last().is_some_and(char::is_whitespace))
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
