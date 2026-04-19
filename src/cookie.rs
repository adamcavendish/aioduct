use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use http::HeaderMap;
use http::header::{COOKIE, SET_COOKIE};

#[derive(Clone, Debug)]
pub struct Cookie {
    name: String,
    value: String,
    domain: Option<String>,
    path: Option<String>,
    secure: bool,
    http_only: bool,
    expired: bool,
}

impl Cookie {
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn value(&self) -> &str {
        &self.value
    }

    pub fn domain(&self) -> Option<&str> {
        self.domain.as_deref()
    }

    pub fn path(&self) -> Option<&str> {
        self.path.as_deref()
    }

    pub fn secure(&self) -> bool {
        self.secure
    }

    pub fn http_only(&self) -> bool {
        self.http_only
    }
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
                    if cookie.expired {
                        cookies.retain(|c| c.name != cookie.name);
                    } else if let Some(existing) =
                        cookies.iter_mut().find(|c| c.name == cookie.name)
                    {
                        *existing = cookie;
                    } else {
                        cookies.push(cookie);
                    }
                }
            }
        }
    }

    /// Add stored cookies to outgoing request headers.
    pub fn apply_to_request(
        &self,
        domain: &str,
        is_secure: bool,
        request_path: &str,
        headers: &mut HeaderMap,
    ) {
        let jar = self.inner.lock().unwrap();

        let mut matching_cookies = Vec::new();

        for (stored_domain, cookies) in jar.iter() {
            for c in cookies {
                let cookie_domain = c.domain.as_deref().unwrap_or(stored_domain);
                if !domain_matches(domain, cookie_domain) {
                    continue;
                }
                if c.secure && !is_secure {
                    continue;
                }
                if let Some(p) = &c.path {
                    if !request_path.starts_with(p.as_str()) {
                        continue;
                    }
                }
                matching_cookies.push(c);
            }
        }

        if matching_cookies.is_empty() {
            return;
        }

        let cookie_header: String = matching_cookies
            .iter()
            .map(|c| format!("{}={}", c.name, c.value))
            .collect::<Vec<_>>()
            .join("; ");

        if let Ok(value) = cookie_header.parse() {
            headers.insert(COOKIE, value);
        }
    }

    /// Remove all stored cookies.
    pub fn clear(&self) {
        self.inner.lock().unwrap().clear();
    }

    /// Return all stored cookies.
    pub fn cookies(&self) -> Vec<Cookie> {
        let jar = self.inner.lock().unwrap();
        jar.values().flatten().cloned().collect()
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
    let mut expired = false;

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
        } else if let Some(val) = lower.strip_prefix("max-age=") {
            if let Ok(seconds) = val.trim().parse::<i64>() {
                if seconds <= 0 {
                    expired = true;
                }
            }
        } else if let Some(val) = attr
            .strip_prefix("Expires=")
            .or_else(|| attr.strip_prefix("expires="))
        {
            if let Some(expires_time) = parse_http_date(val.trim()) {
                if expires_time < SystemTime::now() {
                    expired = true;
                }
            }
        }
    }

    if domain.is_none() {
        domain = Some(request_domain.to_owned());
    }

    Some(Cookie {
        name,
        value,
        domain,
        path,
        secure,
        http_only,
        expired,
    })
}

