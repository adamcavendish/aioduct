use std::net::IpAddr;

use http::header::{HOST, HeaderMap};
use http::uri::{Authority, Scheme};

use crate::error::Error;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum InboundRequestTarget {
    Origin {
        authority: Option<Authority>,
    },
    Absolute {
        authority: Authority,
        scheme: Scheme,
        server_wide_options: bool,
    },
    Authority(Authority),
    Asterisk {
        authority: Option<Authority>,
        authority_in_target: bool,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum HostField {
    Missing,
    Empty,
    Authority(Authority),
}

impl HostField {
    fn authority(&self) -> Option<&Authority> {
        match self {
            Self::Authority(authority) => Some(authority),
            Self::Missing | Self::Empty => None,
        }
    }
}

impl InboundRequestTarget {
    pub(crate) fn capture(parts: &http::request::Parts) -> Result<Self, Error> {
        let uri = &parts.uri;
        let is_h1 = matches!(
            parts.version,
            http::Version::HTTP_10 | http::Version::HTTP_11
        );
        let host = single_host_field(&parts.headers)?;
        if parts.version == http::Version::HTTP_11 && host == HostField::Missing {
            return Err(Error::InvalidHeader(
                "HTTP/1.1 forwarding requires a Host field".to_owned(),
            ));
        }
        if uri.path() == "*" {
            if uri.path_and_query().map(|value| value.as_str()) != Some("*") {
                return Err(Error::InvalidUrl(
                    "the asterisk request target must not include a query".to_owned(),
                ));
            }
            if parts.method != http::Method::OPTIONS {
                return Err(Error::InvalidUrl(
                    "the asterisk request target is only valid for OPTIONS".to_owned(),
                ));
            }
            match (is_h1, uri.scheme(), uri.authority()) {
                (true, None, None) => {}
                (true, _, _) | (false, None, Some(_)) => {
                    return Err(Error::InvalidUrl(
                        "invalid asterisk-form request target control data".to_owned(),
                    ));
                }
                (false, _, _) => {}
            }
            if let Some(authority) = uri.authority() {
                validate_authority(authority)?;
                validate_uri_host_match(
                    parts.version,
                    authority,
                    &host,
                    uri.scheme(),
                    "inbound OPTIONS authority and Host field disagree",
                )?;
            }
            return Ok(Self::Asterisk {
                authority: uri
                    .authority()
                    .cloned()
                    .or_else(|| host.authority().cloned()),
                authority_in_target: uri.authority().is_some(),
            });
        }

        if uri.scheme().is_none()
            && let Some(authority) = uri.authority()
        {
            if uri.path_and_query().is_some() {
                return Err(Error::InvalidUrl(
                    "authority-form request targets must not include a path or query".to_owned(),
                ));
            }
            if parts.method != http::Method::CONNECT {
                return Err(Error::InvalidUrl(
                    "authority-form request targets are valid only for CONNECT".to_owned(),
                ));
            }
            validate_connect_authority(authority)?;
            match parts.version {
                http::Version::HTTP_10 => {}
                http::Version::HTTP_11 => match &host {
                    HostField::Authority(host)
                        if h1_connect_authorities_match(authority, host)? => {}
                    HostField::Authority(_) | HostField::Empty => {
                        return Err(Error::InvalidHeader(
                            "inbound CONNECT authority and Host field disagree".to_owned(),
                        ));
                    }
                    HostField::Missing => unreachable!("HTTP/1.1 Host was checked above"),
                },
                http::Version::HTTP_2 | http::Version::HTTP_3 => {
                    validate_uri_host_match(
                        parts.version,
                        authority,
                        &host,
                        None,
                        "inbound CONNECT authority and Host field disagree",
                    )?;
                }
                _ => {}
            }
            return Ok(Self::Authority(authority.clone()));
        }

        if let (Some(scheme), Some(authority)) = (uri.scheme(), uri.authority()) {
            validate_authority(authority)?;
            if !uri.path().is_empty() && !uri.path().starts_with('/') {
                return Err(Error::InvalidUrl(
                    "absolute request targets must use an absolute path".to_owned(),
                ));
            }
            if !is_h1 {
                validate_uri_host_match(
                    parts.version,
                    authority,
                    &host,
                    Some(scheme),
                    "inbound URI authority and Host field disagree",
                )?;
            }
            return Ok(Self::Absolute {
                authority: authority.clone(),
                scheme: scheme.clone(),
                server_wide_options: parts.method == http::Method::OPTIONS
                    && uri.path_and_query().is_none(),
            });
        }

        if uri.scheme().is_some() {
            return Err(Error::InvalidUrl(
                "absolute request targets require an authority".to_owned(),
            ));
        }

        if !uri.path().starts_with('/') {
            return Err(Error::InvalidUrl(format!(
                "invalid origin-form request target `{uri}`"
            )));
        }

        Ok(Self::Origin {
            authority: host.authority().cloned(),
        })
    }

    pub(crate) fn preserved_authority(&self) -> Option<&Authority> {
        match self {
            Self::Origin { authority } | Self::Asterisk { authority, .. } => authority.as_ref(),
            Self::Absolute { authority, .. } | Self::Authority(authority) => Some(authority),
        }
    }

    pub(crate) fn connect_authority(&self) -> Option<&Authority> {
        match self {
            Self::Authority(authority) => Some(authority),
            _ => None,
        }
    }

    pub(crate) fn server_wide_options_authority_in_target(&self) -> Option<bool> {
        match self {
            Self::Asterisk {
                authority_in_target,
                ..
            } => Some(*authority_in_target),
            Self::Absolute {
                server_wide_options: true,
                ..
            } => Some(true),
            Self::Origin { .. }
            | Self::Absolute {
                server_wide_options: false,
                ..
            }
            | Self::Authority(_) => None,
        }
    }
}

pub(crate) fn single_host_field(headers: &HeaderMap) -> Result<HostField, Error> {
    let mut hosts = headers.get_all(HOST).iter();
    let Some(host) = hosts.next() else {
        return Ok(HostField::Missing);
    };
    if hosts.next().is_some() {
        return Err(Error::InvalidHeader(
            "forwarding requires at most one Host field".to_owned(),
        ));
    }
    let host = host
        .to_str()
        .map_err(|error| Error::InvalidHeader(format!("invalid forwarded Host field: {error}")))?;
    if host.is_empty() {
        return Ok(HostField::Empty);
    }
    let authority: Authority = host
        .parse()
        .map_err(|error| Error::InvalidHeader(format!("invalid forwarded Host field: {error}")))?;
    validate_authority(&authority)?;
    Ok(HostField::Authority(authority))
}

pub(crate) fn validate_authority(authority: &Authority) -> Result<Option<u16>, Error> {
    if authority.as_str().contains('@') {
        return Err(Error::InvalidHeader(
            "forwarded authority must not contain userinfo".to_owned(),
        ));
    }
    explicit_authority_port(authority)
}

fn validate_uri_host_match(
    version: http::Version,
    authority: &Authority,
    host: &HostField,
    scheme: Option<&Scheme>,
    message: &str,
) -> Result<(), Error> {
    let matches = match host {
        HostField::Missing => true,
        HostField::Empty => false,
        HostField::Authority(host) if version == http::Version::HTTP_3 => {
            authority.as_str() == host.as_str()
        }
        HostField::Authority(host) => authorities_equivalent(authority, host, scheme)?,
    };
    if matches {
        Ok(())
    } else {
        Err(Error::InvalidHeader(message.to_owned()))
    }
}

fn h1_connect_authorities_match(target: &Authority, host: &Authority) -> Result<bool, Error> {
    if normalized_host(target.host()) != normalized_host(host.host()) {
        return Ok(false);
    }
    if host.as_str().ends_with(':') {
        return Ok(false);
    }
    let (Some(target_port), Some(host_port)) = (
        explicit_authority_port(target)?,
        explicit_authority_port(host)?,
    ) else {
        return Ok(false);
    };
    Ok(target_port == host_port)
}

pub(crate) fn validate_connect_authority(authority: &Authority) -> Result<(), Error> {
    if validate_authority(authority)?.is_none() {
        return Err(Error::InvalidUrl(
            "ordinary CONNECT authority-form targets require an explicit port".to_owned(),
        ));
    }
    Ok(())
}

fn authorities_equivalent(
    left: &Authority,
    right: &Authority,
    scheme: Option<&Scheme>,
) -> Result<bool, Error> {
    Ok(
        normalized_host(left.host()) == normalized_host(right.host())
            && effective_port(left, scheme)? == effective_port(right, scheme)?,
    )
}

fn normalized_host(host: &str) -> NormalizedHost {
    let unbracketed = host
        .strip_prefix('[')
        .and_then(|host| host.strip_suffix(']'))
        .unwrap_or(host);
    unbracketed
        .parse::<IpAddr>()
        .map(NormalizedHost::Ip)
        .unwrap_or_else(|_| NormalizedHost::Name(unbracketed.to_ascii_lowercase()))
}

#[derive(Eq, PartialEq)]
enum NormalizedHost {
    Ip(IpAddr),
    Name(String),
}

fn effective_port(authority: &Authority, scheme: Option<&Scheme>) -> Result<Option<u16>, Error> {
    Ok(explicit_authority_port(authority)?.or_else(|| super::scheme::default_port(scheme)))
}

fn explicit_authority_port(authority: &Authority) -> Result<Option<u16>, Error> {
    let endpoint = authority.as_str();
    let raw_port = if let Some(bracketed) = endpoint.strip_prefix('[') {
        let closing = bracketed
            .find(']')
            .ok_or_else(|| invalid_authority_port(endpoint))?;
        if closing == 0 {
            return Err(invalid_authority_port(endpoint));
        }
        match &bracketed[closing + 1..] {
            "" => None,
            suffix if suffix.starts_with(':') => Some(&suffix[1..]),
            _ => return Err(invalid_authority_port(endpoint)),
        }
    } else {
        if endpoint.is_empty() || endpoint.contains('[') || endpoint.contains(']') {
            return Err(invalid_authority_port(endpoint));
        }
        match endpoint.rsplit_once(':') {
            Some((host, port)) if !host.is_empty() && !host.contains(':') => Some(port),
            Some(_) => return Err(invalid_authority_port(endpoint)),
            None => None,
        }
    };

    let Some(raw_port) = raw_port else {
        return Ok(None);
    };
    if raw_port.is_empty() {
        return Ok(None);
    }
    if !raw_port.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(invalid_authority_port(endpoint));
    }
    let port = raw_port
        .parse::<u16>()
        .map_err(|_| invalid_authority_port(endpoint))?;
    if authority.port_u16() != Some(port) {
        return Err(invalid_authority_port(endpoint));
    }
    Ok(Some(port))
}

fn invalid_authority_port(endpoint: &str) -> Error {
    Error::InvalidHeader(format!(
        "invalid explicit port in forwarded authority `{endpoint}`"
    ))
}

#[cfg(test)]
#[path = "request_target/port_tests.rs"]
mod port_tests;

#[cfg(test)]
mod tests {
    use super::*;

    fn parts(uri: &str, version: http::Version, host: Option<&str>) -> http::request::Parts {
        let mut request = http::Request::builder()
            .uri(uri)
            .version(version)
            .body(())
            .unwrap();
        if let Some(host) = host {
            request
                .headers_mut()
                .insert(HOST, http::HeaderValue::from_str(host).unwrap());
        }
        request.into_parts().0
    }

    #[test]
    fn h1_absolute_form_uses_uri_authority_and_ignores_host() {
        let parts = parts(
            "https://example.test/resource",
            http::Version::HTTP_11,
            Some("different.test"),
        );
        let target = InboundRequestTarget::capture(&parts).unwrap();
        assert_eq!(target.preserved_authority().unwrap(), "example.test");
    }

    #[test]
    fn h2_authorities_compare_with_default_port_and_canonical_ip() {
        let equivalent = parts(
            "https://[0:0:0:0:0:0:0:1]/resource",
            http::Version::HTTP_2,
            Some("[::1]:443"),
        );
        assert!(InboundRequestTarget::capture(&equivalent).is_ok());

        let mismatch = parts(
            "https://example.test/resource",
            http::Version::HTTP_2,
            Some("example.test:444"),
        );
        assert!(InboundRequestTarget::capture(&mismatch).is_err());
    }

    #[test]
    fn h3_authority_and_host_must_match_exactly() {
        for host in ["EXAMPLE.test", "example.test:443"] {
            let parts = parts(
                "https://example.test/resource",
                http::Version::HTTP_3,
                Some(host),
            );
            assert!(
                InboundRequestTarget::capture(&parts).is_err(),
                "accepted non-identical HTTP/3 Host {host}"
            );
        }

        let exact = parts(
            "https://example.test/resource",
            http::Version::HTTP_3,
            Some("example.test"),
        );
        assert!(InboundRequestTarget::capture(&exact).is_ok());
    }

    #[test]
    fn h2_authorities_compare_mixed_case_schemes_with_default_ports() {
        for (scheme, host) in [("HTTP", "example.test:80"), ("HtTpS", "example.test:443")] {
            let mut parts = parts("/resource", http::Version::HTTP_2, Some(host));
            let mut uri_parts = parts.uri.into_parts();
            uri_parts.scheme = Some(scheme.parse().unwrap());
            uri_parts.authority = Some("example.test".parse().unwrap());
            parts.uri = http::Uri::from_parts(uri_parts).unwrap();
            assert!(InboundRequestTarget::capture(&parts).is_ok(), "{scheme}");
        }
    }

    #[test]
    fn h11_authority_form_connect_requires_an_equivalent_host() {
        for (authority, host) in [
            ("target.example:443", "TARGET.EXAMPLE:443"),
            ("[0:0:0:0:0:0:0:1]:8443", "[::1]:8443"),
        ] {
            let mut parts = parts(authority, http::Version::HTTP_11, Some(host));
            parts.method = http::Method::CONNECT;

            let target = InboundRequestTarget::capture(&parts).unwrap();

            assert_eq!(target.connect_authority().unwrap(), authority);
        }
    }

    #[test]
    fn h11_authority_form_connect_rejects_mismatched_host_or_explicit_port() {
        for (authority, host) in [
            ("target.example:443", "different.example:443"),
            ("target.example:443", "target.example"),
            ("target.example:443", "target.example:"),
            ("target.example:443", "target.example:444"),
            ("[::1]:443", "[::2]:443"),
            ("[::1]:443", "[::1]"),
            ("[::1]:443", "[::1]:"),
            ("[::1]:443", "[::1]:444"),
        ] {
            let mut parts = parts(authority, http::Version::HTTP_11, Some(host));
            parts.method = http::Method::CONNECT;

            let error = InboundRequestTarget::capture(&parts).unwrap_err();

            assert!(
                matches!(error, Error::InvalidHeader(ref message) if message.contains("CONNECT authority and Host field disagree")),
                "unexpected error for authority {authority} and Host {host}: {error}"
            );
        }
    }

    #[test]
    fn h11_authority_form_connect_rejects_a_missing_host() {
        let mut parts = parts("target.example:443", http::Version::HTTP_11, None);
        parts.method = http::Method::CONNECT;

        let error = InboundRequestTarget::capture(&parts).unwrap_err();

        assert!(
            matches!(error, Error::InvalidHeader(ref message) if message.contains("HTTP/1.1 forwarding requires a Host field")),
            "unexpected missing Host error: {error}"
        );
    }

    #[test]
    fn h10_authority_form_connect_keeps_host_optional_and_non_authoritative() {
        for host in [None, Some("different.example:80")] {
            let mut parts = parts("target.example:443", http::Version::HTTP_10, host);
            parts.method = http::Method::CONNECT;

            let target = InboundRequestTarget::capture(&parts).unwrap();

            assert_eq!(target.connect_authority().unwrap(), "target.example:443");
        }
    }

    #[test]
    fn h2_asterisk_form_is_recognized_with_pseudo_header_uri_components() {
        let mut parts = parts("/", http::Version::HTTP_2, Some("downstream.test"));
        parts.method = http::Method::OPTIONS;
        parts.uri = http::Uri::builder()
            .scheme("https")
            .authority("downstream.test")
            .path_and_query("*")
            .build()
            .unwrap();

        let target = InboundRequestTarget::capture(&parts).unwrap();

        assert!(matches!(target, InboundRequestTarget::Asterisk { .. }));
        assert_eq!(target.server_wide_options_authority_in_target(), Some(true));
        assert_eq!(target.preserved_authority().unwrap(), "downstream.test");

        parts
            .headers
            .insert(HOST, http::HeaderValue::from_static("different.test"));
        assert!(InboundRequestTarget::capture(&parts).is_err());
    }

    #[test]
    fn asterisk_form_is_rejected_for_non_options_methods() {
        let parts = parts("*", http::Version::HTTP_11, Some("downstream.test"));
        assert!(InboundRequestTarget::capture(&parts).is_err());
    }

    #[test]
    fn h1_asterisk_form_rejects_pseudo_header_components() {
        let mut h1_with_control_data = parts("/", http::Version::HTTP_11, Some("downstream.test"));
        h1_with_control_data.method = http::Method::OPTIONS;
        h1_with_control_data.uri = http::Uri::builder()
            .scheme("https")
            .authority("downstream.test")
            .path_and_query("*")
            .build()
            .unwrap();
        assert!(InboundRequestTarget::capture(&h1_with_control_data).is_err());
    }

    #[test]
    fn server_wide_options_metadata_distinguishes_absolute_and_asterisk_forms() {
        let target = InboundRequestTarget::Absolute {
            authority: "downstream.test".parse().unwrap(),
            scheme: Scheme::HTTPS,
            server_wide_options: true,
        };
        assert_eq!(target.server_wide_options_authority_in_target(), Some(true));

        let target = InboundRequestTarget::Asterisk {
            authority: Some("downstream.test".parse().unwrap()),
            authority_in_target: false,
        };
        assert_eq!(
            target.server_wide_options_authority_in_target(),
            Some(false)
        );
    }

    #[test]
    fn non_connect_authority_form_is_rejected() {
        let parts = parts("relative", http::Version::HTTP_10, None);
        assert!(InboundRequestTarget::capture(&parts).is_err());
    }

    #[test]
    fn present_empty_host_is_valid_when_the_target_has_no_authority() {
        for version in [http::Version::HTTP_10, http::Version::HTTP_11] {
            let parts = parts("/resource", version, Some(""));
            assert!(
                InboundRequestTarget::capture(&parts).is_ok(),
                "{version:?} rejected a present empty Host"
            );
        }

        let absolute = parts(
            "https://downstream.test/resource",
            http::Version::HTTP_11,
            Some(""),
        );
        assert!(InboundRequestTarget::capture(&absolute).is_ok());

        let h2 = parts(
            "https://downstream.test/resource",
            http::Version::HTTP_2,
            Some(""),
        );
        assert!(InboundRequestTarget::capture(&h2).is_err());
    }

    #[test]
    fn h2_authority_form_connect_validates_host_without_default_ports() {
        for host in [Some("target.example:443"), None] {
            let mut parts = parts("target.example:443", http::Version::HTTP_2, host);
            parts.method = http::Method::CONNECT;
            assert!(InboundRequestTarget::capture(&parts).is_ok());
        }

        for host in [
            "different.example:443",
            "target.example",
            "target.example:444",
        ] {
            let mut parts = parts("target.example:443", http::Version::HTTP_2, Some(host));
            parts.method = http::Method::CONNECT;
            assert!(
                InboundRequestTarget::capture(&parts).is_err(),
                "accepted mismatched Host {host}"
            );
        }
    }
}
