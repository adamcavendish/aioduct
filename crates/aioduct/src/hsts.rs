use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use http::HeaderMap;
use http::header::HeaderValue;

#[derive(Clone, Debug)]
struct HstsEntry {
    include_subdomains: bool,
    expires_at: Instant,
}

/// Thread-safe HTTP Strict Transport Security store.
///
/// Parses `Strict-Transport-Security` response headers and remembers which
/// hosts require HTTPS. Call [`should_upgrade`](Self::should_upgrade) before
/// connecting to check whether an `http://` URL should be upgraded to
/// `https://`.
#[derive(Clone, Default)]
pub struct HstsStore {
    inner: Arc<Mutex<HashMap<String, HstsEntry>>>,
}

impl std::fmt::Debug for HstsStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HstsStore").finish()
    }
}

impl HstsStore {
    /// Create an empty HSTS store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Process a `Strict-Transport-Security` header from a response.
    ///
    /// Per RFC 6797, HSTS headers must only be processed when received over
    /// a secure (HTTPS) connection. The caller must enforce this.
    pub fn store_from_response(&self, host: &str, headers: &HeaderMap) {
        if let Some(value) = headers.get("strict-transport-security")
            && let Some((max_age, include_subdomains)) = parse_hsts(value)
        {
            let mut store = self.inner.lock().unwrap();
            if max_age.is_zero() {
                store.remove(host);
            } else {
                store.insert(
                    host.to_owned(),
                    HstsEntry {
                        include_subdomains,
                        expires_at: Instant::now() + max_age,
                    },
                );
            }
        }
    }

    /// Check whether a host should be upgraded from HTTP to HTTPS.
    pub fn should_upgrade(&self, host: &str) -> bool {
        let store = self.inner.lock().unwrap();

        if let Some(entry) = store.get(host)
            && Instant::now() < entry.expires_at
        {
            return true;
        }

        // Check parent domains for includeSubDomains
        let mut domain = host;
        while let Some(dot_pos) = domain.find('.') {
            domain = &domain[dot_pos + 1..];
            if let Some(entry) = store.get(domain)
                && entry.include_subdomains
                && Instant::now() < entry.expires_at
            {
                return true;
            }
        }

        false
    }

    /// Remove all stored HSTS entries.
    pub fn clear(&self) {
        self.inner.lock().unwrap().clear();
    }
}

fn parse_hsts(value: &HeaderValue) -> Option<(Duration, bool)> {
    let s = value.to_str().ok()?;
    let mut max_age = None;
    let mut include_subdomains = false;

    for part in s.split(';') {
        let part = part.trim().to_lowercase();
        if let Some(age_str) = part.strip_prefix("max-age=") {
            if let Ok(secs) = age_str.trim().parse::<u64>() {
                max_age = Some(Duration::from_secs(secs));
            }
        } else if part == "includesubdomains" {
            include_subdomains = true;
        }
    }

    max_age.map(|ma| (ma, include_subdomains))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hsts_headers(value: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert("strict-transport-security", value.parse().unwrap());
        headers
    }

    #[test]
    fn basic_hsts_store_and_upgrade() {
        let store = HstsStore::new();
        let headers = hsts_headers("max-age=31536000");
        store.store_from_response("example.com", &headers);
        assert!(store.should_upgrade("example.com"));
        assert!(!store.should_upgrade("other.com"));
    }

    #[test]
    fn include_subdomains() {
        let store = HstsStore::new();
        let headers = hsts_headers("max-age=31536000; includeSubDomains");
        store.store_from_response("example.com", &headers);
        assert!(store.should_upgrade("example.com"));
        assert!(store.should_upgrade("sub.example.com"));
        assert!(store.should_upgrade("deep.sub.example.com"));
        assert!(!store.should_upgrade("notexample.com"));
    }

    #[test]
    fn subdomain_not_upgraded_without_flag() {
        let store = HstsStore::new();
        let headers = hsts_headers("max-age=31536000");
        store.store_from_response("example.com", &headers);
        assert!(!store.should_upgrade("sub.example.com"));
    }

    #[test]
    fn max_age_zero_removes_entry() {
        let store = HstsStore::new();
        store.store_from_response("example.com", &hsts_headers("max-age=3600"));
        assert!(store.should_upgrade("example.com"));
        store.store_from_response("example.com", &hsts_headers("max-age=0"));
        assert!(!store.should_upgrade("example.com"));
    }

    #[test]
    fn missing_header_no_op() {
        let store = HstsStore::new();
        let headers = HeaderMap::new();
        store.store_from_response("example.com", &headers);
        assert!(!store.should_upgrade("example.com"));
    }

    #[test]
    fn invalid_header_no_op() {
        let store = HstsStore::new();
        let headers = hsts_headers("invalid");
        store.store_from_response("example.com", &headers);
        assert!(!store.should_upgrade("example.com"));
    }

    #[test]
    fn clear_removes_all() {
        let store = HstsStore::new();
        store.store_from_response("a.com", &hsts_headers("max-age=3600"));
        store.store_from_response("b.com", &hsts_headers("max-age=3600"));
        store.clear();
        assert!(!store.should_upgrade("a.com"));
        assert!(!store.should_upgrade("b.com"));
    }

    #[test]
    fn clone_shares_state() {
        let store = HstsStore::new();
        let store2 = store.clone();
        store.store_from_response("example.com", &hsts_headers("max-age=3600"));
        assert!(store2.should_upgrade("example.com"));
    }
}
