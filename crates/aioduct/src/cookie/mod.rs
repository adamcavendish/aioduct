use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use http::HeaderMap;
use http::header::{COOKIE, SET_COOKIE};

mod date;
mod matching;
mod parse;

use matching::{domain_matches, is_same_site, path_matches};
pub(crate) use parse::parse_set_cookie;

#[cfg(test)]
use date::{compute_unix_time, parse_asctime, parse_rfc850};
#[cfg(test)]
use matching::default_cookie_path;

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
    path: String,
    secure: bool,
    http_only: bool,
    same_site: Option<SameSite>,
    expired: bool,
    expires_at: Option<SystemTime>,
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

    /// Returns the cookie path.
    pub fn path(&self) -> &str {
        &self.path
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
    pub fn store_from_response(&self, domain: &str, request_path: &str, headers: &HeaderMap) {
        let Ok(mut jar) = self.inner.lock() else {
            return;
        };

        for value in headers.get_all(SET_COOKIE) {
            if let Ok(s) = value.to_str()
                && let Some(cookie) = parse_set_cookie(s, domain, request_path)
            {
                let effective_domain = cookie.domain.as_deref().unwrap_or(domain).to_owned();
                let cookies = jar.entry(effective_domain).or_default();

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
    ///
    /// `site_for_cookies` is the host of the site that initiated the request
    /// (e.g. the original URL before redirects). When `Some`, SameSite enforcement
    /// is applied: `Strict`, `Lax`, and cookies without a SameSite attribute
    /// (defaulting to Lax per RFC 6265bis) are excluded on cross-site requests.
    /// Only `SameSite=None` cookies pass through. When `None`, SameSite is not
    /// enforced (useful for first-party same-site requests or backward compat).
    ///
    /// Note: this implementation treats ALL cross-site requests as "unsafe" — there
    /// is no concept of top-level navigation, so `SameSite=Lax` cookies are never
    /// sent cross-site. Same-site detection uses a simplified eTLD+1 heuristic that
    /// does not consult the public suffix list.
    pub fn apply_to_request(
        &self,
        domain: &str,
        is_secure: bool,
        request_path: &str,
        site_for_cookies: Option<&str>,
        headers: &mut HeaderMap,
    ) {
        let Ok(jar) = self.inner.lock() else {
            return;
        };

        let is_cross_site = site_for_cookies.is_some_and(|site| !is_same_site(domain, site));

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
                if !path_matches(request_path, &c.path) {
                    continue;
                }
                if let Some(exp) = c.expires_at
                    && exp <= SystemTime::now()
                {
                    continue;
                }
                if is_cross_site {
                    match c.same_site.as_ref() {
                        Some(SameSite::None) => {}
                        _ => continue,
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

#[cfg(test)]
mod tests;
