use super::MessageSignatureError;

const MAX_STRUCTURED_FIELDS_INTEGER: u64 = 999_999_999_999_999;

/// Signature metadata parameters serialized into `Signature-Input`.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub struct MessageSignatureParams {
    pub(crate) created: Option<u64>,
    pub(crate) expires: Option<u64>,
    pub(crate) nonce: Option<String>,
    pub(crate) algorithm: Option<String>,
    pub(crate) key_id: Option<String>,
    pub(crate) tag: Option<String>,
}

impl MessageSignatureParams {
    pub(crate) fn serialize(&self) -> Result<String, MessageSignatureError> {
        let mut out = String::new();
        if let Some(created) = self.created {
            out.push_str(";created=");
            out.push_str(&serialize_sf_integer("created", created)?);
        }
        if let Some(expires) = self.expires {
            out.push_str(";expires=");
            out.push_str(&serialize_sf_integer("expires", expires)?);
        }
        if let Some(ref nonce) = self.nonce {
            out.push_str(";nonce=");
            out.push_str(&serialize_sf_string(nonce)?);
        }
        if let Some(ref algorithm) = self.algorithm {
            out.push_str(";alg=");
            out.push_str(&serialize_sf_string(algorithm)?);
        }
        if let Some(ref key_id) = self.key_id {
            out.push_str(";keyid=");
            out.push_str(&serialize_sf_string(key_id)?);
        }
        if let Some(ref tag) = self.tag {
            out.push_str(";tag=");
            out.push_str(&serialize_sf_string(tag)?);
        }
        Ok(out)
    }
}

fn serialize_sf_integer(
    parameter: &'static str,
    value: u64,
) -> Result<String, MessageSignatureError> {
    if value > MAX_STRUCTURED_FIELDS_INTEGER {
        return Err(MessageSignatureError::InvalidIntegerParameter { parameter, value });
    }
    Ok(value.to_string())
}

pub(crate) fn serialize_sf_string(value: &str) -> Result<String, MessageSignatureError> {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for c in value.chars() {
        if !c.is_ascii() || c.is_ascii_control() {
            return Err(MessageSignatureError::InvalidStringParameter(
                value.to_owned(),
            ));
        }
        if c == '"' || c == '\\' {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
    Ok(out)
}
