use std::time::{Duration, SystemTime};

use super::date::parse_http_date;
use super::matching::{default_cookie_path, domain_matches};
use super::{Cookie, SameSite};

pub(crate) fn parse_set_cookie(
    header: &str,
    request_domain: &str,
    request_path: &str,
) -> Option<Cookie> {
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
    let mut expires_at = None;

    for attr in parts {
        let attr = attr.trim();
        let lower = attr.to_lowercase();

        if lower == "secure" {
            secure = true;
        } else if lower == "httponly" {
            http_only = true;
        } else if let Some(val) = lower.strip_prefix("domain=") {
            let d = val.trim_start_matches('.').to_owned();
            if !domain_matches(request_domain, &d) {
                return None;
            }
            domain = Some(d);
        } else if lower.starts_with("path=") {
            path = Some(attr[5..].to_owned());
        } else if let Some(val) = lower.strip_prefix("samesite=") {
            same_site = match val.trim() {
                "strict" => Some(SameSite::Strict),
                "lax" => Some(SameSite::Lax),
                "none" => Some(SameSite::None),
                _ => None,
            };
        } else if let Some(val) = lower.strip_prefix("max-age=") {
            if let Ok(seconds) = val.trim().parse::<i64>() {
                if seconds <= 0 {
                    expired = true;
                    expires_at = None;
                } else {
                    expires_at = Some(SystemTime::now() + Duration::from_secs(seconds as u64));
                }
            }
        } else if lower.starts_with("expires=") && expires_at.is_none() && !expired {
            let val = &attr[8..];
            if let Some(expires_time) = parse_http_date(val.trim()) {
                if expires_time < SystemTime::now() {
                    expired = true;
                } else {
                    expires_at = Some(expires_time);
                }
            }
        }
    }

    let path = path.unwrap_or_else(|| default_cookie_path(request_path));

    let host_only = domain.is_none();

    if domain.is_none() {
        domain = Some(request_domain.to_owned());
    }

    // Cookie prefix validation (RFC 6265bis §4.1.3)
    if name.starts_with("__Host-") {
        if !secure || !host_only || domain.as_deref() != Some(request_domain) || path != "/" {
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
        expires_at,
        host_only,
    })
}
