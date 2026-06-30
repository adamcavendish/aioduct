use base64::Engine as _;
use http::header::{HeaderMap, HeaderName};
use http::{Method, StatusCode, Uri};

use super::component::{
    MessageSignatureComponentKind, MessageSignatureComponentTarget, encode_query_param_component,
};
use super::structured_fields;
use super::{MessageSignatureComponent, MessageSignatureError};

#[allow(dead_code)]
pub(crate) enum MessageSignatureContext<'a> {
    Request {
        method: &'a Method,
        target_uri: &'a Uri,
        request_target: &'a Uri,
        headers: &'a HeaderMap,
    },
    Response {
        status: StatusCode,
        headers: &'a HeaderMap,
    },
    RequestResponse {
        method: &'a Method,
        target_uri: &'a Uri,
        request_target: &'a Uri,
        request_headers: &'a HeaderMap,
        status: StatusCode,
        response_headers: &'a HeaderMap,
    },
}

impl<'a> MessageSignatureContext<'a> {
    pub(crate) fn request(
        method: &'a Method,
        target_uri: &'a Uri,
        request_target: &'a Uri,
        headers: &'a HeaderMap,
    ) -> Self {
        Self::Request {
            method,
            target_uri,
            request_target,
            headers,
        }
    }

    #[allow(dead_code)]
    pub(crate) fn response(status: StatusCode, headers: &'a HeaderMap) -> Self {
        Self::Response { status, headers }
    }

    #[allow(dead_code)]
    pub(crate) fn request_response(
        method: &'a Method,
        target_uri: &'a Uri,
        request_target: &'a Uri,
        request_headers: &'a HeaderMap,
        status: StatusCode,
        response_headers: &'a HeaderMap,
    ) -> Self {
        Self::RequestResponse {
            method,
            target_uri,
            request_target,
            request_headers,
            status,
            response_headers,
        }
    }

    pub(crate) fn component_value(
        &self,
        component: &MessageSignatureComponent,
    ) -> Result<String, MessageSignatureError> {
        let identifier = component.identifier()?;
        self.ensure_target(component, &identifier)?;

        match self {
            Self::Request {
                method,
                target_uri,
                request_target,
                headers,
            } => request_component_value(component, method, target_uri, request_target, headers),
            Self::Response { headers, .. } => response_component_value(component, headers),
            Self::RequestResponse {
                method,
                target_uri,
                request_target,
                request_headers,
                response_headers,
                ..
            } => request_response_component_value(
                component,
                method,
                target_uri,
                request_target,
                request_headers,
                response_headers,
            ),
        }
    }

    fn ensure_target(
        &self,
        component: &MessageSignatureComponent,
        identifier: &str,
    ) -> Result<(), MessageSignatureError> {
        match (self, component.target()) {
            (Self::Response { .. }, MessageSignatureComponentTarget::Request) => {
                Err(MessageSignatureError::ComponentNotAvailable {
                    component: identifier.to_owned(),
                    context: "response",
                })
            }
            _ => Ok(()),
        }
    }

    #[cfg(test)]
    pub(crate) fn status_for_test(&self) -> Option<StatusCode> {
        match self {
            Self::Request { .. } => None,
            Self::Response { status, .. } | Self::RequestResponse { status, .. } => Some(*status),
        }
    }
}

fn request_component_value(
    component: &MessageSignatureComponent,
    method: &Method,
    target_uri: &Uri,
    request_target: &Uri,
    headers: &HeaderMap,
) -> Result<String, MessageSignatureError> {
    match component.kind() {
        MessageSignatureComponentKind::QueryParam => query_param_value(component, target_uri),
        MessageSignatureComponentKind::Header(name) => {
            header_component_value(component, headers, name)
        }
        _ if component.has_parameters() => Err(unsupported_component_parameters(component)?),
        MessageSignatureComponentKind::Method => Ok(method.as_str().to_owned()),
        MessageSignatureComponentKind::Scheme => target_uri
            .scheme_str()
            .map(|scheme| scheme.to_ascii_lowercase())
            .ok_or(MessageSignatureError::MissingScheme),
        MessageSignatureComponentKind::Authority => canonical_authority(target_uri),
        MessageSignatureComponentKind::RequestTarget => Ok(request_target.to_string()),
        MessageSignatureComponentKind::TargetUri => Ok(target_uri.to_string()),
        MessageSignatureComponentKind::Path => {
            let path = target_uri.path();
            if path.is_empty() {
                Ok("/".to_owned())
            } else {
                Ok(path.to_owned())
            }
        }
        MessageSignatureComponentKind::Query => Ok(match target_uri.query() {
            Some(query) => format!("?{query}"),
            None => "?".to_owned(),
        }),
    }
}

fn response_component_value(
    component: &MessageSignatureComponent,
    headers: &HeaderMap,
) -> Result<String, MessageSignatureError> {
    match component.kind() {
        MessageSignatureComponentKind::Header(name) => {
            header_component_value(component, headers, name)
        }
        _ => Err(unsupported_component(component)?),
    }
}

fn request_response_component_value(
    component: &MessageSignatureComponent,
    method: &Method,
    target_uri: &Uri,
    request_target: &Uri,
    request_headers: &HeaderMap,
    response_headers: &HeaderMap,
) -> Result<String, MessageSignatureError> {
    match component.kind() {
        MessageSignatureComponentKind::Header(name) => {
            header_component_value(component, response_headers, name)
        }
        _ => request_component_value(
            component,
            method,
            target_uri,
            request_target,
            request_headers,
        ),
    }
}

