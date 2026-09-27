use http::header::{ETAG, EXPIRES, LAST_MODIFIED};
use http::{HeaderMap, Method, StatusCode};

use super::CacheEntry;
use super::headers::parse_cache_control;

pub(crate) fn is_cacheable_method(method: &Method) -> bool {
    *method == Method::GET || *method == Method::HEAD
}

pub(super) fn is_cacheable_status(status: StatusCode) -> bool {
    matches!(
        status.as_u16(),
        200 | 203 | 204 | 206 | 300 | 301 | 308 | 404 | 405 | 410 | 414 | 501
    )
}

pub(super) fn is_unsafe_method(method: &Method) -> bool {
    !matches!(
        *method,
        Method::GET | Method::HEAD | Method::OPTIONS | Method::TRACE
    )
}

// Cache lookup precedes connection selection. A missing Host in that snapshot
// may still become an automatic Host on HTTP/1, so it is not a known absence.
pub(super) fn has_unresolved_host(vary: &[String], request_headers: &HeaderMap) -> bool {
    !request_headers.contains_key(http::header::HOST)
        && vary.iter().any(|name| name.eq_ignore_ascii_case("host"))
}

pub(crate) fn vary_matches(entry: &CacheEntry, request_headers: &HeaderMap) -> bool {
    let Some(ref vary_names) = entry.vary else {
        return true;
    };
    if has_unresolved_host(vary_names, request_headers) {
        return false;
    }
    let Some(ref stored) = entry.request_vary_headers else {
        return true;
    };
    for (name, stored_val) in stored {
        if name == "*" {
            return false;
        }
        let current_val = http::header::HeaderName::from_bytes(name.as_bytes())
            .ok()
            .and_then(|hn| request_headers.get(&hn))
            .and_then(|v| v.to_str().ok())
            .map(String::from);
        if current_val != *stored_val {
            return false;
        }
    }
    true
}

pub(crate) fn is_response_cacheable(status: StatusCode, headers: &HeaderMap) -> bool {
    if !is_cacheable_status(status) {
        return false;
    }
    let directives = parse_cache_control(headers);
    if directives.no_store || directives.private {
        return false;
    }
    directives.max_age.is_some()
        || headers.contains_key(EXPIRES)
        || headers.contains_key(ETAG)
        || headers.contains_key(LAST_MODIFIED)
}
