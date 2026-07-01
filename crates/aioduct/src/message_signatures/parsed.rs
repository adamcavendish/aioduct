use base64::Engine as _;
use http::header::{HeaderMap, HeaderName};
use http::{Method, Uri};

use super::config::{ensure_component_value, validate_component_set, validate_label};
use super::headers::{
    SIGNATURE, SIGNATURE_INPUT, ensure_matching_labels, existing_dictionary,
    reject_duplicate_labels,
};
use super::{
    MessageSignatureBase, MessageSignatureComponent, MessageSignatureComponentParameter,
    MessageSignatureContext, MessageSignatureError, MessageSignatureParams,
};

/// Parsed RFC 9421 signature material selected by label.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct MessageSignature {
    label: String,
    components: Vec<MessageSignatureComponent>,
    params: MessageSignatureParams,
    signature: Vec<u8>,
    signature_params_value: String,
}

impl MessageSignature {
    /// Parse `Signature-Input` and `Signature` fields and select one label.
    pub fn from_headers(
        headers: &HeaderMap,
        label: impl AsRef<str>,
    ) -> Result<Self, MessageSignatureError> {
        let label = label.as_ref();
        validate_label(label)?;

        let signature_input = existing_dictionary(headers, SIGNATURE_INPUT)?;
        let signature = existing_dictionary(headers, SIGNATURE)?;
        reject_duplicate_labels(SIGNATURE_INPUT, &signature_input)?;
        reject_duplicate_labels(SIGNATURE, &signature)?;
        ensure_matching_labels(&signature_input, &signature)?;

        let signature_params_value = find_label(&signature_input, label)?;
        let signature_value = find_label(&signature, label)?;
        let (components, params) = parse_signature_input_member(signature_params_value)?;
        validate_component_set(&components, false)?;
        let signature = parse_signature_bytes(signature_value)?;

        Ok(Self {
            label: label.to_owned(),
            components,
            params,
            signature,
            signature_params_value: signature_params_value.to_owned(),
        })
    }

    /// Return the selected signature label.
    pub fn label(&self) -> &str {
        &self.label
    }

    /// Return the ordered covered components from `Signature-Input`.
    pub fn components(&self) -> &[MessageSignatureComponent] {
        &self.components
    }

    /// Return the known signature metadata parameters.
    pub fn params(&self) -> &MessageSignatureParams {
        &self.params
    }

    /// Return the signature bytes from the matching `Signature` member.
    pub fn signature(&self) -> &[u8] {
        &self.signature
    }

    /// Return the canonical `@signature-params` value for this signature.
    pub fn signature_params_value(&self) -> &str {
        &self.signature_params_value
    }

    /// Rebuild the RFC 9421 signature base for a request.
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
        validate_component_set(&self.components, false)?;

        let mut lines = Vec::with_capacity(self.components.len() + 1);
        for component in &self.components {
            let identifier = component.identifier()?;
            let value = context.component_value(component)?;
            ensure_component_value(component, &value)?;
            lines.push(format!("{identifier}: {value}"));
        }
        lines.push(format!(
            "\"@signature-params\": {}",
            self.signature_params_value
        ));

        let value = lines.join("\n");
        if !value.is_ascii() {
            return Err(MessageSignatureError::NonAsciiSignatureBase);
        }
        Ok(MessageSignatureBase::new(value))
    }
}

fn find_label<'a>(
    entries: &'a [(String, String)],
    label: &str,
) -> Result<&'a str, MessageSignatureError> {
    entries
        .iter()
        .find(|(entry_label, _)| entry_label == label)
        .map(|(_, value)| value.as_str())
        .ok_or_else(|| MessageSignatureError::MissingSignatureLabel(label.to_owned()))
}

fn parse_signature_bytes(member: &str) -> Result<Vec<u8>, MessageSignatureError> {
    let Some(encoded) = member
        .strip_prefix(':')
        .and_then(|value| value.strip_suffix(':'))
    else {
        return Err(MessageSignatureError::MalformedSignatureHeader(SIGNATURE));
    };
    if encoded.contains(';') {
        return Err(MessageSignatureError::MalformedSignatureHeader(SIGNATURE));
    }
    base64::engine::general_purpose::STANDARD
        .decode(encoded.as_bytes())
        .map_err(|_| MessageSignatureError::MalformedSignatureHeader(SIGNATURE))
}

fn parse_signature_input_member(
    member: &str,
) -> Result<(Vec<MessageSignatureComponent>, MessageSignatureParams), MessageSignatureError> {
    let mut parser = SignatureInputParser::new(member);
    parser.parse()
}

