use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use http::uri::Authority;

const H3_FAILURE_BACKOFF: Duration = Duration::from_secs(60);

#[derive(Debug, Clone)]
pub(crate) struct AltSvcEntry {
    pub protocol: String,
    pub host: Option<String>,
    pub port: u16,
    pub max_age: Duration,
    pub recorded_at: Instant,
}

impl AltSvcEntry {
    fn is_expired(&self) -> bool {
        self.recorded_at.elapsed() >= self.max_age
    }

    pub fn supports_h3(&self) -> bool {
        self.protocol == "h3"
    }
}

#[derive(Clone)]
pub(crate) struct AltSvcCache {
    inner: Arc<Mutex<HashMap<Authority, Vec<AltSvcEntry>>>>,
    suppressed_h3_until: Arc<Mutex<HashMap<Authority, Instant>>>,
}

impl AltSvcCache {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
            suppressed_h3_until: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn insert(&self, authority: Authority, entries: Vec<AltSvcEntry>) {
        if entries.is_empty() {
            if let Ok(mut suppressed) = self.suppressed_h3_until.lock() {
                suppressed.remove(&authority);
            }
            if let Ok(mut map) = self.inner.lock() {
                map.remove(&authority);
            }
            return;
        }
        if let Ok(mut map) = self.inner.lock() {
            map.insert(authority, entries);
        }
    }

    pub fn lookup_h3(&self, authority: &Authority) -> Option<(Option<String>, u16)> {
        let mut suppressed = self.suppressed_h3_until.lock().ok()?;
        if let Some(retry_at) = suppressed.get(authority) {
            if *retry_at > Instant::now() {
                return None;
            }
            suppressed.remove(authority);
        }
        drop(suppressed);

        let mut map = self.inner.lock().ok()?;
        let entries = map.get_mut(authority)?;
        entries.retain(|e| !e.is_expired());
        if entries.is_empty() {
            map.remove(authority);
            return None;
        }
        entries
            .iter()
            .find(|e| e.supports_h3())
            .map(|e| (e.host.clone(), e.port))
    }

    pub fn suppress_h3(&self, authority: &Authority) {
        if let Ok(mut suppressed) = self.suppressed_h3_until.lock() {
            suppressed.insert(authority.clone(), Instant::now() + H3_FAILURE_BACKOFF);
        }
        let Ok(mut map) = self.inner.lock() else {
            return;
        };
        let Some(entries) = map.get_mut(authority) else {
            return;
        };
        entries.retain(|entry| !entry.supports_h3());
        if entries.is_empty() {
            map.remove(authority);
        }
    }
}

pub(crate) fn parse_alt_svc(header_value: &str) -> Vec<AltSvcEntry> {
    let trimmed = header_value.trim();
    if trimmed.eq_ignore_ascii_case("clear") {
        return Vec::new();
    }

    let mut entries = Vec::new();
    for part in split_entries(trimmed) {
        if let Some(entry) = parse_single_entry(part.trim()) {
            entries.push(entry);
        }
    }
    entries
}

