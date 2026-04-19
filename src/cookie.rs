use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use http::HeaderMap;
use http::header::{COOKIE, SET_COOKIE};

#[derive(Clone, Debug)]
pub(crate) struct Cookie {
    pub(crate) name: String,
    pub(crate) value: String,
    pub(crate) _domain: Option<String>,
    pub(crate) _path: Option<String>,
    pub(crate) secure: bool,
    pub(crate) _http_only: bool,
}

/// Thread-safe cookie storage for automatic cookie handling.
#[derive(Clone, Default)]
pub struct CookieJar {
    inner: Arc<Mutex<HashMap<String, Vec<Cookie>>>>,
}

impl CookieJar {
    /// Create an empty cookie jar.
    pub fn new() -> Self {
        Self::default()
    }

    /// Extract and store cookies from response `Set-Cookie` headers.
    pub fn store_from_response(&self, domain: &str, headers: &HeaderMap) {
        let mut jar = self.inner.lock().unwrap();
        let cookies = jar.entry(domain.to_owned()).or_default();

        for value in headers.get_all(SET_COOKIE) {
            if let Ok(s) = value.to_str() {
                if let Some(cookie) = parse_set_cookie(s, domain) {
                    if let Some(existing) = cookies.iter_mut().find(|c| c.name == cookie.name) {
                        *existing = cookie;
                    } else {
                        cookies.push(cookie);
                    }
                }
            }
        }
    }

    /// Add stored cookies to outgoing request headers.
    pub fn apply_to_request(&self, domain: &str, is_secure: bool, headers: &mut HeaderMap) {
        let jar = self.inner.lock().unwrap();

        if let Some(cookies) = jar.get(domain) {
            let matching: Vec<&Cookie> =
                cookies.iter().filter(|c| !c.secure || is_secure).collect();

            if matching.is_empty() {
                return;
            }

            let cookie_header: String = matching
                .iter()
                .map(|c| format!("{}={}", c.name, c.value))
                .collect::<Vec<_>>()
                .join("; ");

            if let Ok(value) = cookie_header.parse() {
                headers.insert(COOKIE, value);
            }
        }
    }

    /// Remove all stored cookies.
    pub fn clear(&self) {
        self.inner.lock().unwrap().clear();
    }
}

