use http::Uri;
use http::uri::Scheme;

use crate::error::Error;

pub(crate) fn canonical_http_scheme(scheme: &Scheme) -> Result<Scheme, Error> {
    if scheme.as_str().eq_ignore_ascii_case("http") {
        Ok(Scheme::HTTP)
    } else if scheme.as_str().eq_ignore_ascii_case("https") {
        Ok(Scheme::HTTPS)
    } else {
        Err(Error::Unsupported(format!(
            "forwarding URI scheme `{scheme}` is not supported"
        )))
    }
}

pub(crate) fn canonicalize_http_uri(uri: Uri) -> Result<(Uri, Scheme), Error> {
    let parsed_scheme = uri
        .scheme()
        .ok_or_else(|| Error::InvalidUrl("forward: final URI has no scheme".into()))?;
    let scheme = canonical_http_scheme(parsed_scheme)?;
    if parsed_scheme == &scheme {
        return Ok((uri, scheme));
    }

    let mut parts = uri.into_parts();
    parts.scheme = Some(scheme.clone());
    let uri =
        Uri::from_parts(parts).map_err(|error| Error::InvalidUrl(format!("forward: {error}")))?;
    Ok((uri, scheme))
}

pub(crate) fn default_port(scheme: Option<&Scheme>) -> Option<u16> {
    let scheme = scheme?;
    if scheme.as_str().eq_ignore_ascii_case("http") {
        Some(80)
    } else if scheme.as_str().eq_ignore_ascii_case("https") {
        Some(443)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonicalizes_case_preserving_standard_schemes() {
        for (raw, expected) in [("HTTP", Scheme::HTTP), ("HtTpS", Scheme::HTTPS)] {
            let scheme = raw.parse::<Scheme>().unwrap();
            assert_ne!(scheme, expected, "proof premise changed for {raw}");
            assert_eq!(canonical_http_scheme(&scheme).unwrap(), expected);
        }
    }
}
