use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use http::HeaderMap;
use http::header::{COOKIE, SET_COOKIE};

/// The `SameSite` attribute for cookies (RFC 6265bis).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SameSite {
    /// Cookie is only sent in first-party context.
    Strict,
    /// Cookie is sent with top-level navigations and third-party GET requests.
    Lax,
    /// Cookie is always sent (requires Secure).
    None,
}

/// A parsed HTTP cookie.
#[derive(Clone, Debug)]
pub struct Cookie {
    name: String,
    value: String,
    domain: Option<String>,
    path: Option<String>,
    secure: bool,
    http_only: bool,
    same_site: Option<SameSite>,
    expired: bool,
    host_only: bool,
}

impl Cookie {
    /// Returns the cookie name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the cookie value.
    pub fn value(&self) -> &str {
        &self.value
    }

    /// Returns the cookie domain, if set.
    pub fn domain(&self) -> Option<&str> {
        self.domain.as_deref()
    }

    /// Returns the cookie path, if set.
    pub fn path(&self) -> Option<&str> {
        self.path.as_deref()
    }

    /// Returns whether this cookie requires a secure (HTTPS) connection.
    pub fn secure(&self) -> bool {
        self.secure
    }

    /// Returns whether this cookie is HTTP-only (not accessible to scripts).
    pub fn http_only(&self) -> bool {
        self.http_only
    }

    /// Returns the SameSite attribute, if set.
    pub fn same_site(&self) -> Option<&SameSite> {
        self.same_site.as_ref()
    }
}

/// Thread-safe cookie storage for automatic cookie handling.
#[derive(Clone, Default)]
pub struct CookieJar {
    inner: Arc<Mutex<HashMap<String, Vec<Cookie>>>>,
}

impl std::fmt::Debug for CookieJar {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CookieJar").finish()
    }
}

impl CookieJar {
    /// Create an empty cookie jar.
    pub fn new() -> Self {
        Self::default()
    }