struct SignatureInputParser<'a> {
    input: &'a [u8],
    pos: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum ParameterValue {
    String(String),
    Integer(i64),
    Boolean(bool),
    Other,
}

impl<'a> SignatureInputParser<'a> {
    fn new(input: &'a str) -> Self {
        Self {
            input: input.as_bytes(),
            pos: 0,
        }
    }

    fn parse(
        &mut self,
    ) -> Result<(Vec<MessageSignatureComponent>, MessageSignatureParams), MessageSignatureError>
    {
        if !self.consume_if(b'(') {
            return Err(MessageSignatureError::MalformedSignatureHeader(
                SIGNATURE_INPUT,
            ));
        }

        let mut components = Vec::new();
        if self.consume_if(b')') {
            let params = self.parse_signature_params()?;
            self.ensure_empty()?;
            return Ok((components, params));
        }

        loop {
            components.push(self.parse_component()?);
            match self.peek() {
                Some(b')') => {
                    self.consume();
                    let params = self.parse_signature_params()?;
                    self.ensure_empty()?;
                    return Ok((components, params));
                }
                Some(b' ') => {
                    self.consume();
                    if matches!(self.peek(), Some(b' ' | b')') | None) {
                        return Err(MessageSignatureError::MalformedSignatureHeader(
                            SIGNATURE_INPUT,
                        ));
                    }
                }
                _ => {
                    return Err(MessageSignatureError::MalformedSignatureHeader(
                        SIGNATURE_INPUT,
                    ));
                }
            }
        }
    }

    fn parse_component(&mut self) -> Result<MessageSignatureComponent, MessageSignatureError> {
        let component_name = self.parse_string()?;
        let mut component = component_from_name(&component_name)?;

        while self.consume_if(b';') {
            let key = self.parse_key()?;
            let value = if self.consume_if(b'=') {
                Some(self.parse_parameter_value()?)
            } else {
                None
            };
            let parameter = component_parameter(&key, value)?;
            component = component.with_parsed_parameter(parameter)?;
        }
        Ok(component)
    }

    fn parse_signature_params(&mut self) -> Result<MessageSignatureParams, MessageSignatureError> {
        let mut params = MessageSignatureParams::default();
        while self.consume_if(b';') {
            let key = self.parse_key()?;
            let value = if self.consume_if(b'=') {
                Some(self.parse_parameter_value()?)
            } else {
                None
            };
            match key.as_str() {
                "created" => {
                    params.created = Some(required_u64_parameter(value)?);
                }
                "expires" => {
                    params.expires = Some(required_u64_parameter(value)?);
                }
                "nonce" => {
                    params.nonce = Some(required_string_parameter(value)?);
                }
                "alg" => {
                    params.algorithm = Some(required_string_parameter(value)?);
                }
                "keyid" => {
                    params.key_id = Some(required_string_parameter(value)?);
                }
                "tag" => {
                    params.tag = Some(required_string_parameter(value)?);
                }
                _ => {}
            }
        }
        Ok(params)
    }

    fn parse_parameter_value(&mut self) -> Result<ParameterValue, MessageSignatureError> {
        match self.peek() {
            Some(b'"') => self.parse_string().map(ParameterValue::String),
            Some(b'?') => self.parse_boolean().map(ParameterValue::Boolean),
            Some(b'-' | b'0'..=b'9') => self.parse_number(),
            Some(b':') => {
                self.parse_byte_sequence()?;
                Ok(ParameterValue::Other)
            }
            Some(b'@') => {
                self.consume();
                self.parse_number()?;
                Ok(ParameterValue::Other)
            }
            Some(b'%') => {
                self.parse_display_string()?;
                Ok(ParameterValue::Other)
            }
            Some(byte) if byte.is_ascii_alphabetic() || byte == b'*' => {
                self.parse_token();
                Ok(ParameterValue::Other)
            }
            _ => Err(MessageSignatureError::MalformedSignatureHeader(
                SIGNATURE_INPUT,
            )),
        }
    }

