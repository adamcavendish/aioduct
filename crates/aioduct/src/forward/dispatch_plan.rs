use http::header::{HOST, HeaderMap, HeaderName, HeaderValue};
use http::uri::{Parts as UriParts, PathAndQuery, Scheme, Uri};

use crate::error::Error;
use crate::pool::ProtocolHint;

use super::hop_by_hop::{HeaderProtocol, ResponseHeaderPolicy};
use super::request_target::{
    HostField, InboundRequestTarget, single_host_field, validate_authority,
    validate_connect_authority,
};
use super::trailer_policy::TrailerPolicy;
use super::{hop_by_hop, is_h1_upgrade_request};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DeferredTe {
    Trailers,
    TrailersForH2OrH3,
    InvalidForH2OrH3,
}

impl DeferredTe {
    pub(crate) fn project_for_signature(self, headers: &mut HeaderMap) {
        if matches!(self, Self::Trailers | Self::TrailersForH2OrH3) {
            headers.insert(http::header::TE, HeaderValue::from_static("trailers"));
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct DeferredForwardFraming {
    has_body: bool,
    has_trailer_declaration: bool,
}

#[derive(Clone)]
pub(crate) struct DeferredForwardTrailers(TrailerPolicy);

impl DeferredForwardTrailers {
    pub(crate) fn apply(&self, version: http::Version) {
        self.0.select_for_version(version);
    }
}

impl DeferredForwardFraming {
    pub(crate) fn apply(
        self,
        headers: &mut HeaderMap,
        version: http::Version,
    ) -> Result<(), Error> {
        if !self.has_body {
            headers.remove(http::header::TRAILER);
        }
        if matches!(version, http::Version::HTTP_2 | http::Version::HTTP_3) {
            headers.remove(http::header::TRANSFER_ENCODING);
            return Ok(());
        }
        apply_http11_request_framing(headers, self.has_body, self.has_trailer_declaration)
    }
}

#[derive(Clone)]
pub(crate) struct DeferredForwardTarget {
    h1_uri: Uri,
    h2_h3_uri: Uri,
    h2_uses_h1_translation: bool,
}

impl DeferredForwardTarget {
    pub(crate) fn apply<Body>(&self, request: &mut http::Request<Body>, version: http::Version) {
        match version {
            http::Version::HTTP_2 if self.h2_uses_h1_translation => {
                *request.uri_mut() = self.h1_uri.clone();
                *request.version_mut() = http::Version::HTTP_11;
            }
            http::Version::HTTP_2 | http::Version::HTTP_3 => {
                *request.uri_mut() = self.h2_h3_uri.clone();
                *request.version_mut() = version;
            }
            _ => {
                *request.uri_mut() = self.h1_uri.clone();
                *request.version_mut() = http::Version::HTTP_11;
            }
        }
    }
}

#[derive(Clone)]
pub(crate) struct ForwardSigningTarget {
    target_uri: Uri,
    semantic_request_target: Uri,
}

impl ForwardSigningTarget {
    pub(crate) fn target_uri(&self) -> &Uri {
        &self.target_uri
    }

    pub(crate) fn semantic_request_target(&self) -> &Uri {
        &self.semantic_request_target
    }
}

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
    Http3,
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

/// Final protocol and request-target decisions for one wire dispatch.
///
/// The request body is deliberately not part of this value. It remains owned by
/// the Send or Local executor and crosses this policy boundary exactly once.
pub(crate) struct ForwardDispatchPlan {
    full_uri: Uri,
    request_uri: Uri,
    request_version: http::Version,
    protocol_hint: ProtocolHint,
    deferred_te: Option<DeferredTe>,
    negotiated_framing: bool,
    deferred_target: Option<DeferredForwardTarget>,
    h3_asterisk_authority: Option<ForwardAsteriskAuthority>,
    signing_target: ForwardSigningTarget,
    response_headers: ResponseHeaderPolicy,
    request_trailers: TrailerPolicy,
}

impl ForwardDispatchPlan {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn finalize(
        parts: &mut http::request::Parts,
        rewritten_uri: &Uri,
        inbound_target: &InboundRequestTarget,
        rewritten_trailer_policy: &TrailerPolicy,
        requested_hint: ProtocolHint,
        force_h1_upgrade: bool,
        downstream_h1_upgrade_offer: Option<hop_by_hop::H1UpgradeOffer>,
        version_changed_by_hook: bool,
        allow_h3: bool,
        allow_extended_connect: bool,
        downstream_connect_protocol: Option<&str>,
        downstream_version: http::Version,
        downstream_method: &http::Method,
        downstream_accepts_trailers: bool,
        preserve_host: bool,
    ) -> Result<Self, Error> {
        if parts.version == http::Version::HTTP_09 {
            return Err(Error::Unsupported(
                "forwarding HTTP/0.9 requests is not supported".to_owned(),
            ));
        }

        validate_method_rewrite(downstream_method, &parts.method)?;

        let forwarded_connect_protocol = normalize_connect_protocol(&mut parts.extensions)?;
        if forwarded_connect_protocol.is_some() && parts.method != http::Method::CONNECT {
            return Err(Error::Unsupported(
                "extended CONNECT protocol metadata requires the CONNECT method".to_owned(),
            ));
        }
        validate_connect_protocol_rewrite(
            downstream_connect_protocol,
            forwarded_connect_protocol.as_deref(),
        )?;

        let is_extended_connect =
            forwarded_connect_protocol.is_some() && parts.method == http::Method::CONNECT;
        if is_extended_connect && !allow_extended_connect {
            return Err(Error::Unsupported(
                "extended CONNECT forwarding is unavailable on this runtime".to_owned(),
            ));
        }
        let is_h1_upgrade = force_h1_upgrade || is_h1_upgrade_request(&parts.headers);
        let h1_upgrade_offer = if is_h1_upgrade {
            let upstream_offer = hop_by_hop::h1_upgrade_offer(&parts.headers).ok_or_else(|| {
                Error::InvalidHeader(
                    "HTTP/1.1 upgrade requires a valid downstream-compatible Upgrade offer"
                        .to_owned(),
                )
            })?;
            let downstream_offer = downstream_h1_upgrade_offer.ok_or_else(|| {
                Error::InvalidHeader(
                    "a forward hook cannot create an HTTP/1.1 upgrade that the downstream did not offer"
                        .to_owned(),
                )
            })?;
            let shared_offer = upstream_offer.intersect(&downstream_offer);
            if shared_offer.is_empty() {
                return Err(Error::InvalidHeader(
                    "the forwarded and downstream HTTP/1.1 Upgrade offers have no shared protocol"
                        .to_owned(),
                ));
            }
            shared_offer.constrain_request_headers(&mut parts.headers)?;
            Some(shared_offer)
        } else {
            None
        };
        if is_h1_upgrade && parts.method == http::Method::CONNECT {
            return Err(Error::Unsupported(
                "a forwarded CONNECT request cannot also be an HTTP/1.1 upgrade".to_owned(),
            ));
        }

        let mode = if is_extended_connect {
            ForwardMode::ExtendedConnect
        } else if is_h1_upgrade {
            ForwardMode::H1Upgrade
        } else {
            ForwardMode::Normal
        };
        let downstream_protocol = header_protocol_for_version(downstream_version)?;
        if mode == ForwardMode::H1Upgrade && downstream_version != http::Version::HTTP_11 {
            return Err(Error::Unsupported(
                "HTTP/1.1 upgrades require an HTTP/1.1 downstream request".to_owned(),
            ));
        }

        let ordinary_connect = mode == ForwardMode::Normal && parts.method == http::Method::CONNECT;
        let (full_uri, server_wide_options, connect_authority) = if ordinary_connect {
            let hook_changed_target = parts.uri != *rewritten_uri;
            let authority = if hook_changed_target {
                if parts.uri.scheme().is_none()
                    && parts.uri.path().is_empty()
                    && let Some(authority) = parts.uri.authority()
                {
                    authority.clone()
                } else {
                    return Err(Error::InvalidUrl(
                        "forward: ordinary CONNECT hooks must use an authority-form target"
                            .to_owned(),
                    ));
                }
            } else {
                inbound_target.connect_authority().cloned().ok_or_else(|| {
                    Error::InvalidUrl(
                        "forward: ordinary CONNECT requires an authority-form inbound target"
                            .to_owned(),
                    )
                })?
            };
            validate_connect_authority(&authority)?;
            (rewritten_uri.clone(), None, Some(authority))
        } else {
            let (full_uri, server_wide_options) =
                resolve_hook_uri(rewritten_uri, &parts.uri, &parts.method)?;
            (full_uri, server_wide_options, None)
        };
        let (full_uri, scheme) = super::scheme::canonicalize_http_uri(full_uri)?;
        if requested_hint == ProtocolHint::H2c && scheme == Scheme::HTTPS {
            return Err(Error::Unsupported(
                "h2c forwarding requires a plaintext HTTP upstream".to_owned(),
            ));
        }

        if !ordinary_connect && !preserve_host && full_uri.authority() != rewritten_uri.authority()
        {
            rewrite_host(
                &mut parts.headers,
                full_uri.authority().ok_or_else(|| {
                    Error::InvalidUrl("forward: final URI has no authority".into())
                })?,
            )?;
        }

        let mut egress = egress_for_hint(requested_hint);
        if version_changed_by_hook {
            let version_egress = egress_for_explicit_version(parts.version)?;
            if egress != EgressProtocol::Negotiated && egress != version_egress {
                return Err(Error::Unsupported(format!(
                    "forward hook requested {:?}, which conflicts with {:?} dispatch",
                    parts.version, requested_hint
                )));
            }
            egress = version_egress;
        }

        egress = match mode {
            ForwardMode::H1Upgrade => match egress {
                EgressProtocol::Negotiated | EgressProtocol::Http1 => EgressProtocol::Http1,
                EgressProtocol::Http2 | EgressProtocol::Http3 => {
                    return Err(Error::Unsupported(
                        "HTTP/1.1 upgrade forwarding conflicts with the requested upstream protocol"
                            .to_owned(),
                    ));
                }
            },
            ForwardMode::ExtendedConnect => match egress {
                EgressProtocol::Negotiated | EgressProtocol::Http2 => EgressProtocol::Http2,
                EgressProtocol::Http1 | EgressProtocol::Http3 => {
                    return Err(Error::Unsupported(
                        "extended CONNECT forwarding requires HTTP/2".to_owned(),
                    ));
                }
            },
            ForwardMode::Normal => egress,
        };

        // Ordinary CONNECT has protocol-specific tunnel handoff semantics. Auto
        // Auto dispatch must not negotiate H3, whose CONNECT tunnel handoff is
        // unsupported, so retain the universally supported H1 form.
        if mode == ForwardMode::Normal
            && parts.method == http::Method::CONNECT
            && egress == EgressProtocol::Negotiated
        {
            egress = EgressProtocol::Http1;
        }

        if server_wide_options.is_some_and(|options| !options.authority_in_target)
            && scheme == Scheme::HTTPS
        {
            match egress {
                // h2 cannot represent an HTTPS scheme without URI authority.
                // Negotiated forwarding therefore selects H1 rather than emit
                // the h2 crate's HTTP/1 translation default of `:scheme=http`.
                EgressProtocol::Negotiated => egress = EgressProtocol::Http1,
                EgressProtocol::Http2 => {
                    return Err(Error::Unsupported(
                        "authority-omitted HTTPS OPTIONS * cannot be represented by the HTTP/2 transport"
                            .to_owned(),
                    ));
                }
                EgressProtocol::Http1 | EgressProtocol::Http3 => {}
            }
        }

        if egress == EgressProtocol::Http3 {
            if !allow_h3 {
                return Err(Error::Unsupported(
                    "HTTP/3 forwarding is unavailable on this runtime".to_owned(),
                ));
            }
            if scheme != Scheme::HTTPS {
                return Err(Error::Unsupported(
                    "HTTP/3 forwarding requires an HTTPS upstream".to_owned(),
                ));
            }
            if parts.method == http::Method::CONNECT {
                return Err(Error::Unsupported(
                    "ordinary CONNECT forwarding over HTTP/3 is not supported".to_owned(),
                ));
            }
        }
        if parts.method == http::Method::CONNECT
            && egress == EgressProtocol::Http2
            && !allow_extended_connect
        {
            return Err(Error::Unsupported(
                "HTTP/2 CONNECT forwarding is unavailable on this runtime".to_owned(),
            ));
        }

        let protocol_hint = match egress {
            EgressProtocol::Negotiated => match requested_hint {
                ProtocolHint::AdaptiveH2c => ProtocolHint::AdaptiveH2c,
                _ => ProtocolHint::Auto,
            },
            EgressProtocol::Http1 => ProtocolHint::Http1,
            EgressProtocol::Http2 if scheme == Scheme::HTTP => ProtocolHint::H2c,
            EgressProtocol::Http2 => ProtocolHint::Http2,
            EgressProtocol::Http3 => ProtocolHint::Http3,
        };
        let wire_authority = if let Some(connect_authority) = connect_authority {
            rewrite_host(&mut parts.headers, &connect_authority)?;
            connect_authority
        } else {
            forwarded_authority(
                &mut parts.headers,
                full_uri.authority().ok_or_else(|| {
                    Error::InvalidUrl("forward: final URI has no authority".into())
                })?,
                preserve_host,
                inbound_target.preserved_authority(),
            )?
        };
        let target_form = if let Some(server_wide_options) = server_wide_options {
            match (egress, server_wide_options.authority_in_target) {
                (EgressProtocol::Negotiated | EgressProtocol::Http1, _) => {
                    RequestTargetForm::Asterisk
                }
                (EgressProtocol::Http2 | EgressProtocol::Http3, true) => {
                    RequestTargetForm::AsteriskAbsolute
                }
                (EgressProtocol::Http2, false) => RequestTargetForm::Asterisk,
                (EgressProtocol::Http3, false) => RequestTargetForm::AsteriskAuthorityOmitted,
            }
        } else {
            match mode {
                ForwardMode::ExtendedConnect => RequestTargetForm::Absolute,
                ForwardMode::Normal if parts.method == http::Method::CONNECT => {
                    RequestTargetForm::Authority
                }
                ForwardMode::Normal
                    if matches!(egress, EgressProtocol::Http2 | EgressProtocol::Http3) =>
                {
                    RequestTargetForm::Absolute
                }
                ForwardMode::Normal | ForwardMode::H1Upgrade => RequestTargetForm::Origin,
            }
        };
        if target_form == RequestTargetForm::AsteriskAuthorityOmitted {
            // TODO(http3-malformed-fields): remove this fail-closed boundary
            // when upstream h3 can encode :scheme without :authority.
            return Err(Error::Unsupported(
                "HTTP/3 forwarding cannot encode authority-free OPTIONS * with upstream h3"
                    .to_owned(),
            ));
        }
        let authority_override = matches!(
            target_form,
            RequestTargetForm::Absolute
                | RequestTargetForm::Authority
                | RequestTargetForm::AsteriskAuthorityOmitted
                | RequestTargetForm::AsteriskAbsolute
        )
        .then_some(&wire_authority);
        let final_request_uri = request_uri(&full_uri, target_form, authority_override)?;
        let h3_asterisk_authority = (egress == EgressProtocol::Http3)
            .then_some(server_wide_options)
            .flatten()
            .map(|_| {
                if target_form == RequestTargetForm::AsteriskAuthorityOmitted {
                    ForwardAsteriskAuthority::Omit
                } else {
                    ForwardAsteriskAuthority::Include
                }
            });
        let deferred_target = (egress == EgressProtocol::Negotiated).then(|| {
            Ok::<_, Error>(DeferredForwardTarget {
                h1_uri: request_uri(
                    &full_uri,
                    if server_wide_options.is_some() {
                        RequestTargetForm::Asterisk
                    } else {
                        RequestTargetForm::Origin
                    },
                    None,
                )?,
                h2_h3_uri: request_uri(
                    &full_uri,
                    if server_wide_options.is_some() {
                        RequestTargetForm::AsteriskAbsolute
                    } else {
                        RequestTargetForm::Absolute
                    },
                    Some(&wire_authority),
                )?,
                h2_uses_h1_translation: server_wide_options
                    .is_some_and(|options| !options.authority_in_target),
            })
        });
        let deferred_target = deferred_target.transpose()?;
        let semantic_request_target = match target_form {
            RequestTargetForm::Origin => final_request_uri.clone(),
            RequestTargetForm::Absolute => request_uri(&full_uri, RequestTargetForm::Origin, None)?,
            RequestTargetForm::Authority => request_uri(
                &full_uri,
                RequestTargetForm::Authority,
                Some(&wire_authority),
            )?,
            RequestTargetForm::Asterisk
            | RequestTargetForm::AsteriskAuthorityOmitted
            | RequestTargetForm::AsteriskAbsolute => Uri::from_static("*"),
        };
        let signing_target = if matches!(
            target_form,
            RequestTargetForm::Authority
                | RequestTargetForm::Asterisk
                | RequestTargetForm::AsteriskAuthorityOmitted
                | RequestTargetForm::AsteriskAbsolute
        ) {
            pathless_absolute_uri(&full_uri, Some(&wire_authority))?
        } else {
            request_uri(
                &full_uri,
                RequestTargetForm::Absolute,
                Some(&wire_authority),
            )?
        };

        let request_version = match egress {
            EgressProtocol::Negotiated | EgressProtocol::Http1 => http::Version::HTTP_11,
            EgressProtocol::Http2 if target_form == RequestTargetForm::Asterisk => {
                http::Version::HTTP_11
            }
            EgressProtocol::Http2 => http::Version::HTTP_2,
            EgressProtocol::Http3 => http::Version::HTTP_3,
        };
        let request_header_protocol = header_protocol_for_egress(egress);
        let deferred_te = (request_header_protocol == HeaderProtocol::Negotiated)
            .then(|| hop_by_hop::deferred_te(&parts.headers, downstream_accepts_trailers))
            .flatten();
        let final_trailer_policy = hop_by_hop::sanitize_request(
            &mut parts.headers,
            request_header_protocol,
            mode == ForwardMode::H1Upgrade,
            downstream_accepts_trailers,
        )?;
        // Connection-nominated fields are removed by sanitization. Host is
        // generated control data for the selected wire authority, so restore
        // its canonical value only after that removal is complete.
        rewrite_host(&mut parts.headers, &wire_authority)?;
        let mut request_trailers = rewritten_trailer_policy.clone();
        request_trailers.merge(final_trailer_policy);
        match egress {
            EgressProtocol::Negotiated => request_trailers.defer_to_connection(&parts.headers),
            EgressProtocol::Http1 => request_trailers.restrict_to_declaration(&parts.headers),
            EgressProtocol::Http2 | EgressProtocol::Http3 => {}
        }

        Ok(Self {
            full_uri,
            request_uri: final_request_uri,
            request_version,
            protocol_hint,
            deferred_te,
            negotiated_framing: egress == EgressProtocol::Negotiated,
            deferred_target,
            h3_asterisk_authority,
            signing_target: ForwardSigningTarget {
                target_uri: signing_target,
                semantic_request_target,
            },
            response_headers: ResponseHeaderPolicy::new(
                downstream_protocol,
                downstream_method.clone(),
                downstream_accepts_trailers,
                h1_upgrade_offer,
            ),
            request_trailers,
        })
    }

    pub(crate) fn full_uri(&self) -> &Uri {
        &self.full_uri
    }

    pub(crate) fn protocol_hint(&self) -> ProtocolHint {
        self.protocol_hint
    }

    pub(crate) fn response_header_policy(&self) -> ResponseHeaderPolicy {
        self.response_headers.clone()
    }

    pub(crate) fn request_trailer_policy(&self) -> TrailerPolicy {
        self.request_trailers.clone()
    }

    pub(crate) fn apply(
        &self,
        parts: &mut http::request::Parts,
        body_is_end_stream: bool,
    ) -> Result<(), Error> {
        parts.uri = self.request_uri.clone();
        parts.version = self.request_version;
        parts.extensions.insert(self.protocol_hint);
        if let Some(deferred_te) = self.deferred_te {
            parts.extensions.insert(deferred_te);
        }
        if let Some(target) = &self.deferred_target {
            parts.extensions.insert(target.clone());
        }
        if let Some(authority) = self.h3_asterisk_authority {
            parts.extensions.insert(authority);
        }
        if body_is_end_stream {
            parts.headers.remove(http::header::TRAILER);
        }
        let framing = DeferredForwardFraming {
            has_body: !body_is_end_stream,
            has_trailer_declaration: parts.headers.contains_key(http::header::TRAILER),
        };
        if self.negotiated_framing {
            parts.extensions.insert(framing);
            parts
                .extensions
                .insert(DeferredForwardTrailers(self.request_trailers.clone()));
        } else if self.request_version == http::Version::HTTP_11 {
            framing.apply(&mut parts.headers, self.request_version)?;
        }
        parts.extensions.insert(self.signing_target.clone());
        Ok(())
    }
}

fn apply_http11_request_framing(
    headers: &mut HeaderMap,
    has_body: bool,
    has_trailer_declaration: bool,
) -> Result<(), Error> {
    headers.remove(http::header::TRANSFER_ENCODING);
    if !has_body {
        headers.remove(http::header::TRAILER);
        return Ok(());
    }
    if has_trailer_declaration || !headers.contains_key(http::header::CONTENT_LENGTH) {
        headers.remove(http::header::CONTENT_LENGTH);
        headers.insert(
            http::header::TRANSFER_ENCODING,
            HeaderValue::from_static("chunked"),
        );
    }
    Ok(())
}

fn header_protocol_for_egress(egress: EgressProtocol) -> HeaderProtocol {
    match egress {
        EgressProtocol::Negotiated => HeaderProtocol::Negotiated,
        EgressProtocol::Http1 => HeaderProtocol::Http11,
        EgressProtocol::Http2 => HeaderProtocol::Http2,
        EgressProtocol::Http3 => HeaderProtocol::Http3,
    }
}

fn header_protocol_for_version(version: http::Version) -> Result<HeaderProtocol, Error> {
    match version {
        http::Version::HTTP_10 => Ok(HeaderProtocol::Http10),
        http::Version::HTTP_11 => Ok(HeaderProtocol::Http11),
        http::Version::HTTP_2 => Ok(HeaderProtocol::Http2),
        http::Version::HTTP_3 => Ok(HeaderProtocol::Http3),
        _ => Err(Error::Unsupported(format!(
            "forwarding downstream HTTP version {version:?} is not supported"
        ))),
    }
}

fn egress_for_hint(hint: ProtocolHint) -> EgressProtocol {
    match hint {
        ProtocolHint::Auto | ProtocolHint::AdaptiveH2c => EgressProtocol::Negotiated,
        ProtocolHint::Http1 => EgressProtocol::Http1,
        ProtocolHint::Http2 | ProtocolHint::H2c => EgressProtocol::Http2,
        ProtocolHint::Http3 => EgressProtocol::Http3,
    }
}

fn egress_for_explicit_version(version: http::Version) -> Result<EgressProtocol, Error> {
    match version {
        http::Version::HTTP_10 | http::Version::HTTP_11 => Ok(EgressProtocol::Http1),
        http::Version::HTTP_2 => Ok(EgressProtocol::Http2),
        http::Version::HTTP_3 => Ok(EgressProtocol::Http3),
        _ => Err(Error::Unsupported(format!(
            "forward hook selected unsupported HTTP version {version:?}"
        ))),
    }
}

fn validate_method_rewrite(
    downstream_method: &http::Method,
    upstream_method: &http::Method,
) -> Result<(), Error> {
    let downstream_class = method_semantic_class(downstream_method);
    let upstream_class = method_semantic_class(upstream_method);
    if downstream_class != upstream_class {
        return Err(Error::Unsupported(format!(
            "forward hooks cannot rewrite {downstream_method} to {upstream_method} across HEAD or CONNECT response semantic classes"
        )));
    }
    Ok(())
}

fn method_semantic_class(method: &http::Method) -> MethodSemanticClass {
    if *method == http::Method::HEAD {
        MethodSemanticClass::Head
    } else if *method == http::Method::CONNECT {
        MethodSemanticClass::Connect
    } else {
        MethodSemanticClass::Ordinary
    }
}

fn resolve_hook_uri(
    rewritten_uri: &Uri,
    hook_uri: &Uri,
    method: &http::Method,
) -> Result<(Uri, Option<ServerWideOptions>), Error> {
    if hook_uri.path() == "*" {
        if hook_uri.path_and_query().map(|value| value.as_str()) != Some("*") {
            return Err(Error::InvalidUrl(
                "the asterisk request target must not include a query".to_owned(),
            ));
        }
        if *method != http::Method::OPTIONS {
            return Err(Error::Unsupported(
                "the asterisk request target is only valid for OPTIONS".to_owned(),
            ));
        }
        return match (hook_uri.scheme(), hook_uri.authority()) {
            (None, None) => Ok((
                rewritten_uri.clone(),
                Some(ServerWideOptions {
                    authority_in_target: false,
                }),
            )),
            (Some(_), Some(authority)) => {
                validate_authority(authority)?;
                Ok((
                    hook_uri.clone(),
                    Some(ServerWideOptions {
                        authority_in_target: true,
                    }),
                ))
            }
            _ => Err(Error::InvalidUrl(format!(
                "forward: invalid asterisk request target `{hook_uri}`"
            ))),
        };
    }

    match (hook_uri.scheme(), hook_uri.authority()) {
        (Some(_), Some(authority)) => {
            validate_authority(authority)?;
            if !hook_uri.path().is_empty() && !hook_uri.path().starts_with('/') {
                return Err(Error::InvalidUrl(
                    "forward: absolute request targets must use an absolute path".to_owned(),
                ));
            }
            if hook_uri.path_and_query().is_none() && *method == http::Method::OPTIONS {
                return Ok((
                    hook_uri.clone(),
                    Some(ServerWideOptions {
                        authority_in_target: true,
                    }),
                ));
            }
            normalize_absolute_hook_uri(hook_uri).map(|uri| (uri, None))
        }
        (None, None) if hook_uri.path().starts_with('/') => {
            let mut uri_parts = rewritten_uri.clone().into_parts();
            uri_parts.path_and_query = hook_uri.path_and_query().cloned();
            Uri::from_parts(uri_parts)
                .map(|uri| (uri, None))
                .map_err(|error| Error::InvalidUrl(format!("forward: {error}")))
        }
        (None, Some(authority)) if *method == http::Method::CONNECT => {
            let mut uri_parts = rewritten_uri.clone().into_parts();
            uri_parts.authority = Some(authority.clone());
            uri_parts.path_and_query = Some(PathAndQuery::from_static("/"));
            Uri::from_parts(uri_parts)
                .map(|uri| (uri, None))
                .map_err(|error| Error::InvalidUrl(format!("forward: {error}")))
        }
        _ => Err(Error::InvalidUrl(format!(
            "forward: unsupported request target `{hook_uri}`"
        ))),
    }
}

fn normalize_absolute_hook_uri(uri: &Uri) -> Result<Uri, Error> {
    let path_and_query = match uri.path_and_query() {
        Some(path_and_query) if !path_and_query.as_str().starts_with('?') => {
            return Ok(uri.clone());
        }
        Some(path_and_query) => format!("/{}", path_and_query.as_str())
            .parse()
            .map_err(|error| Error::InvalidUrl(format!("forward: {error}")))?,
        None => PathAndQuery::from_static("/"),
    };
    let mut parts = uri.clone().into_parts();
    parts.path_and_query = Some(path_and_query);
    Uri::from_parts(parts).map_err(|error| Error::InvalidUrl(format!("forward: {error}")))
}

fn pathless_absolute_uri(
    uri: &Uri,
    authority: Option<&http::uri::Authority>,
) -> Result<Uri, Error> {
    let scheme = uri
        .scheme()
        .ok_or_else(|| Error::InvalidUrl("forward: final URI has no scheme".into()))?;
    let authority =
        authority.ok_or_else(|| Error::InvalidUrl("forward: final URI has no authority".into()))?;
    format!("{scheme}://{authority}")
        .parse()
        .map_err(|error| Error::InvalidUrl(format!("forward: {error}")))
}

pub(crate) struct ForwardRewrite<'a> {
    pub(crate) upstream: &'a Uri,
    pub(crate) strip_prefix: Option<&'a str>,
    pub(crate) preserve_host: bool,
    pub(crate) forward_headers: &'a [HeaderName],
    pub(crate) extra_headers: &'a HeaderMap,
    pub(crate) remove_headers: &'a [HeaderName],
}

pub(crate) struct ForwardRewriteResult {
    pub(crate) uri: Uri,
    pub(crate) inbound_target: InboundRequestTarget,
    pub(crate) trailer_policy: TrailerPolicy,
}

pub(crate) fn rewrite_for_upstream(
    parts: &mut http::request::Parts,
    rewrite: ForwardRewrite<'_>,
) -> Result<ForwardRewriteResult, Error> {
    hop_by_hop::validate_inbound_request_headers(parts.version, &mut parts.headers)?;
    let inbound_target = InboundRequestTarget::capture(parts)?;
    let trailer_policy = TrailerPolicy::capture_request_for_version(&parts.headers, parts.version);
    let preserve_upgrade = is_h1_upgrade_request(&parts.headers);
    let preserve_h2c_settings = preserve_upgrade && hop_by_hop::is_h2c_upgrade(&parts.headers);
    let mut preserved_names = rewrite.forward_headers.to_vec();
    if !preserved_names.contains(&http::header::TE) {
        preserved_names.push(http::header::TE);
    }
    let mut forwarded_values = Vec::new();
    let mut visited_names = Vec::new();
    for name in preserved_names {
        if visited_names.contains(&name) {
            continue;
        }
        visited_names.push(name.clone());
        let connection_nominated = trailer_policy.connection_names().contains(&name);
        let controlled_exception = name == http::header::TE
            || (preserve_upgrade && name == http::header::UPGRADE)
            || (preserve_h2c_settings && name == "http2-settings");
        if connection_nominated && !controlled_exception {
            continue;
        }
        forwarded_values.extend(
            parts
                .headers
                .get_all(&name)
                .iter()
                .cloned()
                .map(|value| (name.clone(), value)),
        );
    }

    hop_by_hop::strip_hop_by_hop_with_policy(&mut parts.headers, &trailer_policy);

    let upstream_scheme = rewrite
        .upstream
        .scheme()
        .map(super::scheme::canonical_http_scheme)
        .transpose()?
        .unwrap_or(Scheme::HTTP);
    let upstream_authority = rewrite
        .upstream
        .authority()
        .cloned()
        .ok_or_else(|| Error::InvalidUrl("forward: upstream has no authority".into()))?;

    let server_wide_options_authority_in_target =
        inbound_target.server_wide_options_authority_in_target();
    let is_server_wide_options = server_wide_options_authority_in_target.is_some();
    let original_path = if is_server_wide_options
        || (parts.method == http::Method::CONNECT && parts.uri.path().is_empty())
        || parts.uri.path().is_empty()
    {
        "/"
    } else {
        parts.uri.path()
    };
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

    if rewrite.preserve_host {
        if let Some(authority) = inbound_target.preserved_authority() {
            rewrite_host(&mut parts.headers, authority)?;
        }
    } else {
        rewrite_host(&mut parts.headers, &upstream_authority)?;
    }

    for name in visited_names {
        parts.headers.remove(name);
    }
    for (name, value) in forwarded_values {
        parts.headers.append(name, value);
    }
    for (name, value) in rewrite.extra_headers {
        parts.headers.insert(name, value.clone());
    }
    for name in rewrite.remove_headers {
        parts.headers.remove(name);
    }

    parts.uri = match server_wide_options_authority_in_target {
        Some(true) => request_uri(
            &full_uri,
            RequestTargetForm::AsteriskAbsolute,
            full_uri.authority(),
        )?,
        Some(false) => Uri::from_static("*"),
        None => full_uri.clone(),
    };
    Ok(ForwardRewriteResult {
        uri: full_uri,
        inbound_target,
        trailer_policy,
    })
}

fn rewrite_host(headers: &mut HeaderMap, authority: &http::uri::Authority) -> Result<(), Error> {
    let host = authority
        .as_str()
        .parse::<HeaderValue>()
        .map_err(|error| Error::InvalidHeader(format!("invalid upstream Host field: {error}")))?;
    headers.remove(HOST);
    headers.insert(HOST, host);
    Ok(())
}

fn forwarded_authority(
    headers: &mut HeaderMap,
    fallback: &http::uri::Authority,
    require_host: bool,
    preserved_authority: Option<&http::uri::Authority>,
) -> Result<http::uri::Authority, Error> {
    match single_host_field(headers)? {
        HostField::Authority(host) => Ok(host),
        HostField::Missing if require_host => {
            let authority = preserved_authority.ok_or_else(|| {
                Error::InvalidHeader(
                    "preserve_host requires an inbound authority or Host field".to_owned(),
                )
            })?;
            rewrite_host(headers, authority)?;
            Ok(authority.clone())
        }
        HostField::Empty if require_host => Err(Error::InvalidHeader(
            "preserve_host cannot use an empty Host field as an upstream authority".to_owned(),
        )),
        HostField::Missing | HostField::Empty => {
            rewrite_host(headers, fallback)?;
            Ok(fallback.clone())
        }
    }
}

fn request_uri(
    full_uri: &Uri,
    target_form: RequestTargetForm,
    authority_override: Option<&http::uri::Authority>,
) -> Result<Uri, Error> {
    match target_form {
        RequestTargetForm::Origin => full_uri
            .path_and_query()
            .map(|path_and_query| path_and_query.as_str())
            .unwrap_or("/")
            .parse()
            .map_err(|error| Error::Other(Box::new(error))),
        RequestTargetForm::Absolute => {
            let Some(authority) = authority_override else {
                return Ok(full_uri.clone());
            };
            if full_uri.path_and_query().is_none() {
                return pathless_absolute_uri(full_uri, Some(authority));
            }
            let mut parts = full_uri.clone().into_parts();
            parts.authority = Some(authority.clone());
            Uri::from_parts(parts).map_err(|error| Error::InvalidUrl(format!("forward: {error}")))
        }
        RequestTargetForm::Authority => authority_override
            .or_else(|| full_uri.authority())
            .ok_or_else(|| Error::InvalidUrl("forward: final URI has no authority".into()))?
            .as_str()
            .parse()
            .map_err(|error| Error::Other(Box::new(error))),
        RequestTargetForm::Asterisk => Ok(Uri::from_static("*")),
        // `http::Uri` cannot represent a scheme without an authority. The H3
        // encoder consumes `ForwardAsteriskAuthority` to omit `:authority` for
        // the distinct `AsteriskAuthorityOmitted` form.
        RequestTargetForm::AsteriskAuthorityOmitted | RequestTargetForm::AsteriskAbsolute => {
            let mut parts = full_uri.clone().into_parts();
            parts.authority = authority_override.cloned();
            parts.path_and_query = Some(PathAndQuery::from_static("*"));
            Uri::from_parts(parts).map_err(|error| Error::InvalidUrl(format!("forward: {error}")))
        }
    }
}

#[cfg(test)]
mod tests;
