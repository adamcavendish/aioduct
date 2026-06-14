use super::*;
use super::{DEFAULT_USER_AGENT, resolve_redirect};

#[test]
fn resolve_redirect_absolute_url() {
    let base: Uri = "http://example.com/old".parse().unwrap();
    let (result, _frag) = resolve_redirect(&base, "https://other.com/new", None).unwrap();
    assert_eq!(result.to_string(), "https://other.com/new");
}

#[test]
fn resolve_redirect_relative_path() {
    let base: Uri = "http://example.com/old".parse().unwrap();
    let (result, _frag) = resolve_redirect(&base, "/new/path", None).unwrap();
    assert_eq!(result.to_string(), "http://example.com/new/path");
}

#[test]
fn resolve_redirect_relative_with_query() {
    let base: Uri = "https://example.com/page".parse().unwrap();
    let (result, _frag) = resolve_redirect(&base, "/search?q=test", None).unwrap();
    assert_eq!(result.to_string(), "https://example.com/search?q=test");
}

#[test]
fn resolve_redirect_relative_without_leading_slash_uses_base_directory() {
    let base: Uri = "http://example.com/dir/page".parse().unwrap();
    let (result, _frag) = resolve_redirect(&base, "next", None).unwrap();
    assert_eq!(result.to_string(), "http://example.com/dir/next");
}

#[test]
fn resolve_redirect_relative_parent_directory_is_normalized() {
    let base: Uri = "http://example.com/dir/page".parse().unwrap();
    let (result, _frag) = resolve_redirect(&base, "../up", None).unwrap();
    assert_eq!(result.to_string(), "http://example.com/up");
}

#[test]
fn resolve_redirect_query_only_keeps_base_path() {
    let base: Uri = "http://example.com/dir/page?old=1".parse().unwrap();
    let (result, _frag) = resolve_redirect(&base, "?new=2", None).unwrap();
    assert_eq!(result.to_string(), "http://example.com/dir/page?new=2");
}

#[test]
fn resolve_redirect_protocol_relative_uses_base_scheme() {
    let base: Uri = "https://example.com/old".parse().unwrap();
    let (result, _frag) = resolve_redirect(&base, "//other.example/new", None).unwrap();
    assert_eq!(result.to_string(), "https://other.example/new");
}

#[test]
fn resolve_redirect_preserves_port() {
    let base: Uri = "http://example.com:8080/old".parse().unwrap();
    let (result, _frag) = resolve_redirect(&base, "/new", None).unwrap();
    assert_eq!(result.to_string(), "http://example.com:8080/new");
}

#[test]
fn resolve_redirect_inherits_original_fragment_when_location_has_none() {
    let base: Uri = "http://example.com/dir/page".parse().unwrap();
    let (result, fragment) = resolve_redirect(&base, "/target", Some("section1")).unwrap();
    assert_eq!(result.path(), "/target");
    assert_eq!(fragment.as_deref(), Some("section1"));
}

#[test]
fn resolve_redirect_location_fragment_overrides_original_fragment() {
    let base: Uri = "http://example.com/dir/page".parse().unwrap();
    let (result, fragment) =
        resolve_redirect(&base, "/target#newsection", Some("oldsection")).unwrap();
    assert_eq!(result.path(), "/target");
    assert_eq!(fragment.as_deref(), Some("newsection"));
}

#[test]
fn resolve_redirect_without_fragments_keeps_none() {
    let base: Uri = "http://example.com/dir/page".parse().unwrap();
    let (result, fragment) = resolve_redirect(&base, "/target", None).unwrap();
    assert_eq!(result.to_string(), "http://example.com/target");
    assert_eq!(fragment, None);
}

#[test]
fn resolve_redirect_scheme_without_authority_is_relative() {
    let base: Uri = "http://example.com/".parse().unwrap();
    let (result, _frag) = resolve_redirect(&base, "/path", None).unwrap();
    assert_eq!(result.host().unwrap(), "example.com");
}

#[test]
fn is_cacheable_method_test() {
    assert!(Method::GET == Method::GET);
}

#[test]
fn default_user_agent_contains_version() {
    assert!(DEFAULT_USER_AGENT.starts_with("aioduct/"));
}

#[test]
fn resolve_redirect_missing_scheme() {
    let base: Uri = "/relative".parse().unwrap();
    let result = resolve_redirect(&base, "/new", None);
    assert!(result.is_err());
    match result.unwrap_err() {
        Error::InvalidUrl(msg) => assert!(msg.contains("scheme")),
        other => panic!("expected InvalidUrl, got {other:?}"),
    }
}

#[test]
fn resolve_redirect_missing_authority() {
    let base = Uri::from_static("http:");
    let result = resolve_redirect(&base, "/new", None);
    assert!(result.is_err());
}
