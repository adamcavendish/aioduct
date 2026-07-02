use base64::Engine as _;
use bytes::Bytes;
use http::header::{HeaderMap, HeaderName, HeaderValue};

use crate::message_signatures::MessageSignatureError;
use crate::message_signatures::structured_fields;

pub(crate) const CONTENT_DIGEST: &str = "content-digest";

#[derive(Clone, Debug)]
pub(crate) enum ContentDigestBody {
    None,
    Buffered(Bytes),
    Unavailable,
}

pub(crate) fn has_content_digest(headers: &HeaderMap) -> bool {
    headers.contains_key(HeaderName::from_static(CONTENT_DIGEST))
}

pub(crate) fn sha256_content_digest_value(
    body: &[u8],
) -> Result<HeaderValue, http::header::InvalidHeaderValue> {
    let digest = crate::sha256::compute(body);
    let digest = base64::engine::general_purpose::STANDARD.encode(digest);
    HeaderValue::from_str(&format!("sha-256=:{digest}:"))
}

pub(crate) fn insert_sha256_content_digest(
    headers: &mut HeaderMap,
    body: &[u8],
) -> Result<(), http::header::InvalidHeaderValue> {
    headers.insert(
        HeaderName::from_static(CONTENT_DIGEST),
        sha256_content_digest_value(body)?,
    );
    Ok(())
}

pub(crate) fn verify_sha256_content_digest(
    headers: &HeaderMap,
    body: &[u8],
) -> Result<(), MessageSignatureError> {
    let value = combined_content_digest(headers)?.ok_or_else(|| {
        MessageSignatureError::MissingHeader(HeaderName::from_static(CONTENT_DIGEST))
    })?;
    let entries = structured_fields::dictionary(&value)
        .map_err(|_| MessageSignatureError::MalformedContentDigest)?;
    let Some((_, member)) = entries
        .iter()
        .rev()
        .find(|(algorithm, _)| algorithm == "sha-256")
    else {
        return Err(MessageSignatureError::UnsupportedContentDigestAlgorithm);
    };
    let digest = byte_sequence_member(member)?;
    if digest == crate::sha256::compute(body) {
        Ok(())
    } else {
        Err(MessageSignatureError::ContentDigestMismatch)
    }
}

fn combined_content_digest(headers: &HeaderMap) -> Result<Option<String>, MessageSignatureError> {
    let values = headers.get_all(HeaderName::from_static(CONTENT_DIGEST));
    let mut out = Vec::new();
    for value in values {
        let value = value
            .to_str()
            .map_err(|_| MessageSignatureError::MalformedContentDigest)?;
        out.push(value.trim_matches(|c| c == ' ' || c == '\t').to_owned());
    }
    if out.is_empty() {
        Ok(None)
    } else {
        Ok(Some(out.join(", ")))
    }
}

fn byte_sequence_member(member: &str) -> Result<Vec<u8>, MessageSignatureError> {
    let Some(rest) = member.strip_prefix(':') else {
        return Err(MessageSignatureError::MalformedContentDigest);
    };
    let Some(end) = rest.find(':') else {
        return Err(MessageSignatureError::MalformedContentDigest);
    };
    let encoded = &rest[..end];
    let parameters = &rest[end + 1..];
    if !parameters.is_empty() && !parameters.starts_with(';') {
        return Err(MessageSignatureError::MalformedContentDigest);
    }
    base64::engine::general_purpose::STANDARD
        .decode(encoded.as_bytes())
        .map_err(|_| MessageSignatureError::MalformedContentDigest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_sha256_content_digest() {
        let value = sha256_content_digest_value(b"hello").unwrap();
        assert_eq!(
            value,
            HeaderValue::from_static("sha-256=:LPJNul+wow4m6DsqxbninhsWHlwfp0JecwQzYpOLmCQ=:")
        );
    }

    #[test]
    fn verifies_sha256_content_digest() {
        let mut headers = HeaderMap::new();
        headers.insert(
            HeaderName::from_static(CONTENT_DIGEST),
            HeaderValue::from_static("sha-256=:LPJNul+wow4m6DsqxbninhsWHlwfp0JecwQzYpOLmCQ=:"),
        );

        verify_sha256_content_digest(&headers, b"hello").unwrap();
    }

    #[test]
    fn rejects_mismatched_sha256_content_digest() {
        let mut headers = HeaderMap::new();
        headers.insert(
            HeaderName::from_static(CONTENT_DIGEST),
            HeaderValue::from_static("sha-256=:LPJNul+wow4m6DsqxbninhsWHlwfp0JecwQzYpOLmCQ=:"),
        );

        assert!(matches!(
            verify_sha256_content_digest(&headers, b"goodbye"),
            Err(MessageSignatureError::ContentDigestMismatch)
        ));
    }
}