fn parse_http_date(s: &str) -> Option<SystemTime> {
    // Parse "Wed, 21 Oct 2015 07:28:00 GMT" (RFC 7231 preferred format)
    let parts: Vec<&str> = s.split_whitespace().collect();
    if parts.len() != 6 || parts[5] != "GMT" {
        return None;
    }

    let day: u64 = parts[1].parse().ok()?;
    let month = match parts[2] {
        "Jan" => 1u64,
        "Feb" => 2,
        "Mar" => 3,
        "Apr" => 4,
        "May" => 5,
        "Jun" => 6,
        "Jul" => 7,
        "Aug" => 8,
        "Sep" => 9,
        "Oct" => 10,
        "Nov" => 11,
        "Dec" => 12,
        _ => return None,
    };
    let year: u64 = parts[3].parse().ok()?;
    let time_parts: Vec<&str> = parts[4].split(':').collect();
    if time_parts.len() != 3 {
        return None;
    }
    let hour: u64 = time_parts[0].parse().ok()?;
    let min: u64 = time_parts[1].parse().ok()?;
    let sec: u64 = time_parts[2].parse().ok()?;

    // Days before each month (non-leap year)
    let days_before_month = [0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334];
    let m = (month - 1) as usize;
    if m >= 12 {
        return None;
    }

    let mut days = (year - 1970) * 365;
    // Add leap days
    if year > 1970 {
        days += (year - 1) / 4 - 1969 / 4;
        days -= (year - 1) / 100 - 1969 / 100;
        days += (year - 1) / 400 - 1969 / 400;
    }
    days += days_before_month[m];
    if month > 2 && (year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)) {
        days += 1;
    }
    days += day - 1;

    let unix_secs = days * 86400 + hour * 3600 + min * 60 + sec;
    Some(SystemTime::UNIX_EPOCH + Duration::from_secs(unix_secs))
}

