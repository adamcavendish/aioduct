use http::header::{HOST, HeaderMap, HeaderName, HeaderValue};
use http::uri::{Parts as UriParts, PathAndQuery, Scheme, Uri};

use crate::error::Error;
use crate::pool::ProtocolHint;

use super::hop_by_hop;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ForwardMode {
    Normal,
    H1Upgrade,
    ExtendedConnect,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum EgressProtocol {
    Negotiated,
    Http1,
    Http2,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RequestTargetForm {
    Origin,
    Absolute,
    Authority,
    Asterisk,
    AsteriskAuthorityOmitted,
    AsteriskAbsolute,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ServerWideOptions {
    authority_in_target: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ForwardAsteriskAuthority {
    Include,
    Omit,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MethodSemanticClass {
    Ordinary,
    Head,
    Connect,
}

pub(crate) fn capture_downstream_connect_protocol(
    parts: &mut http::request::Parts,
) -> Result<Option<String>, Error> {
    let protocol = normalize_connect_protocol(&mut parts.extensions)?;
    if protocol.is_some() && parts.method != http::Method::CONNECT {
        return Err(Error::Unsupported(
            "extended CONNECT protocol metadata requires the CONNECT method".to_owned(),
        ));
    }
    Ok(protocol)
}

fn normalize_connect_protocol(extensions: &mut http::Extensions) -> Result<Option<String>, Error> {
    #[cfg(feature = "http3")]
    if extensions.get::<h3::ext::Protocol>().is_some() {
        // TODO(http3-extended-connect): enable this only after aioduct can
        // validate peer settings and supervise the tunnel lifecycle end to end
        // while continuing to use upstream h3.
        return Err(Error::Unsupported(
            "HTTP/3 extended CONNECT forwarding is not supported by aioduct".to_owned(),
        ));
    }

    let protocol = extensions
        .get::<crate::Protocol>()
        .map(crate::Protocol::as_str);
    if let Some(protocol) = protocol
        && !hop_by_hop::valid_token(protocol.as_bytes())
    {
        return Err(Error::InvalidHeader(
            "extended CONNECT protocol metadata must be a non-empty HTTP token".to_owned(),
        ));
    }
    Ok(protocol.map(str::to_owned))
}

fn validate_connect_protocol_rewrite(
    downstream: Option<&str>,
    forwarded: Option<&str>,
) -> Result<(), Error> {
    if downstream == forwarded {
        return Ok(());
    }
    let message = match (downstream, forwarded) {
        (None, Some(_)) => "a forward hook cannot create extended CONNECT protocol metadata",
        (Some(_), None) => "a forward hook cannot remove extended CONNECT protocol metadata",
        (Some(_), Some(_)) => "a forward hook cannot change extended CONNECT protocol metadata",
        (None, None) => return Ok(()),
    };
    Err(Error::Unsupported(message.to_owned()))
}

/// Protocol and request-target decisions consumed by a single forward dispatch.
///
/// The request body is deliberately not part of this value. It remains owned by
/// the Send or Local executor and crosses this policy boundary exactly once.
pub(crate) struct ForwardDispatchPlan {
    full_uri: Uri,
    request_uri: Uri,
    mode: ForwardMode,
    egress: EgressProtocol,
    protocol_hint: ProtocolHint,
}

impl ForwardDispatchPlan {
    pub(crate) fn from_legacy_classification(
        full_uri: Uri,
        method: &http::Method,
        protocol_hint: ProtocolHint,
        is_h1_upgrade: bool,
        is_h2_extended_connect: bool,
    ) -> Result<Self, Error> {
        let mode = if is_h2_extended_connect {
            ForwardMode::ExtendedConnect
        } else if is_h1_upgrade {
            ForwardMode::H1Upgrade
        } else {
            ForwardMode::Normal
        };
        let egress = if is_h2_extended_connect || protocol_hint == ProtocolHint::H2c {
            EgressProtocol::Http2
        } else if is_h1_upgrade {
            EgressProtocol::Http1
        } else {
            EgressProtocol::Negotiated
        };
        let target_form = if is_h2_extended_connect {
            RequestTargetForm::Absolute
        } else if protocol_hint == ProtocolHint::H2c && *method == http::Method::CONNECT {
            RequestTargetForm::Authority
        } else if protocol_hint == ProtocolHint::H2c {
            RequestTargetForm::Absolute
        } else {
            RequestTargetForm::Origin
        };
        let request_uri = request_uri(&full_uri, target_form)?;

        Ok(Self {
            full_uri,
            request_uri,
            mode,
            egress,
            protocol_hint,
        })
    }

    pub(crate) fn prepare_legacy_version(&self, parts: &mut http::request::Parts) {
        if self.mode == ForwardMode::H1Upgrade {
            parts.version = http::Version::HTTP_11;
        }
        if self.egress == EgressProtocol::Http2 {
            parts.version = http::Version::HTTP_2;
        }
    }

    pub(crate) fn apply_request_target(&self, parts: &mut http::request::Parts) {
        parts.uri = self.request_uri.clone();
    }

    pub(crate) fn full_uri(&self) -> &Uri {
        &self.full_uri
    }

    pub(crate) fn protocol_hint(&self) -> ProtocolHint {
        self.protocol_hint
    }

    pub(crate) fn mode(&self) -> ForwardMode {
        self.mode
    }
}

pub(crate) struct ForwardRewrite<'a> {
    pub(crate) upstream: &'a Uri,
    pub(crate) strip_prefix: Option<&'a str>,
    pub(crate) preserve_host: bool,
    pub(crate) forward_headers: &'a [HeaderName],
    pub(crate) extra_headers: &'a HeaderMap,
    pub(crate) remove_headers: &'a [HeaderName],
}

pub(crate) fn rewrite_for_upstream(
    parts: &mut http::request::Parts,
    rewrite: ForwardRewrite<'_>,
) -> Result<Uri, Error> {
    let forwarded_values: Vec<(HeaderName, HeaderValue)> = rewrite
        .forward_headers
        .iter()
        .filter_map(|name| {
            parts
                .headers
                .get(name)
                .map(|value| (name.clone(), value.clone()))
        })
        .collect();

    hop_by_hop::strip_hop_by_hop(&mut parts.headers);

    let upstream_scheme = rewrite.upstream.scheme().cloned().unwrap_or(Scheme::HTTP);
    let upstream_authority = rewrite
        .upstream
        .authority()
        .cloned()
        .ok_or_else(|| Error::InvalidUrl("forward: upstream has no authority".into()))?;

    let original_path = parts.uri.path();
    let path_after_strip = match rewrite.strip_prefix {
        Some(prefix) => {
            let stripped = original_path.strip_prefix(prefix).unwrap_or(original_path);
            if stripped.is_empty() || !stripped.starts_with('/') {
                format!("/{stripped}")
            } else {
                stripped.to_owned()
            }
        }
        None => original_path.to_owned(),
    };

    let upstream_base = rewrite.upstream.path().trim_end_matches('/');
    let combined_path = if upstream_base.is_empty() {
        path_after_strip
    } else {
        format!("{upstream_base}{path_after_strip}")
    };
    let path_and_query = if let Some(query) = parts.uri.query() {
        format!("{combined_path}?{query}")
    } else {
        combined_path
    };
    let path_and_query: PathAndQuery = path_and_query
        .parse()
        .map_err(|error| Error::InvalidUrl(format!("forward: invalid path: {error}")))?;

    let mut uri_parts = UriParts::default();
    uri_parts.scheme = Some(upstream_scheme);
    uri_parts.authority = Some(upstream_authority.clone());
    uri_parts.path_and_query = Some(path_and_query);
    let full_uri = Uri::from_parts(uri_parts)
        .map_err(|error| Error::InvalidUrl(format!("forward: {error}")))?;

    if !rewrite.preserve_host {
        parts.headers.remove(HOST);
        if let Ok(host) = upstream_authority.as_str().parse::<HeaderValue>() {
            parts.headers.insert(HOST, host);
        }
    }

    for (name, value) in forwarded_values {
        parts.headers.insert(name, value);
    }
    for (name, value) in rewrite.extra_headers {
        parts.headers.insert(name, value.clone());
    }
    for name in rewrite.remove_headers {
        parts.headers.remove(name);
    }

    Ok(full_uri)
}

fn request_uri(full_uri: &Uri, target_form: RequestTargetForm) -> Result<Uri, Error> {
    match target_form {
        RequestTargetForm::Origin => full_uri
            .path_and_query()
            .map(|path_and_query| path_and_query.as_str())
            .unwrap_or("/")
            .parse()
            .map_err(|error| Error::Other(Box::new(error))),
        RequestTargetForm::Absolute => Ok(full_uri.clone()),
        RequestTargetForm::Authority => full_uri
            .authority()
            .ok_or_else(|| Error::InvalidUrl("forward: upstream has no authority".into()))?
            .as_str()
            .parse()
            .map_err(|error| Error::Other(Box::new(error))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn uri() -> Uri {
        "https://example.test/base?q=1".parse().unwrap()
    }

    #[test]
    fn legacy_plan_models_normal_upgrade_and_extended_connect() {
        let normal = ForwardDispatchPlan::from_legacy_classification(
            uri(),
            &http::Method::GET,
            ProtocolHint::Auto,
            false,
            false,
        )
        .unwrap();
        assert_eq!(normal.mode(), ForwardMode::Normal);
        assert_eq!(normal.egress, EgressProtocol::Negotiated);
        assert_eq!(normal.request_uri, "/base?q=1");

        let upgrade = ForwardDispatchPlan::from_legacy_classification(
            uri(),
            &http::Method::GET,
            ProtocolHint::Auto,
            true,
            false,
        )
        .unwrap();
        assert_eq!(upgrade.mode(), ForwardMode::H1Upgrade);
        assert_eq!(upgrade.egress, EgressProtocol::Http1);

        let extended_connect = ForwardDispatchPlan::from_legacy_classification(
            uri(),
            &http::Method::CONNECT,
            ProtocolHint::H2c,
            false,
            true,
        )
        .unwrap();
        assert_eq!(extended_connect.mode(), ForwardMode::ExtendedConnect);
        assert_eq!(extended_connect.egress, EgressProtocol::Http2);
        assert_eq!(extended_connect.request_uri, uri());
    }

    #[test]
    fn h2c_connect_uses_authority_form() {
        let plan = ForwardDispatchPlan::from_legacy_classification(
            uri(),
            &http::Method::CONNECT,
            ProtocolHint::H2c,
            false,
            false,
        )
        .unwrap();

        assert_eq!(plan.request_uri, "example.test");
    }
}
