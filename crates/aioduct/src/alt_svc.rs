use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use http::uri::Authority;

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
}

impl AltSvcCache {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn insert(&self, authority: Authority, entries: Vec<AltSvcEntry>) {
        let mut map = self.inner.lock().unwrap();
        if entries.is_empty() {
            map.remove(&authority);
        } else {
            map.insert(authority, entries);
        }
    }

    pub fn lookup_h3(&self, authority: &Authority) -> Option<(Option<String>, u16)> {
        let mut map = self.inner.lock().unwrap();
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
    let mut depth = 0u32;
    let mut start = 0;
    for (i, c) in s.char_indices() {
        match c {
            '"' => depth ^= 1,
            ',' if depth == 0 => {
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
        if let Some(val) = param.strip_prefix("ma=") {
            if let Ok(secs) = val.trim().parse::<u64>() {
                max_age = Duration::from_secs(secs);
            }
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
}