fn split_entries(s: &str) -> Vec<&str> {
    let mut entries = Vec::new();
    let mut in_quotes = false;
    let mut prev_backslash = false;
    let mut start = 0;
    for (i, c) in s.char_indices() {
        if prev_backslash {
            prev_backslash = false;
            continue;
        }
        match c {
            '\\' if in_quotes => {
                prev_backslash = true;
            }
            '"' => in_quotes = !in_quotes,
            ',' if !in_quotes => {
                entries.push(&s[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    entries.push(&s[start..]);
    entries
}

fn parse_single_entry(s: &str) -> Option<AltSvcEntry> {
    let (proto_authority, params) = match s.find(';') {
        Some(pos) => (&s[..pos], &s[pos + 1..]),
        None => (s, ""),
    };

    let proto_authority = proto_authority.trim();
    let eq_pos = proto_authority.find('=')?;
    let protocol = proto_authority[..eq_pos].trim().to_owned();
    let authority_str = proto_authority[eq_pos + 1..].trim().trim_matches('"');

    let (host, port) = parse_authority(authority_str)?;

    let mut max_age = Duration::from_secs(86400);
    for param in params.split(';') {
        let param = param.trim();
        if let Some(val) = param.strip_prefix("ma=")
            && let Ok(secs) = val.trim().parse::<u64>()
        {
            max_age = Duration::from_secs(secs);
        }
    }

    Some(AltSvcEntry {
        protocol,
        host: if host.is_empty() { None } else { Some(host) },
        port,
        max_age,
        recorded_at: Instant::now(),
    })
}

fn parse_authority(s: &str) -> Option<(String, u16)> {
    if let Some(colon) = s.rfind(':') {
        let host = s[..colon].to_owned();
        let port = s[colon + 1..].parse().ok()?;
        Some((host, port))
    } else {
        let port = s.parse().ok()?;
        Some((String::new(), port))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_h3_basic() {
        let entries = parse_alt_svc("h3=\":443\"; ma=86400");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].protocol, "h3");
        assert!(entries[0].host.is_none());
        assert_eq!(entries[0].port, 443);
        assert_eq!(entries[0].max_age, Duration::from_secs(86400));
        assert!(entries[0].supports_h3());
    }

    #[test]
    fn test_parse_h3_with_host() {
        let entries = parse_alt_svc("h3=\"alt.example.com:8443\"");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].protocol, "h3");
        assert_eq!(entries[0].host.as_deref(), Some("alt.example.com"));
        assert_eq!(entries[0].port, 8443);
        assert_eq!(entries[0].max_age, Duration::from_secs(86400));
    }

    #[test]
    fn test_parse_multiple_entries() {
        let entries = parse_alt_svc("h3=\":443\"; ma=3600, h3-29=\":443\"; ma=3600");
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].protocol, "h3");
        assert_eq!(entries[1].protocol, "h3-29");
    }

    #[test]
    fn test_parse_clear() {
        let entries = parse_alt_svc("clear");
        assert!(entries.is_empty());
    }

    #[test]
    fn test_parse_clear_case_insensitive() {
        let entries = parse_alt_svc("Clear");
        assert!(entries.is_empty());
    }

    #[test]
    fn test_parse_default_max_age() {
        let entries = parse_alt_svc("h3=\":443\"");
        assert_eq!(entries[0].max_age, Duration::from_secs(86400));
    }

    #[test]
    fn test_cache_insert_and_lookup() {
        let cache = AltSvcCache::new();
        let authority: Authority = "example.com:443".parse().unwrap();
        let entries = vec![AltSvcEntry {
            protocol: "h3".to_owned(),
            host: None,
            port: 443,
            max_age: Duration::from_secs(3600),
            recorded_at: Instant::now(),
        }];
        cache.insert(authority.clone(), entries);

        let result = cache.lookup_h3(&authority);
        assert!(result.is_some());
        let (host, port) = result.unwrap();
        assert!(host.is_none());
        assert_eq!(port, 443);
    }

    #[test]
    fn test_cache_clear_removes_entries() {
        let cache = AltSvcCache::new();
        let authority: Authority = "example.com:443".parse().unwrap();
        let entries = vec![AltSvcEntry {
            protocol: "h3".to_owned(),
            host: None,
            port: 443,
            max_age: Duration::from_secs(3600),
            recorded_at: Instant::now(),
        }];
        cache.insert(authority.clone(), entries);
        cache.insert(authority.clone(), Vec::new());

        assert!(cache.lookup_h3(&authority).is_none());
    }

    #[test]
    fn test_cache_suppress_h3_preserves_other_protocols() {
        let cache = AltSvcCache::new();
        let authority: Authority = "example.com:443".parse().unwrap();
        let entry = |protocol: &str| AltSvcEntry {
            protocol: protocol.to_owned(),
            host: None,
            port: 443,
            max_age: Duration::from_secs(3600),
            recorded_at: Instant::now(),
        };
        cache.insert(authority.clone(), vec![entry("h3"), entry("h2")]);

        cache.suppress_h3(&authority);

        assert!(cache.lookup_h3(&authority).is_none());
        assert_eq!(cache.inner.lock().unwrap()[&authority].len(), 1);
        assert_eq!(cache.inner.lock().unwrap()[&authority][0].protocol, "h2");
    }

    #[test]
    fn test_cache_suppression_ignores_repeated_h3_advertisement() {
        let cache = AltSvcCache::new();
        let authority: Authority = "example.com:443".parse().unwrap();
        let entry = || AltSvcEntry {
            protocol: "h3".to_owned(),
            host: None,
            port: 443,
            max_age: Duration::from_secs(3600),
            recorded_at: Instant::now(),
        };
        cache.insert(authority.clone(), vec![entry()]);
        cache.suppress_h3(&authority);
        cache.insert(authority.clone(), vec![entry()]);

        assert!(cache.lookup_h3(&authority).is_none());
    }

    #[test]
    fn test_cache_suppression_expires() {
        let cache = AltSvcCache::new();
        let authority: Authority = "example.com:443".parse().unwrap();
        let entry = AltSvcEntry {
            protocol: "h3".to_owned(),
            host: None,
            port: 443,
            max_age: Duration::from_secs(3600),
            recorded_at: Instant::now(),
        };
        cache.insert(authority.clone(), vec![entry.clone()]);
        cache.suppress_h3(&authority);
        cache.insert(authority.clone(), vec![entry]);
        cache
            .suppressed_h3_until
            .lock()
            .unwrap()
            .insert(authority.clone(), Instant::now() - Duration::from_secs(1));

        assert_eq!(cache.lookup_h3(&authority), Some((None, 443)));
    }

    #[test]
    fn test_cache_expired_entries() {
        let cache = AltSvcCache::new();
        let authority: Authority = "example.com:443".parse().unwrap();
        let entries = vec![AltSvcEntry {
            protocol: "h3".to_owned(),
            host: None,
            port: 443,
            max_age: Duration::from_secs(0),
            recorded_at: Instant::now() - Duration::from_secs(1),
        }];
        cache.insert(authority.clone(), entries);

        assert!(cache.lookup_h3(&authority).is_none());
    }

    #[test]
    fn test_cache_no_h3_entry() {
        let cache = AltSvcCache::new();
        let authority: Authority = "example.com:443".parse().unwrap();
        let entries = vec![AltSvcEntry {
            protocol: "h2".to_owned(),
            host: None,
            port: 443,
            max_age: Duration::from_secs(3600),
            recorded_at: Instant::now(),
        }];
        cache.insert(authority.clone(), entries);

        assert!(cache.lookup_h3(&authority).is_none());
    }

    #[test]
    fn test_parse_port_only_authority() {
        let entries = parse_alt_svc("h3=\":8443\"");
        assert_eq!(entries.len(), 1);
        assert!(entries[0].host.is_none());
        assert_eq!(entries[0].port, 8443);
    }

    #[test]
    fn test_is_expired_fresh() {
        let entry = AltSvcEntry {
            protocol: "h3".to_owned(),
            host: None,
            port: 443,
            max_age: Duration::from_secs(3600),
            recorded_at: Instant::now(),
        };
        assert!(!entry.is_expired());
    }

    #[test]
    fn test_is_expired_stale() {
        let entry = AltSvcEntry {
            protocol: "h3".to_owned(),
            host: None,
            port: 443,
            max_age: Duration::from_secs(0),
            recorded_at: Instant::now() - Duration::from_secs(1),
        };
        assert!(entry.is_expired());
    }

    #[test]
    fn test_supports_h3_false_for_other_protocol() {
        let entry = AltSvcEntry {
            protocol: "h2".to_owned(),
            host: None,
            port: 443,
            max_age: Duration::from_secs(3600),
            recorded_at: Instant::now(),
        };
        assert!(!entry.supports_h3());
    }

    #[test]
    fn test_cache_clone_shares_state() {
        let cache = AltSvcCache::new();
        let cloned = cache.clone();
        let authority: Authority = "shared.com:443".parse().unwrap();
        let entries = vec![AltSvcEntry {
            protocol: "h3".to_owned(),
            host: None,
            port: 443,
            max_age: Duration::from_secs(3600),
            recorded_at: Instant::now(),
        }];
        cache.insert(authority.clone(), entries);
        assert!(cloned.lookup_h3(&authority).is_some());
    }

    #[test]
    fn test_parse_malformed_ma_value() {
        let entries = parse_alt_svc("h3=\":443\"; ma=notanumber");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].max_age, Duration::from_secs(86400));
    }

    #[test]
    fn test_parse_no_equals_in_entry() {
        let entries = parse_alt_svc("garbage");
        assert!(entries.is_empty());
    }

    #[test]
    fn test_parse_invalid_port() {
        let entries = parse_alt_svc("h3=\":notaport\"");
        assert!(entries.is_empty());
    }

    #[test]
    fn test_cache_mixed_h3_and_non_h3() {
        let cache = AltSvcCache::new();
        let authority: Authority = "example.com:443".parse().unwrap();
        let entries = vec![
            AltSvcEntry {
                protocol: "h2".to_owned(),
                host: None,
                port: 443,
                max_age: Duration::from_secs(3600),
                recorded_at: Instant::now(),
            },
            AltSvcEntry {
                protocol: "h3".to_owned(),
                host: Some("alt.com".to_owned()),
                port: 8443,
                max_age: Duration::from_secs(3600),
                recorded_at: Instant::now(),
            },
        ];
        cache.insert(authority.clone(), entries);
        let result = cache.lookup_h3(&authority).unwrap();
        assert_eq!(result.0.as_deref(), Some("alt.com"));
        assert_eq!(result.1, 8443);
    }

    #[test]
    fn test_cache_lookup_evicts_all_expired() {
        let cache = AltSvcCache::new();
        let authority: Authority = "example.com:443".parse().unwrap();
        let entries = vec![AltSvcEntry {
            protocol: "h3".to_owned(),
            host: None,
            port: 443,
            max_age: Duration::from_secs(0),
            recorded_at: Instant::now() - Duration::from_secs(1),
        }];
        cache.insert(authority.clone(), entries);
        assert!(cache.lookup_h3(&authority).is_none());
        // After eviction, the authority should be removed entirely
        let map = cache.inner.lock().unwrap();
        assert!(!map.contains_key(&authority));
    }

    #[test]
    fn test_parse_authority_without_colon() {
        let result = parse_authority("443");
        assert!(result.is_some());
        let (host, port) = result.unwrap();
        assert!(host.is_empty());
        assert_eq!(port, 443);
    }

    #[test]
    fn test_entry_debug_and_clone() {
        let entry = AltSvcEntry {
            protocol: "h3".to_owned(),
            host: None,
            port: 443,
            max_age: Duration::from_secs(3600),
            recorded_at: Instant::now(),
        };
        let cloned = entry.clone();
        assert_eq!(entry.protocol, cloned.protocol);
        let dbg = format!("{:?}", entry);
        assert!(dbg.contains("h3"));
    }

    #[test]
    fn test_split_entries_escaped_quotes() {
        let entries = split_entries(r#"h3="host:443", h2="other:80""#);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].trim(), r#"h3="host:443""#);
        assert_eq!(entries[1].trim(), r#"h2="other:80""#);
    }

    #[test]
    fn test_split_entries_comma_in_quotes() {
        let entries = split_entries(r#"h3="host,with,commas:443"; ma=60"#);
        assert_eq!(entries.len(), 1);
    }

    #[test]
    fn test_split_entries_escaped_quote_in_value() {
        let entries = split_entries(r#"h3="val\"ue:443", h2=":80""#);
        assert_eq!(entries.len(), 2);
    }
}
