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
        let Some(host) = canonical_host(host) else {
            return;
        };
        if let Some(value) = headers.get("strict-transport-security")
            && let Some((max_age, include_subdomains)) = parse_hsts(value)
        {
            let Ok(mut store) = self.inner.lock() else {
                return;
            };
            if max_age.is_zero() {
                store.remove(&host);
            } else {
                let now = Instant::now();
                if store.len() >= 1024 {
                    store.retain(|_, e| now < e.expires_at);
                }
                if store.len() >= 1024
                    && let Some(oldest_key) = store
                        .iter()
                        .min_by_key(|(_, e)| e.expires_at)
                        .map(|(k, _)| k.clone())
                {
                    store.remove(&oldest_key);
                }
                store.insert(
                    host,
                    HstsEntry {
                        include_subdomains,
                        expires_at: now + max_age,
                    },
                );
            }
        }
    }

    /// Check whether a host should be upgraded from HTTP to HTTPS.
    pub fn should_upgrade(&self, host: &str) -> bool {
        let Some(host) = canonical_host(host) else {
            return false;
        };
        let Ok(store) = self.inner.lock() else {
            return false;
        };

        if let Some(entry) = store.get(&host)
            && Instant::now() < entry.expires_at
        {
            return true;
        }

        // Check parent domains for includeSubDomains
        let mut domain = host.as_str();
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
        let Ok(mut inner) = self.inner.lock() else {
            return;
        };
        inner.clear();
    }
}

fn canonical_host(host: &str) -> Option<String> {
    let mut host = host.trim();
    if host.is_empty() {
        return None;
    }

    if let Some(rest) = host.strip_prefix('[') {
        let end = rest.find(']')?;
        let (addr, suffix) = rest.split_at(end);
        let suffix = &suffix[1..];
        if !suffix.is_empty() {
            let port = suffix.strip_prefix(':')?;
            if port.parse::<u16>().is_err() {
                return None;
            }
        }
        host = addr;
    } else if host.matches(':').count() == 1
        && let Some((name, port)) = host.rsplit_once(':')
        && !name.is_empty()
        && port.parse::<u16>().is_ok()
    {
        host = name;
    }

    let host = host.trim_end_matches('.').to_ascii_lowercase();
    if host.is_empty() { None } else { Some(host) }
}

fn parse_hsts(value: &HeaderValue) -> Option<(Duration, bool)> {
    let s = value.to_str().ok()?;
    let mut max_age = None;
    let mut include_subdomains = false;

    for part in s.split(';') {
        let part = part.trim().to_lowercase();
        if let Some(age_str) = part.strip_prefix("max-age=") {
            if let Ok(secs) = age_str.trim().trim_matches('"').parse::<u64>() {
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
    fn host_matching_is_case_insensitive() {
        let store = HstsStore::new();
        let headers = hsts_headers("max-age=31536000");
        store.store_from_response("EXAMPLE.COM", &headers);

        assert!(store.should_upgrade("example.com"));
        assert!(store.should_upgrade("Example.Com"));
    }

    #[test]
    fn host_matching_ignores_single_port_suffix() {
        let store = HstsStore::new();
        let headers = hsts_headers("max-age=31536000");
        store.store_from_response("example.com:443", &headers);

        assert!(store.should_upgrade("example.com"));
        assert!(store.should_upgrade("example.com:80"));
    }

    #[test]
    fn include_subdomains_matches_canonicalized_hosts() {
        let store = HstsStore::new();
        let headers = hsts_headers("max-age=31536000; includeSubDomains");
        store.store_from_response("Example.Com:443", &headers);

        assert!(store.should_upgrade("api.EXAMPLE.com:80"));
        assert!(store.should_upgrade("deep.api.example.com."));
    }

    #[test]
    fn bracketed_ipv6_hosts_are_canonicalized() {
        let store = HstsStore::new();
        let headers = hsts_headers("max-age=31536000");
        store.store_from_response("[::1]", &headers);

        assert!(store.should_upgrade("::1"));
        assert!(store.should_upgrade("[::1]"));
    }

    #[test]
    fn bracketed_ipv6_authorities_with_ports_are_canonicalized() {
        let store = HstsStore::new();
        let headers = hsts_headers("max-age=31536000");
        store.store_from_response("[::1]:443", &headers);

        assert!(store.should_upgrade("::1"));
        assert!(store.should_upgrade("[::1]"));
        assert!(store.should_upgrade("[::1]:80"));
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

    #[test]
    fn debug_format() {
        let store = HstsStore::new();
        let dbg = format!("{:?}", store);
        assert!(dbg.contains("HstsStore"));
    }

    #[test]
    fn debug_format_after_insert() {
        let store = HstsStore::new();
        store.store_from_response("example.com", &hsts_headers("max-age=3600"));
        // Debug should still work and show "HstsStore" even with entries
        let dbg = format!("{:?}", store);
        assert!(dbg.contains("HstsStore"));
        // Verify the store still functions after formatting
        assert!(store.should_upgrade("example.com"));
    }

    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn poisoned_lock_store_from_response_does_not_panic() {
        let store = HstsStore::new();

        // Poison the mutex by panicking in a thread while holding the lock
        let inner_clone = store.inner.clone();
        let result = std::thread::spawn(move || {
            let _guard = inner_clone.lock().unwrap();
            panic!("intentional panic to poison mutex");
        })
        .join();
        assert!(result.is_err(), "thread should have panicked");

        // Now the mutex is poisoned. store_from_response should gracefully return
        // without panicking.
        store.store_from_response("example.com", &hsts_headers("max-age=3600"));
        // The entry should NOT have been stored since the lock is poisoned
        // (should_upgrade will also hit the poisoned lock and return false)
    }

    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn poisoned_lock_should_upgrade_returns_false() {
        let store = HstsStore::new();

        // Poison the mutex
        let inner_clone = store.inner.clone();
        let result = std::thread::spawn(move || {
            let _guard = inner_clone.lock().unwrap();
            panic!("intentional panic to poison mutex");
        })
        .join();
        assert!(result.is_err());

        // should_upgrade should return false when lock is poisoned
        assert!(!store.should_upgrade("example.com"));
    }

    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn poisoned_lock_clear_does_not_panic() {
        let store = HstsStore::new();

        // Poison the mutex
        let inner_clone = store.inner.clone();
        let result = std::thread::spawn(move || {
            let _guard = inner_clone.lock().unwrap();
            panic!("intentional panic to poison mutex");
        })
        .join();
        assert!(result.is_err());

        // clear should not panic, just return
        store.clear();
    }

    #[test]
    fn default_creates_empty_store() {
        let store = HstsStore::default();
        assert!(!store.should_upgrade("example.com"));
    }

    #[test]
    fn quoted_max_age_parsed() {
        let store = HstsStore::new();
        let headers = hsts_headers("max-age=\"31536000\"");
        store.store_from_response("example.com", &headers);
        assert!(store.should_upgrade("example.com"));
    }

    #[test]
    fn hsts_max_age_negative_ignored() {
        let store = HstsStore::new();
        let headers = hsts_headers("max-age=-1");
        store.store_from_response("example.com", &headers);
        // max-age=-1 should NOT create an entry — should_upgrade returns false
        assert!(
            !store.should_upgrade("example.com"),
            "max-age=-1 should be ignored, no entry should be stored"
        );
    }
}