fn header_component_value(
    component: &MessageSignatureComponent,
    headers: &HeaderMap,
    name: &HeaderName,
) -> Result<String, MessageSignatureError> {
    if let Some(key) = component.dictionary_key() {
        canonical_header_dictionary_member_value(headers, name, key)
    } else if component.has_only_byte_sequence_parameter() {
        canonical_header_byte_sequence_value(headers, name)
    } else if component.has_parameters() {
        Err(unsupported_component_parameters(component)?)
    } else {
        canonical_header_value(headers, name)
    }
}

fn canonical_header_dictionary_member_value(
    headers: &HeaderMap,
    name: &HeaderName,
    key: &str,
) -> Result<String, MessageSignatureError> {
    let value = canonical_header_value(headers, name)?;
    structured_fields::dictionary_member(&value, key)
        .map_err(|_| MessageSignatureError::MalformedStructuredField(name.clone()))?
        .ok_or_else(|| MessageSignatureError::MissingDictionaryKey {
            field: name.clone(),
            key: key.to_owned(),
        })
}

fn query_param_value(
    component: &MessageSignatureComponent,
    target_uri: &Uri,
) -> Result<String, MessageSignatureError> {
    let Some(name) = component.query_param_name() else {
        return Err(unsupported_component_parameters(component)?);
    };
    let Some(query) = target_uri.query() else {
        return Err(MessageSignatureError::MissingQueryParam(name.to_owned()));
    };

    let mut value = None;
    for (query_name, query_value) in url::form_urlencoded::parse(query.as_bytes()) {
        if encode_query_param_component(&query_name) == name {
            if value.is_some() {
                return Err(MessageSignatureError::DuplicateQueryParam(name.to_owned()));
            }
            value = Some(encode_query_param_component(&query_value));
        }
    }
    value.ok_or_else(|| MessageSignatureError::MissingQueryParam(name.to_owned()))
}

pub(crate) fn canonical_authority(uri: &Uri) -> Result<String, MessageSignatureError> {
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

pub(crate) fn canonical_header_value(
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

fn canonical_header_byte_sequence_value(
    headers: &HeaderMap,
    name: &HeaderName,
) -> Result<String, MessageSignatureError> {
    let values = headers.get_all(name);
    let mut out = Vec::new();
    for value in values {
        let value = trim_field_value_bytes(value.as_bytes());
        out.push(format!(
            ":{}:",
            base64::engine::general_purpose::STANDARD.encode(value)
        ));
    }
    if out.is_empty() {
        return Err(MessageSignatureError::MissingHeader(name.clone()));
    }
    Ok(out.join(", "))
}

fn normalize_field_value(value: &str) -> String {
    value.trim_matches(|c| c == ' ' || c == '\t').to_owned()
}

fn trim_field_value_bytes(mut value: &[u8]) -> &[u8] {
    while matches!(value.first(), Some(b' ' | b'\t')) {
        value = &value[1..];
    }
    while matches!(value.last(), Some(b' ' | b'\t')) {
        value = &value[..value.len() - 1];
    }
    value
}

fn unsupported_component(
    component: &MessageSignatureComponent,
) -> Result<MessageSignatureError, MessageSignatureError> {
    Ok(MessageSignatureError::UnsupportedComponent(
        component.identifier()?,
    ))
}

fn unsupported_component_parameters(
    component: &MessageSignatureComponent,
) -> Result<MessageSignatureError, MessageSignatureError> {
    Ok(MessageSignatureError::UnsupportedComponentParameters(
        component.identifier()?,
    ))
}

#[cfg(test)]
mod tests {
    use http::header::{CONTENT_TYPE, HeaderMap, HeaderValue};
    use http::{Method, StatusCode, Uri};

    use super::*;

    #[test]
    fn response_context_can_represent_response_headers() {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        let context = MessageSignatureContext::response(StatusCode::OK, &headers);
        let component = MessageSignatureComponent::header(CONTENT_TYPE);

        assert_eq!(context.status_for_test(), Some(StatusCode::OK));
        assert_eq!(
            context.component_value(&component).unwrap(),
            "application/json"
        );
    }

    #[test]
    fn response_context_rejects_request_only_components() {
        let headers = HeaderMap::new();
        let context = MessageSignatureContext::response(StatusCode::OK, &headers);
        let err = context
            .component_value(&MessageSignatureComponent::method())
            .unwrap_err();

        assert!(matches!(
            err,
            MessageSignatureError::ComponentNotAvailable {
                context: "response",
                ..
            }
        ));
    }

    #[test]
    fn request_response_context_can_represent_both_messages() {
        let method = Method::GET;
        let target_uri: Uri = "https://example.com/demo".parse().unwrap();
        let request_target: Uri = "/demo".parse().unwrap();
        let request_headers = HeaderMap::new();
        let mut response_headers = HeaderMap::new();
        response_headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        let context = MessageSignatureContext::request_response(
            &method,
            &target_uri,
            &request_target,
            &request_headers,
            StatusCode::OK,
            &response_headers,
        );

        assert_eq!(
            context
                .component_value(&MessageSignatureComponent::method())
                .unwrap(),
            "GET"
        );
        assert_eq!(
            context
                .component_value(&MessageSignatureComponent::header(CONTENT_TYPE))
                .unwrap(),
            "application/json"
        );
        assert_eq!(context.status_for_test(), Some(StatusCode::OK));
    }
}