    /// Extract and store cookies from response `Set-Cookie` headers.
    pub fn store_from_response(&self, domain: &str, headers: &HeaderMap) {
        let Ok(mut jar) = self.inner.lock() else {
            return;
        };
        let cookies = jar.entry(domain.to_owned()).or_default();

        for value in headers.get_all(SET_COOKIE) {
            if let Ok(s) = value.to_str()
                && let Some(cookie) = parse_set_cookie(s, domain)
            {
                if cookie.expired {
                    cookies.retain(|c| {
                        !(c.name == cookie.name
                            && c.domain == cookie.domain
                            && c.path == cookie.path)
                    });
                } else if let Some(existing) = cookies.iter_mut().find(|c| {
                    c.name == cookie.name && c.domain == cookie.domain && c.path == cookie.path
                }) {
                    *existing = cookie;
                } else {
                    cookies.push(cookie);
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
        let Ok(jar) = self.inner.lock() else {
            return;
        };

        let mut matching_cookies = Vec::new();

        for (stored_domain, cookies) in jar.iter() {
            for c in cookies {
                let cookie_domain = c.domain.as_deref().unwrap_or(stored_domain);
                if c.host_only {
                    if domain != cookie_domain {
                        continue;
                    }
                } else if !domain_matches(domain, cookie_domain) {
                    continue;
                }
                if c.secure && !is_secure {
                    continue;
                }
                if let Some(p) = &c.path
                    && !path_matches(request_path, p)
                {
                    continue;
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
            if let Some(existing) = headers.get(COOKIE) {
                let merged = format!("{}; {}", existing.to_str().unwrap_or(""), cookie_header);
                if let Ok(merged_value) = merged.parse() {
                    headers.insert(COOKIE, merged_value);
                }
            } else {
                headers.insert(COOKIE, value);
            }
        }
    }

    /// Remove all stored cookies.
    pub fn clear(&self) {
        let Ok(mut inner) = self.inner.lock() else {
            return;
        };
        inner.clear();
    }

    /// Return all stored cookies.
    pub fn cookies(&self) -> Vec<Cookie> {
        let Ok(jar) = self.inner.lock() else {
            return Vec::new();
        };
        jar.values().flatten().cloned().collect()
    }
}

pub(crate) fn parse_set_cookie(header: &str, request_domain: &str) -> Option<Cookie> {
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
    let mut same_site = None;
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
        } else if lower.starts_with("path=") {
            path = Some(attr[5..].to_owned());
        } else if let Some(val) = lower.strip_prefix("samesite=") {
            same_site = match val.trim() {
                "strict" => Some(SameSite::Strict),
                "lax" => Some(SameSite::Lax),
                "none" => Some(SameSite::None),
                _ => None,
            };
        } else if let Some(val) = lower.strip_prefix("max-age=")
            && let Ok(seconds) = val.trim().parse::<i64>()
            && seconds <= 0
        {
            expired = true;
        } else if lower.starts_with("expires=") {
            let val = &attr[8..];
            if let Some(expires_time) = parse_http_date(val.trim())
                && expires_time < SystemTime::now()
            {
                expired = true;
            }
        }
    }

    let host_only = domain.is_none();

    if domain.is_none() {
        domain = Some(request_domain.to_owned());
    }

    // Cookie prefix validation (RFC 6265bis §4.1.3)
    if name.starts_with("__Host-") {
        if !secure
            || !host_only
            || domain.as_deref() != Some(request_domain)
            || path.as_deref() != Some("/")
        {
            return None;
        }
    } else if name.starts_with("__Secure-") && !secure {
        return None;
    }

    Some(Cookie {
        name,
        value,
        domain,
        path,
        secure,
        http_only,
        same_site,
        expired,
        host_only,
    })
}

fn parse_http_date(s: &str) -> Option<SystemTime> {
    parse_imf_fixdate(s)
        .or_else(|| parse_rfc850(s))
        .or_else(|| parse_asctime(s))
}

fn parse_imf_fixdate(s: &str) -> Option<SystemTime> {
    let parts: Vec<&str> = s.split_whitespace().collect();
    if parts.len() != 6 || parts[5] != "GMT" {
        return None;
    }

    let day: u64 = parts[1].parse().ok()?;
    let month = parse_month(parts[2])?;
    let year: u64 = parts[3].parse().ok()?;
    let (hour, min, sec) = parse_time(parts[4])?;

    compute_unix_time(year, month, day, hour, min, sec)
}

fn parse_rfc850(s: &str) -> Option<SystemTime> {
    let (_, rest) = s.split_once(", ")?;
    let parts: Vec<&str> = rest.split_whitespace().collect();
    if parts.len() != 3 || parts[2] != "GMT" {
        return None;
    }
    let date_parts: Vec<&str> = parts[0].split('-').collect();
    if date_parts.len() != 3 {
        return None;
    }
    let day: u64 = date_parts[0].parse().ok()?;
    let month = parse_month(date_parts[1])?;
    let mut year: u64 = date_parts[2].parse().ok()?;
    if year < 70 {
        year += 2000;
    } else if year < 100 {
        year += 1900;
    }
    let (hour, min, sec) = parse_time(parts[1])?;

    compute_unix_time(year, month, day, hour, min, sec)
}

fn parse_asctime(s: &str) -> Option<SystemTime> {
    let parts: Vec<&str> = s.split_whitespace().collect();
    if parts.len() != 5 {
        return None;
    }
    let month = parse_month(parts[1])?;
    let day: u64 = parts[2].parse().ok()?;
    let (hour, min, sec) = parse_time(parts[3])?;
    let year: u64 = parts[4].parse().ok()?;

    compute_unix_time(year, month, day, hour, min, sec)
}

fn parse_month(s: &str) -> Option<u64> {
    match s {
        "Jan" => Some(1),
        "Feb" => Some(2),
        "Mar" => Some(3),
        "Apr" => Some(4),
        "May" => Some(5),
        "Jun" => Some(6),
        "Jul" => Some(7),
        "Aug" => Some(8),
        "Sep" => Some(9),
        "Oct" => Some(10),
        "Nov" => Some(11),
        "Dec" => Some(12),
        _ => None,
    }
}

fn parse_time(s: &str) -> Option<(u64, u64, u64)> {
    let parts: Vec<&str> = s.split(':').collect();
    if parts.len() != 3 {
        return None;
    }
    Some((
        parts[0].parse().ok()?,
        parts[1].parse().ok()?,
        parts[2].parse().ok()?,
    ))
}

fn compute_unix_time(
    year: u64,
    month: u64,
    day: u64,
    hour: u64,
    min: u64,
    sec: u64,
) -> Option<SystemTime> {
    if year < 1970 {
        return Some(SystemTime::UNIX_EPOCH);
    }

    let days_before_month = [0u64, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334];
    let m = month.checked_sub(1)? as usize;
    if m >= 12 {
        return None;
    }

    let mut days = (year - 1970) * 365;
    if year > 1970 {
        days += (year - 1) / 4 - 1969 / 4;
        days -= (year - 1) / 100 - 1969 / 100;
        days += (year - 1) / 400 - 1969 / 400;
    }
    days += days_before_month[m];
    if month > 2
        && (year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400)))
    {
        days += 1;
    }
    days += day - 1;

    let unix_secs = days * 86400 + hour * 3600 + min * 60 + sec;
    Some(SystemTime::UNIX_EPOCH + Duration::from_secs(unix_secs))
}

fn path_matches(request_path: &str, cookie_path: &str) -> bool {
    if request_path == cookie_path {
        return true;
    }
    if !request_path.starts_with(cookie_path) {
        return false;
    }
    cookie_path.ends_with('/') || request_path.as_bytes().get(cookie_path.len()) == Some(&b'/')
}

fn domain_matches(request_domain: &str, cookie_domain: &str) -> bool {
    if request_domain.eq_ignore_ascii_case(cookie_domain) {
        return true;
    }
    let rd = request_domain.as_bytes();
    let cd = cookie_domain.as_bytes();
    if rd.len() <= cd.len() {
        return false;
    }
    let suffix_start = rd.len() - cd.len();
    rd[suffix_start - 1] == b'.' && rd[suffix_start..].eq_ignore_ascii_case(cd)
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
    fn host_only_cookie_not_sent_to_subdomain() {
        let jar = CookieJar::new();
        jar.store_from_response("example.com", &headers_with_cookies(&["hostonly=1"]));

        let mut req_headers = HeaderMap::new();
        jar.apply_to_request("sub.example.com", false, "/", &mut req_headers);

        assert!(
            req_headers.get(COOKIE).is_none(),
            "cookie without Domain must be scoped to the exact origin host"
        );
    }

    #[test]
    fn domain_attribute_cookie_sent_to_subdomain() {
        let jar = CookieJar::new();
        jar.store_from_response(
            "example.com",
            &headers_with_cookies(&["domain=1; Domain=example.com"]),
        );

        let mut req_headers = HeaderMap::new();
        jar.apply_to_request("sub.example.com", false, "/", &mut req_headers);

        assert_eq!(req_headers.get(COOKIE).unwrap(), "domain=1");
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
        jar.store_from_response(
            "example.com",
            &headers_with_cookies(&["k=v; Domain=example.com"]),
        );

        let mut req_headers = HeaderMap::new();
        jar.apply_to_request("sub.example.com", false, "/", &mut req_headers);
        assert_eq!(req_headers.get(COOKIE).unwrap(), "k=v");
    }

    #[test]
    fn samesite_strict_parsed() {
        let cookie = parse_set_cookie("a=b; SameSite=Strict", "example.com");
        assert_eq!(cookie.unwrap().same_site, Some(SameSite::Strict));
    }

    #[test]
    fn samesite_lax_parsed() {
        let cookie = parse_set_cookie("a=b; SameSite=Lax", "example.com");
        assert_eq!(cookie.unwrap().same_site, Some(SameSite::Lax));
    }

    #[test]
    fn samesite_none_parsed() {
        let cookie = parse_set_cookie("a=b; SameSite=None; Secure", "example.com");
        let c = cookie.unwrap();
        assert_eq!(c.same_site, Some(SameSite::None));
        assert!(c.secure);
    }

    #[test]
    fn samesite_unknown_value_ignored() {
        let cookie = parse_set_cookie("a=b; SameSite=Invalid", "example.com");
        assert_eq!(cookie.unwrap().same_site, None);
    }

    #[test]
    fn host_prefix_valid() {
        let cookie = parse_set_cookie("__Host-id=123; Secure; Path=/", "example.com");
        let c = cookie.unwrap();
        assert_eq!(c.name, "__Host-id");
        assert!(c.secure);
        assert_eq!(c.path.as_deref(), Some("/"));
    }

    #[test]
    fn host_prefix_rejected_without_secure() {
        let cookie = parse_set_cookie("__Host-id=123; Path=/", "example.com");
        assert!(cookie.is_none());
    }

    #[test]
    fn host_prefix_rejected_without_root_path() {
        let cookie = parse_set_cookie("__Host-id=123; Secure; Path=/api", "example.com");
        assert!(cookie.is_none());
    }

    #[test]
    fn host_prefix_rejected_with_domain() {
        let cookie = parse_set_cookie(
            "__Host-id=123; Secure; Path=/; Domain=other.com",
            "example.com",
        );
        assert!(cookie.is_none());
    }

    #[test]
    fn secure_prefix_valid() {
        let cookie = parse_set_cookie("__Secure-token=abc; Secure", "example.com");
        assert!(cookie.is_some());
    }

    #[test]
    fn secure_prefix_rejected_without_secure() {
        let cookie = parse_set_cookie("__Secure-token=abc", "example.com");
        assert!(cookie.is_none());
    }

    #[test]
    fn expires_past_marks_expired() {
        let cookie = parse_set_cookie(
            "sid=abc; Expires=Wed, 01 Jan 2020 00:00:00 GMT",
            "example.com",
        );
        assert!(cookie.is_some());
        assert!(cookie.unwrap().expired);
    }

    #[test]
    fn expires_future_not_expired() {
        let cookie = parse_set_cookie(
            "sid=abc; Expires=Wed, 01 Jan 2099 00:00:00 GMT",
            "example.com",
        );
        assert!(cookie.is_some());
        assert!(!cookie.unwrap().expired);
    }

    #[test]
    fn max_age_positive_not_expired() {
        let cookie = parse_set_cookie("sid=abc; Max-Age=3600", "example.com");
        assert!(cookie.is_some());
        assert!(!cookie.unwrap().expired);
    }

    #[test]
    fn max_age_negative_expired() {
        let cookie = parse_set_cookie("sid=abc; Max-Age=-1", "example.com");
        assert!(cookie.is_some());
        assert!(cookie.unwrap().expired);
    }

    #[test]
    fn cookie_jar_debug() {
        let jar = CookieJar::new();
        let dbg = format!("{jar:?}");
        assert!(dbg.contains("CookieJar"));
    }

    #[test]
    fn cookie_jar_cookies_returns_all() {
        let jar = CookieJar::new();
        let mut headers = HeaderMap::new();
        headers.append(SET_COOKIE, "a=1".parse().unwrap());
        headers.append(SET_COOKIE, "b=2".parse().unwrap());
        jar.store_from_response("example.com", &headers);
        let cookies = jar.cookies();
        assert_eq!(cookies.len(), 2);
    }

    #[test]
    fn apply_to_request_adds_cookie_header() {
        let jar = CookieJar::new();
        let mut headers = HeaderMap::new();
        headers.append(SET_COOKIE, "token=xyz".parse().unwrap());
        jar.store_from_response("example.com", &headers);

        let mut req_headers = HeaderMap::new();
        jar.apply_to_request("example.com", false, "/", &mut req_headers);
        let cookie = req_headers.get("cookie").unwrap().to_str().unwrap();
        assert!(cookie.contains("token=xyz"));
    }

    #[test]
    fn apply_to_request_no_match_no_header() {
        let jar = CookieJar::new();
        let mut headers = HeaderMap::new();
        headers.append(SET_COOKIE, "token=xyz".parse().unwrap());
        jar.store_from_response("example.com", &headers);

        let mut req_headers = HeaderMap::new();
        jar.apply_to_request("other.com", false, "/", &mut req_headers);
        assert!(req_headers.get("cookie").is_none());
    }

    #[test]
    fn secure_cookie_not_sent_on_http() {
        let jar = CookieJar::new();
        let mut headers = HeaderMap::new();
        headers.append(SET_COOKIE, "s=1; Secure".parse().unwrap());
        jar.store_from_response("example.com", &headers);

        let mut req_headers = HeaderMap::new();
        jar.apply_to_request("example.com", false, "/", &mut req_headers);
        assert!(req_headers.get("cookie").is_none());
    }

    #[test]
    fn secure_cookie_sent_on_https() {
        let jar = CookieJar::new();
        let mut headers = HeaderMap::new();
        headers.append(SET_COOKIE, "s=1; Secure".parse().unwrap());
        jar.store_from_response("example.com", &headers);

        let mut req_headers = HeaderMap::new();
        jar.apply_to_request("example.com", true, "/", &mut req_headers);
        assert!(req_headers.get("cookie").is_some());
    }

    #[test]
    fn cookie_accessors() {
        let cookie = parse_set_cookie(
            "name=value; Domain=example.com; Path=/api; Secure; HttpOnly; SameSite=Strict",
            "example.com",
        )
        .unwrap();
        assert_eq!(cookie.name(), "name");
        assert_eq!(cookie.value(), "value");
        assert_eq!(cookie.domain(), Some("example.com"));
        assert_eq!(cookie.path(), Some("/api"));
        assert!(cookie.secure());
        assert!(cookie.http_only());
        assert_eq!(cookie.same_site(), Some(&SameSite::Strict));
    }

    #[test]
    fn expires_november() {
        let cookie = parse_set_cookie(
            "sid=abc; Expires=Sun, 06 Nov 2099 08:49:37 GMT",
            "example.com",
        );
        assert!(cookie.is_some());
        assert!(!cookie.unwrap().expired);
    }

    #[test]
    fn expires_december() {
        let cookie = parse_set_cookie(
            "sid=abc; Expires=Mon, 25 Dec 2099 12:00:00 GMT",
            "example.com",
        );
        assert!(cookie.is_some());
        assert!(!cookie.unwrap().expired);
    }

    #[test]
    fn expires_invalid_month() {
        let cookie = parse_set_cookie(
            "sid=abc; Expires=Wed, 21 Xyz 2025 07:28:00 GMT",
            "example.com",
        );
        let c = cookie.unwrap();
        assert!(!c.expired);
    }

    #[test]
    fn expires_bad_time_format() {
        let cookie = parse_set_cookie("sid=abc; Expires=Wed, 21 Oct 2025 07:28 GMT", "example.com");
        let c = cookie.unwrap();
        assert!(!c.expired);
    }

    #[test]
    fn expires_leap_year_after_february() {
        let cookie = parse_set_cookie(
            "sid=abc; Expires=Fri, 01 Mar 2096 00:00:00 GMT",
            "example.com",
        );
        assert!(cookie.is_some());
        assert!(!cookie.unwrap().expired);
    }

    #[test]
    fn expired_cookie_removes_existing() {
        let jar = CookieJar::new();
        jar.store_from_response("example.com", &headers_with_cookies(&["k=v"]));
        let cookies = jar.cookies();
        assert_eq!(cookies.len(), 1);

        jar.store_from_response(
            "example.com",
            &headers_with_cookies(&["k=v; Expires=Wed, 01 Jan 2020 00:00:00 GMT"]),
        );
        let cookies = jar.cookies();
        assert_eq!(cookies.len(), 0);
    }

    #[test]
    fn parse_http_date_all_months() {
        let months = [
            "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
        ];
        for m in &months {
            let date = format!("Mon, 15 {m} 2099 10:30:00 GMT");
            let cookie = parse_set_cookie(&format!("sid=abc; Expires={date}"), "example.com");
            assert!(cookie.is_some(), "cookie with month {m} should parse");
            assert!(
                !cookie.unwrap().expired,
                "future date with month {m} should not be expired"
            );
        }
    }

    #[test]
    fn rfc850_date_format_expired() {
        let cookie = parse_set_cookie(
            "sid=abc; Expires=Sunday, 06-Nov-94 08:49:37 GMT",
            "example.com",
        );
        assert!(cookie.is_some());
        assert!(cookie.unwrap().expired, "1994 date should be expired");
    }

    #[test]
    fn rfc850_date_format_future_two_digit_year() {
        let cookie = parse_set_cookie(
            "sid=abc; Expires=Monday, 01-Jan-69 00:00:00 GMT",
            "example.com",
        );
        let c = cookie.unwrap();
        assert!(
            !c.expired,
            "two-digit year < 70 should be interpreted as 2069"
        );
    }

    #[test]
    fn asctime_date_format_expired() {
        let cookie = parse_set_cookie("sid=abc; Expires=Sun Nov  6 08:49:37 1994", "example.com");
        assert!(cookie.is_some());
        assert!(
            cookie.unwrap().expired,
            "1994 asctime date should be expired"
        );
    }

    #[test]
    fn asctime_date_format_future() {
        let cookie = parse_set_cookie("sid=abc; Expires=Thu Jan  1 00:00:00 2099", "example.com");
        let c = cookie.unwrap();
        assert!(!c.expired, "2099 asctime date should not be expired");
    }

    #[test]
    fn apply_to_request_merges_with_existing_cookie_header() {
        let jar = CookieJar::new();
        jar.store_from_response("example.com", &headers_with_cookies(&["k=v"]));

        let mut req_headers = HeaderMap::new();
        req_headers.insert(COOKIE, "existing=cookie".parse().unwrap());
        jar.apply_to_request("example.com", false, "/", &mut req_headers);
        let cookie = req_headers.get(COOKIE).unwrap().to_str().unwrap();
        assert!(
            cookie.contains("existing=cookie"),
            "should preserve existing cookie"
        );
        assert!(cookie.contains("k=v"), "should add jar cookie");
    }

    #[test]
    fn compute_unix_time_pre_1970_returns_epoch() {
        let result = compute_unix_time(1969, 12, 31, 23, 59, 59);
        assert_eq!(result, Some(SystemTime::UNIX_EPOCH));
    }

    #[test]
    fn compute_unix_time_epoch_exactly() {
        let result = compute_unix_time(1970, 1, 1, 0, 0, 0).unwrap();
        assert_eq!(result, SystemTime::UNIX_EPOCH);
    }

    #[test]
    fn compute_unix_time_invalid_month_13_returns_none() {
        // month=13 means m=12 which is >= 12, should return None (line 377)
        let result = compute_unix_time(2025, 13, 1, 0, 0, 0);
        assert_eq!(result, None, "month 13 should return None");
    }

    #[test]
    fn compute_unix_time_invalid_month_99_returns_none() {
        // month=99 means m=98 which is >= 12, should return None
        let result = compute_unix_time(2025, 99, 1, 0, 0, 0);
        assert_eq!(result, None, "month 99 should return None");
    }

    /// BUG(#67): compute_unix_time panics on month=0 due to unchecked subtraction.
    /// month=0 causes `month - 1` to underflow. Should return None instead.
    #[test]
    fn compute_unix_time_month_zero_returns_none() {
        assert_eq!(compute_unix_time(2025, 0, 1, 0, 0, 0), None);
    }

    #[test]
    fn parse_rfc850_missing_date_parts() {
        // "Sunday, 06-Nov 08:49:37 GMT" has only 2 parts in the date
        // date_parts.len() != 3 should return None (line 304)
        assert!(parse_rfc850("Sunday, 06-Nov 08:49:37 GMT").is_none());
    }

    #[test]
    fn parse_rfc850_two_digit_year_below_70() {
        // Two-digit year < 70 gets 2000 added (line 310: year += 2000)
        let result = parse_rfc850("Monday, 15-Jan-25 10:30:00 GMT");
        assert!(result.is_some(), "rfc850 with 2-digit year 25 should parse");
        // Year 25 -> 2025, which is in the past relative to 2026
        // 2025-01-15 should parse to a valid time
        let t = result.unwrap();
        assert!(t > SystemTime::UNIX_EPOCH);
    }

    #[test]
    fn parse_rfc850_two_digit_year_exactly_69() {
        // Year 69 < 70, so gets 2000 added -> 2069 (future)
        let result = parse_rfc850("Wednesday, 01-Mar-69 00:00:00 GMT");
        assert!(result.is_some());
        let t = result.unwrap();
        // 2069 is well in the future
        assert!(t > SystemTime::now());
    }

    #[test]
    fn parse_rfc850_two_digit_year_70_gets_1900() {
        // Year 70 >= 70 and < 100, so gets 1900 added -> 1970
        let result = parse_rfc850("Thursday, 01-Jan-70 00:00:01 GMT");
        assert!(result.is_some());
        let t = result.unwrap();
        // 1970-01-01 00:00:01 is UNIX_EPOCH + 1 second
        assert_eq!(t, SystemTime::UNIX_EPOCH + Duration::from_secs(1));
    }

    #[test]
    fn parse_asctime_valid_format() {
        // Standard asctime format: "Wdy Mon DD HH:MM:SS YYYY"
        let result = parse_asctime("Thu Jan  1 00:00:00 2099");
        assert!(result.is_some());
        let t = result.unwrap();
        assert!(t > SystemTime::now(), "2099 should be in the future");
    }

    #[test]
    fn parse_asctime_past_date() {
        let result = parse_asctime("Wed Jan  1 00:00:00 2020");
        assert!(result.is_some());
        let t = result.unwrap();
        assert!(t < SystemTime::now(), "2020 should be in the past");
    }

    #[test]
    fn expires_rfc850_two_digit_year_below_70_not_expired() {
        // This exercises the full path through parse_set_cookie -> parse_http_date -> parse_rfc850
        // with a two-digit year < 70 (interpreted as 2000+year)
        let cookie = parse_set_cookie(
            "sid=abc; Expires=Wednesday, 01-Mar-69 00:00:00 GMT",
            "example.com",
        );
        let c = cookie.unwrap();
        assert!(
            !c.expired,
            "year 69 should be interpreted as 2069 (not expired)"
        );
    }

    #[test]
    fn domain_matches_subdomain_not_at_dot_boundary() {
        // "fooexample.com" should NOT match "example.com"
        // because the character before "example.com" is not a dot
        assert!(!domain_matches("fooexample.com", "example.com"));
    }

    #[test]
    fn host_only_cookie_exact_match_applies() {
        // A cookie without Domain attribute (host_only=true) should match exact domain
        let jar = CookieJar::new();
        jar.store_from_response("exact.example.com", &headers_with_cookies(&["h=1"]));

        let mut req_headers = HeaderMap::new();
        jar.apply_to_request("exact.example.com", false, "/", &mut req_headers);
        assert_eq!(req_headers.get(COOKIE).unwrap(), "h=1");
    }

    #[test]
    fn domain_cookie_does_not_apply_to_unrelated() {
        // A cookie with Domain=example.com should NOT match totally-different.com
        let jar = CookieJar::new();
        jar.store_from_response(
            "example.com",
            &headers_with_cookies(&["k=v; Domain=example.com"]),
        );

        let mut req_headers = HeaderMap::new();
        jar.apply_to_request("totally-different.com", false, "/", &mut req_headers);
        assert!(req_headers.get(COOKIE).is_none());
    }

    #[test]
    fn cookies_accessor_returns_cloned_cookies() {
        let jar = CookieJar::new();
        jar.store_from_response(
            "example.com",
            &headers_with_cookies(&["name=value; Secure; HttpOnly; Path=/api"]),
        );
        let cookies = jar.cookies();
        assert_eq!(cookies.len(), 1);
        let c = &cookies[0];
        assert_eq!(c.name(), "name");
        assert_eq!(c.value(), "value");
        assert!(c.secure());
        assert!(c.http_only());
        assert_eq!(c.path(), Some("/api"));
    }

    #[test]
    fn clear_on_empty_jar_is_noop() {
        let jar = CookieJar::new();
        jar.clear();
        assert!(jar.cookies().is_empty());
    }

    #[test]
    fn parse_rfc850_invalid_format() {
        assert!(parse_rfc850("not a date").is_none());
        assert!(parse_rfc850("Sunday, 06-Nov-94 08:49:37 EST").is_none());
        assert!(parse_rfc850("Sunday, 06-Nov 08:49:37 GMT").is_none());
    }

    #[test]
    fn parse_asctime_invalid_format() {
        assert!(parse_asctime("not a date").is_none());
        assert!(parse_asctime("Sun Nov").is_none());
    }

    #[test]
    fn path_matches_trailing_slash() {
        assert!(path_matches("/api/v1", "/api/"));
        assert!(!path_matches("/api-v2", "/api/"));
    }

    #[test]
    fn path_matches_exact_with_subpath() {
        assert!(path_matches("/foo/bar", "/foo"));
        assert!(!path_matches("/foobar", "/foo"));
    }
}
