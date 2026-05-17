use http::header::{HeaderMap, HeaderName};

const HOP_BY_HOP: &[&str] = &[
    "connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "proxy-connection",
    "te",
    "trailer",
    "transfer-encoding",
];

pub(crate) fn strip_hop_by_hop(headers: &mut HeaderMap) {
    let mut dynamic_names: Vec<HeaderName> = Vec::new();
    if let Some(conn) = headers.get("connection")
        && let Ok(s) = conn.to_str()
    {
        for token in s.split(',') {
            let token = token.trim();
            if !token.is_empty()
                && let Ok(name) = HeaderName::from_bytes(token.as_bytes())
            {
                dynamic_names.push(name);
            }
        }
    }

    for name in HOP_BY_HOP {
        headers.remove(*name);
    }
    for name in &dynamic_names {
        headers.remove(name);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use http::header::HeaderValue;

    #[test]
    fn strips_known_hop_by_hop_headers() {
        let mut headers = HeaderMap::new();
        headers.insert("connection", HeaderValue::from_static("keep-alive"));
        headers.insert("keep-alive", HeaderValue::from_static("timeout=5"));
        headers.insert("te", HeaderValue::from_static("trailers"));
        headers.insert("transfer-encoding", HeaderValue::from_static("chunked"));
        headers.insert("x-custom", HeaderValue::from_static("preserved"));
        headers.insert("content-type", HeaderValue::from_static("text/plain"));

        strip_hop_by_hop(&mut headers);

        assert!(!headers.contains_key("connection"));
        assert!(!headers.contains_key("keep-alive"));
        assert!(!headers.contains_key("te"));
        assert!(!headers.contains_key("transfer-encoding"));
        assert!(headers.contains_key("x-custom"));
        assert!(headers.contains_key("content-type"));
    }

    #[test]
    fn strips_proxy_headers() {
        let mut headers = HeaderMap::new();
        headers.insert("proxy-authenticate", HeaderValue::from_static("Basic"));
        headers.insert("proxy-authorization", HeaderValue::from_static("secret"));
        headers.insert("proxy-connection", HeaderValue::from_static("keep-alive"));

        strip_hop_by_hop(&mut headers);

        assert!(!headers.contains_key("proxy-authenticate"));
        assert!(!headers.contains_key("proxy-authorization"));
        assert!(!headers.contains_key("proxy-connection"));
    }

    #[test]
    fn noop_on_empty_headers() {
        let mut headers = HeaderMap::new();
        strip_hop_by_hop(&mut headers);
        assert!(headers.is_empty());
    }

    #[test]
    fn strips_dynamic_connection_listed_headers() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "connection",
            HeaderValue::from_static("X-Custom-Debug, close"),
        );
        headers.insert("x-custom-debug", HeaderValue::from_static("sensitive"));
        headers.insert("content-type", HeaderValue::from_static("text/plain"));

        strip_hop_by_hop(&mut headers);

        assert!(!headers.contains_key("connection"));
        assert!(!headers.contains_key("x-custom-debug"));
        assert!(headers.contains_key("content-type"));
    }

    #[test]
    fn strips_multiple_dynamic_headers() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "connection",
            HeaderValue::from_static("X-Foo, X-Bar, close"),
        );
        headers.insert("x-foo", HeaderValue::from_static("foo-val"));
        headers.insert("x-bar", HeaderValue::from_static("bar-val"));
        headers.insert("x-keep", HeaderValue::from_static("keep-val"));

        strip_hop_by_hop(&mut headers);

        assert!(!headers.contains_key("connection"));
        assert!(!headers.contains_key("x-foo"));
        assert!(!headers.contains_key("x-bar"));
        assert!(headers.contains_key("x-keep"));
    }
}