fn parse_set_cookie(header: &str, request_domain: &str) -> Option<Cookie> {
    let mut parts = header.split(';');
    let name_value = parts.next()?.trim();
    let (name, value) = name_value.split_once('=')?;

    let name = name.trim().to_owned();
    let value = value.trim().to_owned();

    if name.is_empty() {
        return None;
    }

    let mut domain = None;
    let mut path = None;
    let mut secure = false;
    let mut http_only = false;

    for attr in parts {
        let attr = attr.trim();
        let lower = attr.to_lowercase();

        if lower == "secure" {
            secure = true;
        } else if lower == "httponly" {
            http_only = true;
        } else if let Some(val) = lower.strip_prefix("domain=") {
            domain = Some(val.trim_start_matches('.').to_owned());
        } else if let Some(val) = lower.strip_prefix("path=") {
            path = Some(val.to_owned());
        }
    }

    if domain.is_none() {
        domain = Some(request_domain.to_owned());
    }

    Some(Cookie {
        name,
        value,
        _domain: domain,
        _path: path,
        secure,
        _http_only: http_only,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use http::header::HeaderValue;

    fn headers_with_cookies(cookies: &[&str]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for c in cookies {
            headers.append(SET_COOKIE, HeaderValue::from_str(c).unwrap());
        }
        headers
    }

    #[test]
    fn store_and_apply_roundtrip() {
        let jar = CookieJar::new();
        let headers = headers_with_cookies(&["foo=bar"]);
        jar.store_from_response("example.com", &headers);

        let mut req_headers = HeaderMap::new();
        jar.apply_to_request("example.com", false, &mut req_headers);
        assert_eq!(req_headers.get(COOKIE).unwrap(), "foo=bar");
    }

    #[test]
    fn multiple_cookies_joined() {
        let jar = CookieJar::new();
        let headers = headers_with_cookies(&["a=1", "b=2"]);
        jar.store_from_response("example.com", &headers);

        let mut req_headers = HeaderMap::new();
        jar.apply_to_request("example.com", false, &mut req_headers);
        let cookie_str = req_headers.get(COOKIE).unwrap().to_str().unwrap();
        assert!(cookie_str.contains("a=1"));
        assert!(cookie_str.contains("b=2"));
        assert!(cookie_str.contains("; "));
    }

    #[test]
    fn cookie_update_overwrites_existing() {
        let jar = CookieJar::new();
        jar.store_from_response("example.com", &headers_with_cookies(&["k=old"]));
        jar.store_from_response("example.com", &headers_with_cookies(&["k=new"]));

        let mut req_headers = HeaderMap::new();
        jar.apply_to_request("example.com", false, &mut req_headers);
        assert_eq!(req_headers.get(COOKIE).unwrap(), "k=new");
    }

    #[test]
    fn secure_cookie_excluded_on_insecure() {
        let jar = CookieJar::new();
        let headers = headers_with_cookies(&["s=secret; Secure"]);
        jar.store_from_response("example.com", &headers);

        let mut req_headers = HeaderMap::new();
        jar.apply_to_request("example.com", false, &mut req_headers);
        assert!(req_headers.get(COOKIE).is_none());
    }

    #[test]
    fn secure_cookie_included_on_secure() {
        let jar = CookieJar::new();
        let headers = headers_with_cookies(&["s=secret; Secure"]);
        jar.store_from_response("example.com", &headers);

        let mut req_headers = HeaderMap::new();
        jar.apply_to_request("example.com", true, &mut req_headers);
        assert_eq!(req_headers.get(COOKIE).unwrap(), "s=secret");
    }

    #[test]
    fn clear_empties_jar() {
        let jar = CookieJar::new();
        jar.store_from_response("example.com", &headers_with_cookies(&["x=y"]));
        jar.clear();

        let mut req_headers = HeaderMap::new();
        jar.apply_to_request("example.com", false, &mut req_headers);
        assert!(req_headers.get(COOKIE).is_none());
    }

    #[test]
    fn empty_name_ignored() {
        let jar = CookieJar::new();
        let headers = headers_with_cookies(&["=value"]);
        jar.store_from_response("example.com", &headers);

        let mut req_headers = HeaderMap::new();
        jar.apply_to_request("example.com", false, &mut req_headers);
        assert!(req_headers.get(COOKIE).is_none());
    }

    #[test]
    fn domain_attribute_with_leading_dot_stripped() {
        let cookie = parse_set_cookie("a=b; Domain=.foo.com", "bar.com");
        let c = cookie.unwrap();
        assert_eq!(c._domain.as_deref(), Some("foo.com"));
    }

    #[test]
    fn domain_defaults_to_request_domain() {
        let cookie = parse_set_cookie("a=b", "request.com");
        let c = cookie.unwrap();
        assert_eq!(c._domain.as_deref(), Some("request.com"));
    }

    #[test]
    fn httponly_attribute_parsed() {
        let cookie = parse_set_cookie("a=b; HttpOnly", "example.com");
        assert!(cookie.unwrap()._http_only);
    }

    #[test]
    fn path_attribute_parsed() {
        let cookie = parse_set_cookie("a=b; Path=/api", "example.com");
        assert_eq!(cookie.unwrap()._path.as_deref(), Some("/api"));
    }

    #[test]
    fn no_equals_returns_none() {
        assert!(parse_set_cookie("invalid", "example.com").is_none());
    }

    #[test]
    fn different_domain_does_not_apply() {
        let jar = CookieJar::new();
        jar.store_from_response("a.com", &headers_with_cookies(&["x=1"]));

        let mut req_headers = HeaderMap::new();
        jar.apply_to_request("b.com", false, &mut req_headers);
        assert!(req_headers.get(COOKIE).is_none());
    }

    #[test]
    fn clone_shares_state() {
        let jar = CookieJar::new();
        let jar2 = jar.clone();
        jar.store_from_response("example.com", &headers_with_cookies(&["x=1"]));

        let mut req_headers = HeaderMap::new();
        jar2.apply_to_request("example.com", false, &mut req_headers);
        assert_eq!(req_headers.get(COOKIE).unwrap(), "x=1");
    }
}