fn domain_matches(request_domain: &str, cookie_domain: &str) -> bool {
    if request_domain == cookie_domain {
        return true;
    }
    request_domain.ends_with(cookie_domain)
        && request_domain.as_bytes()[request_domain.len() - cookie_domain.len() - 1] == b'.'
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
        jar.apply_to_request("example.com", false, "/", &mut req_headers);
        assert_eq!(req_headers.get(COOKIE).unwrap(), "foo=bar");
    }

    #[test]
    fn multiple_cookies_joined() {
        let jar = CookieJar::new();
        let headers = headers_with_cookies(&["a=1", "b=2"]);
        jar.store_from_response("example.com", &headers);

        let mut req_headers = HeaderMap::new();
        jar.apply_to_request("example.com", false, "/", &mut req_headers);
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
        jar.apply_to_request("example.com", false, "/", &mut req_headers);
        assert_eq!(req_headers.get(COOKIE).unwrap(), "k=new");
    }

    #[test]
    fn secure_cookie_excluded_on_insecure() {
        let jar = CookieJar::new();
        let headers = headers_with_cookies(&["s=secret; Secure"]);
        jar.store_from_response("example.com", &headers);

        let mut req_headers = HeaderMap::new();
        jar.apply_to_request("example.com", false, "/", &mut req_headers);
        assert!(req_headers.get(COOKIE).is_none());
    }

    #[test]
    fn secure_cookie_included_on_secure() {
        let jar = CookieJar::new();
        let headers = headers_with_cookies(&["s=secret; Secure"]);
        jar.store_from_response("example.com", &headers);

        let mut req_headers = HeaderMap::new();
        jar.apply_to_request("example.com", true, "/", &mut req_headers);
        assert_eq!(req_headers.get(COOKIE).unwrap(), "s=secret");
    }

    #[test]
    fn clear_empties_jar() {
        let jar = CookieJar::new();
        jar.store_from_response("example.com", &headers_with_cookies(&["x=y"]));
        jar.clear();

        let mut req_headers = HeaderMap::new();
        jar.apply_to_request("example.com", false, "/", &mut req_headers);
        assert!(req_headers.get(COOKIE).is_none());
    }

    #[test]
    fn empty_name_ignored() {
        let jar = CookieJar::new();
        let headers = headers_with_cookies(&["=value"]);
        jar.store_from_response("example.com", &headers);

        let mut req_headers = HeaderMap::new();
        jar.apply_to_request("example.com", false, "/", &mut req_headers);
        assert!(req_headers.get(COOKIE).is_none());
    }

    #[test]
    fn domain_attribute_with_leading_dot_stripped() {
        let cookie = parse_set_cookie("a=b; Domain=.foo.com", "bar.com");
        let c = cookie.unwrap();
        assert_eq!(c.domain.as_deref(), Some("foo.com"));
    }

    #[test]
    fn domain_defaults_to_request_domain() {
        let cookie = parse_set_cookie("a=b", "request.com");
        let c = cookie.unwrap();
        assert_eq!(c.domain.as_deref(), Some("request.com"));
    }

    #[test]
    fn httponly_attribute_parsed() {
        let cookie = parse_set_cookie("a=b; HttpOnly", "example.com");
        assert!(cookie.unwrap().http_only);
    }

    #[test]
    fn path_attribute_parsed() {
        let cookie = parse_set_cookie("a=b; Path=/api", "example.com");
        assert_eq!(cookie.unwrap().path.as_deref(), Some("/api"));
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
        jar.apply_to_request("b.com", false, "/", &mut req_headers);
        assert!(req_headers.get(COOKIE).is_none());
    }

    #[test]
    fn clone_shares_state() {
        let jar = CookieJar::new();
        let jar2 = jar.clone();
        jar.store_from_response("example.com", &headers_with_cookies(&["x=1"]));

        let mut req_headers = HeaderMap::new();
        jar2.apply_to_request("example.com", false, "/", &mut req_headers);
        assert_eq!(req_headers.get(COOKIE).unwrap(), "x=1");
    }

    #[test]
    fn max_age_zero_removes_cookie() {
        let jar = CookieJar::new();
        jar.store_from_response("example.com", &headers_with_cookies(&["k=v"]));
        jar.store_from_response("example.com", &headers_with_cookies(&["k=v; Max-Age=0"]));

        let mut req_headers = HeaderMap::new();
        jar.apply_to_request("example.com", false, "/", &mut req_headers);
        assert!(req_headers.get(COOKIE).is_none());
    }

    #[test]
    fn max_age_zero_not_stored() {
        let jar = CookieJar::new();
        jar.store_from_response("example.com", &headers_with_cookies(&["k=v; Max-Age=0"]));

        let mut req_headers = HeaderMap::new();
        jar.apply_to_request("example.com", false, "/", &mut req_headers);
        assert!(req_headers.get(COOKIE).is_none());
    }

    #[test]
    fn path_scoping() {
        let jar = CookieJar::new();
        jar.store_from_response("example.com", &headers_with_cookies(&["k=v; Path=/api"]));

        let mut req_headers = HeaderMap::new();
        jar.apply_to_request("example.com", false, "/", &mut req_headers);
        assert!(req_headers.get(COOKIE).is_none());

        let mut req_headers = HeaderMap::new();
        jar.apply_to_request("example.com", false, "/api", &mut req_headers);
        assert_eq!(req_headers.get(COOKIE).unwrap(), "k=v");

        let mut req_headers = HeaderMap::new();
        jar.apply_to_request("example.com", false, "/api/sub", &mut req_headers);
        assert_eq!(req_headers.get(COOKIE).unwrap(), "k=v");
    }

    #[test]
    fn domain_matching_exact() {
        assert!(domain_matches("example.com", "example.com"));
    }

    #[test]
    fn domain_matching_subdomain() {
        assert!(domain_matches("sub.example.com", "example.com"));
    }

    #[test]
    fn domain_matching_no_partial() {
        assert!(!domain_matches("notexample.com", "example.com"));
    }

    #[test]
    fn domain_matching_different() {
        assert!(!domain_matches("other.com", "example.com"));
    }

    #[test]
    fn subdomain_cookie_applied() {
        let jar = CookieJar::new();
        jar.store_from_response("example.com", &headers_with_cookies(&["k=v"]));

        let mut req_headers = HeaderMap::new();
        jar.apply_to_request("sub.example.com", false, "/", &mut req_headers);
        assert_eq!(req_headers.get(COOKIE).unwrap(), "k=v");
    }
}
