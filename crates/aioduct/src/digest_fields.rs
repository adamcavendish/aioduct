use base64::Engine as _;
use bytes::Bytes;
use http::header::{HeaderMap, HeaderName, HeaderValue};

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
}