    fn parse_string(&mut self) -> Result<String, MessageSignatureError> {
        if !self.consume_if(b'"') {
            return Err(MessageSignatureError::MalformedSignatureHeader(
                SIGNATURE_INPUT,
            ));
        }
        let mut out = String::new();
        loop {
            let Some(byte) = self.consume() else {
                return Err(MessageSignatureError::MalformedSignatureHeader(
                    SIGNATURE_INPUT,
                ));
            };
            match byte {
                b'\\' => {
                    let Some(next) = self.consume() else {
                        return Err(MessageSignatureError::MalformedSignatureHeader(
                            SIGNATURE_INPUT,
                        ));
                    };
                    if !matches!(next, b'"' | b'\\') {
                        return Err(MessageSignatureError::MalformedSignatureHeader(
                            SIGNATURE_INPUT,
                        ));
                    }
                    out.push(next as char);
                }
                b'"' => return Ok(out),
                0x00..=0x1f | 0x7f => {
                    return Err(MessageSignatureError::MalformedSignatureHeader(
                        SIGNATURE_INPUT,
                    ));
                }
                _ => out.push(byte as char),
            }
        }
    }

    fn parse_key(&mut self) -> Result<String, MessageSignatureError> {
        let start = self.pos;
        match self.peek() {
            Some(byte) if byte.is_ascii_lowercase() || byte == b'*' => {
                self.consume();
            }
            _ => {
                return Err(MessageSignatureError::MalformedSignatureHeader(
                    SIGNATURE_INPUT,
                ));
            }
        }
        while self.peek().is_some_and(is_key_char) {
            self.consume();
        }
        Ok(as_ascii_string(&self.input[start..self.pos]))
    }

    fn parse_boolean(&mut self) -> Result<bool, MessageSignatureError> {
        if !self.consume_if(b'?') {
            return Err(MessageSignatureError::MalformedSignatureHeader(
                SIGNATURE_INPUT,
            ));
        }
        match self.consume() {
            Some(b'1') => Ok(true),
            Some(b'0') => Ok(false),
            _ => Err(MessageSignatureError::MalformedSignatureHeader(
                SIGNATURE_INPUT,
            )),
        }
    }

    fn parse_number(&mut self) -> Result<ParameterValue, MessageSignatureError> {
        let negative = self.consume_if(b'-');
        let start = self.pos;
        if !self.peek().is_some_and(|byte| byte.is_ascii_digit()) {
            return Err(MessageSignatureError::MalformedSignatureHeader(
                SIGNATURE_INPUT,
            ));
        }
        while self.peek().is_some_and(|byte| byte.is_ascii_digit()) {
            self.consume();
        }
        let integer = as_ascii_string(&self.input[start..self.pos]);
        if self.consume_if(b'.') {
            if !self.peek().is_some_and(|byte| byte.is_ascii_digit()) {
                return Err(MessageSignatureError::MalformedSignatureHeader(
                    SIGNATURE_INPUT,
                ));
            }
            while self.peek().is_some_and(|byte| byte.is_ascii_digit()) {
                self.consume();
            }
            return Ok(ParameterValue::Other);
        }
        let value = integer
            .parse::<i64>()
            .map_err(|_| MessageSignatureError::MalformedSignatureHeader(SIGNATURE_INPUT))?;
        Ok(ParameterValue::Integer(if negative {
            -value
        } else {
            value
        }))
    }

    fn parse_byte_sequence(&mut self) -> Result<(), MessageSignatureError> {
        if !self.consume_if(b':') {
            return Err(MessageSignatureError::MalformedSignatureHeader(
                SIGNATURE_INPUT,
            ));
        }
        while !matches!(self.peek(), Some(b':') | None) {
            self.consume();
        }
        if self.consume_if(b':') {
            Ok(())
        } else {
            Err(MessageSignatureError::MalformedSignatureHeader(
                SIGNATURE_INPUT,
            ))
        }
    }

    fn parse_display_string(&mut self) -> Result<(), MessageSignatureError> {
        if !self.consume_if(b'%') || !self.consume_if(b'"') {
            return Err(MessageSignatureError::MalformedSignatureHeader(
                SIGNATURE_INPUT,
            ));
        }
        loop {
            let Some(byte) = self.consume() else {
                return Err(MessageSignatureError::MalformedSignatureHeader(
                    SIGNATURE_INPUT,
                ));
            };
            if byte == b'"' {
                return Ok(());
            }
            if byte == b'%'
                && (self.consume().and_then(lowercase_hex_value).is_none()
                    || self.consume().and_then(lowercase_hex_value).is_none())
            {
                return Err(MessageSignatureError::MalformedSignatureHeader(
                    SIGNATURE_INPUT,
                ));
            }
        }
    }

    fn parse_token(&mut self) {
        self.consume();
        while self
            .peek()
            .is_some_and(|byte| is_tchar(byte) || matches!(byte, b':' | b'/'))
        {
            self.consume();
        }
    }

