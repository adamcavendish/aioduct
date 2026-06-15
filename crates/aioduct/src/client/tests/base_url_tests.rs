use super::resolve_request_url;

fn join(base: &str, input: &str) -> String {
    let base = url::Url::parse(base).unwrap();
    let (uri, _frag) = resolve_request_url(Some(&base), input).unwrap();
    uri.to_string()
}

#[test]
fn relative_path_against_base_with_trailing_slash() {
    assert_eq!(
        join("https://api.example.com/v1/", "users"),
        "https://api.example.com/v1/users"
    );
}

#[test]
fn relative_path_against_base_without_trailing_slash_replaces_last_segment() {
    // RFC 3986: the last path segment of the base is replaced.
    assert_eq!(
        join("https://api.example.com/v1", "users"),
        "https://api.example.com/users"
    );
}

#[test]
fn absolute_path_replaces_base_path() {
    assert_eq!(
        join("https://api.example.com/v1/", "/users"),
        "https://api.example.com/users"
    );
}

#[test]
fn query_only_reference_keeps_base_path() {
    assert_eq!(
        join("https://api.example.com/v1/items", "?page=2"),
        "https://api.example.com/v1/items?page=2"
    );
}

#[test]
fn absolute_url_overrides_base() {
    assert_eq!(
        join("https://api.example.com/v1/", "https://other.example.org/x"),
        "https://other.example.org/x"
    );
}

#[test]
fn absolute_http_override_to_other_host_is_allowed() {
    // Same-kind override (http/https) is fine even to a different host.
    assert_eq!(
        join("https://api.example.com/v1/", "http://other.example.org/x"),
        "http://other.example.org/x"
    );
}

#[test]
fn non_http_absolute_override_is_rejected() {
    // With a base set, an absolute non-http(s) override must not slip through
    // and get dispatched as cleartext HTTP.
    let base = url::Url::parse("https://api.example.com/").unwrap();
    assert!(resolve_request_url(Some(&base), "ftp://example.com/path").is_err());
    assert!(resolve_request_url(Some(&base), "data:text/plain,hi").is_err());
    assert!(resolve_request_url(Some(&base), "file:///etc/passwd").is_err());
}

#[test]
fn dot_segments_are_resolved() {
    assert_eq!(
        join("https://api.example.com/v1/a/", "../b"),
        "https://api.example.com/v1/b"
    );
}

#[test]
fn fragment_is_extracted_when_base_set() {
    let base = url::Url::parse("https://api.example.com/v1/").unwrap();
    let (uri, fragment) = resolve_request_url(Some(&base), "users#section").unwrap();
    assert_eq!(uri.to_string(), "https://api.example.com/v1/users");
    assert_eq!(fragment.as_deref(), Some("section"));
}

#[test]
fn no_base_parses_absolute_input_unchanged() {
    let (uri, fragment) = resolve_request_url(None, "https://example.com/path?q=1#frag").unwrap();
    assert_eq!(uri.to_string(), "https://example.com/path?q=1");
    assert_eq!(fragment.as_deref(), Some("frag"));
}

#[test]
fn no_base_relative_input_parses_unchanged() {
    // Without a base, the input is parsed directly as an http::Uri (existing
    // behavior); resolution against a base only happens when one is set.
    let (uri, _frag) = resolve_request_url(None, "users").unwrap();
    assert_eq!(uri.to_string(), "users");
}

#[test]
fn invalid_relative_against_base_is_an_error() {
    let base = url::Url::parse("https://api.example.com/").unwrap();
    // An input that url cannot join (bad scheme-relative form with control chars).
    assert!(resolve_request_url(Some(&base), "http://[::bad").is_err());
}

#[cfg(feature = "tokio")]
mod builder_validation {
    use crate::client::HttpEngineSend;
    use crate::runtime::TokioRuntime;
    use crate::runtime::tokio_rt::TcpConnector;

    #[test]
    fn rejects_non_http_scheme() {
        let result =
            HttpEngineSend::<TokioRuntime, TcpConnector>::builder().base_url("ftp://host/path");
        assert!(result.is_err(), "ftp base_url should be rejected");
    }

    #[test]
    fn rejects_base_without_host() {
        // `http://` has no host (the parser errors with "empty host").
        let result = HttpEngineSend::<TokioRuntime, TcpConnector>::builder().base_url("http://");
        assert!(result.is_err(), "hostless base_url should be rejected");
    }

    #[test]
    fn accepts_http_and_https() {
        assert!(
            HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
                .base_url("http://example.com/")
                .is_ok()
        );
        assert!(
            HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
                .base_url("https://example.com/v1/")
                .is_ok()
        );
    }
}