    fn ensure_empty(&self) -> Result<(), MessageSignatureError> {
        if self.pos == self.input.len() {
            Ok(())
        } else {
            Err(MessageSignatureError::MalformedSignatureHeader(
                SIGNATURE_INPUT,
            ))
        }
    }

    fn consume_if(&mut self, byte: u8) -> bool {
        if self.peek() == Some(byte) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn consume(&mut self) -> Option<u8> {
        let byte = self.peek()?;
        self.pos += 1;
        Some(byte)
    }

    fn peek(&self) -> Option<u8> {
        self.input.get(self.pos).copied()
    }
}

fn component_from_name(name: &str) -> Result<MessageSignatureComponent, MessageSignatureError> {
    match name {
        "@method" => Ok(MessageSignatureComponent::method()),
        "@scheme" => Ok(MessageSignatureComponent::scheme()),
        "@authority" => Ok(MessageSignatureComponent::authority()),
        "@request-target" => Ok(MessageSignatureComponent::request_target()),
        "@target-uri" => Ok(MessageSignatureComponent::target_uri()),
        "@path" => Ok(MessageSignatureComponent::path()),
        "@query" => Ok(MessageSignatureComponent::query()),
        "@query-param" => Ok(MessageSignatureComponent::parsed_query_param()),
        "@signature-params" => Err(MessageSignatureError::UnsupportedComponent(
            "\"@signature-params\"".to_owned(),
        )),
        _ if name.starts_with('@') => Err(MessageSignatureError::UnsupportedComponent(format!(
            "\"{name}\""
        ))),
        _ => {
            if name != name.to_ascii_lowercase() {
                return Err(MessageSignatureError::MalformedSignatureHeader(
                    SIGNATURE_INPUT,
                ));
            }
            let name = HeaderName::from_bytes(name.as_bytes())
                .map_err(|_| MessageSignatureError::MalformedSignatureHeader(SIGNATURE_INPUT))?;
            Ok(MessageSignatureComponent::header(name))
        }
    }
}

fn component_parameter(
    key: &str,
    value: Option<ParameterValue>,
) -> Result<MessageSignatureComponentParameter, MessageSignatureError> {
    match key {
        "sf" => {
            require_true_or_absent(value)?;
            Ok(MessageSignatureComponentParameter::StructuredField)
        }
        "key" => Ok(MessageSignatureComponentParameter::Key(
            required_string_parameter(value)?,
        )),
        "bs" => {
            require_true_or_absent(value)?;
            Ok(MessageSignatureComponentParameter::ByteSequence)
        }
        "tr" => {
            require_true_or_absent(value)?;
            Ok(MessageSignatureComponentParameter::Trailer)
        }
        "req" => {
            require_true_or_absent(value)?;
            Ok(MessageSignatureComponentParameter::RelatedRequest)
        }
        "name" => Ok(MessageSignatureComponentParameter::Name(
            required_string_parameter(value)?,
        )),
        _ => Err(MessageSignatureError::MalformedSignatureHeader(
            SIGNATURE_INPUT,
        )),
    }
}

fn require_true_or_absent(value: Option<ParameterValue>) -> Result<(), MessageSignatureError> {
    match value {
        None | Some(ParameterValue::Boolean(true)) => Ok(()),
        _ => Err(MessageSignatureError::MalformedSignatureHeader(
            SIGNATURE_INPUT,
        )),
    }
}

fn required_string_parameter(
    value: Option<ParameterValue>,
) -> Result<String, MessageSignatureError> {
    match value {
        Some(ParameterValue::String(value)) => Ok(value),
        _ => Err(MessageSignatureError::MalformedSignatureHeader(
            SIGNATURE_INPUT,
        )),
    }
}

fn required_u64_parameter(value: Option<ParameterValue>) -> Result<u64, MessageSignatureError> {
    match value {
        Some(ParameterValue::Integer(value)) if value >= 0 => Ok(value as u64),
        _ => Err(MessageSignatureError::MalformedSignatureHeader(
            SIGNATURE_INPUT,
        )),
    }
}

fn as_ascii_string(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| *byte as char).collect()
}

fn is_key_char(byte: u8) -> bool {
    byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'-' | b'.' | b'*')
}

fn is_tchar(byte: u8) -> bool {
    byte.is_ascii_alphanumeric()
        || matches!(
            byte,
            b'!' | b'#'
                | b'$'
                | b'%'
                | b'&'
                | b'\''
                | b'*'
                | b'+'
                | b'-'
                | b'.'
                | b'^'
                | b'_'
                | b'`'
                | b'|'
                | b'~'
        )
}

fn lowercase_hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    }
}
