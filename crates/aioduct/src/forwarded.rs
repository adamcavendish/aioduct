use std::fmt;
use std::net::IpAddr;

/// A single element of the `Forwarded` header (RFC 7239).
///
/// Each element represents one proxy hop. Use the builder methods to set
/// the `by`, `for`, `host`, and `proto` parameters, then call
/// [`to_header_value`](ForwardedElement::to_header_value) or use `Display`
/// to produce the header-ready string.
#[derive(Debug, Clone, Default)]
pub struct ForwardedElement {
    by: Option<String>,
    forwarded_for: Option<String>,
    host: Option<String>,
    proto: Option<String>,
}

impl ForwardedElement {
    /// Create an empty forwarded element.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the `by` parameter (the proxy that received the request).
    pub fn by(mut self, value: impl Into<String>) -> Self {
        self.by = Some(value.into());
        self
    }

    /// Set the `by` parameter from an IP address. IPv6 is automatically bracketed.
    pub fn by_ip(self, ip: IpAddr) -> Self {
        self.by(format_ip(ip))
    }

    /// Set the `for` parameter (the client that made the request).
    pub fn forwarded_for(mut self, value: impl Into<String>) -> Self {
        self.forwarded_for = Some(value.into());
        self
    }

    /// Set the `for` parameter from an IP address. IPv6 is automatically bracketed.
    pub fn forwarded_for_ip(self, ip: IpAddr) -> Self {
        self.forwarded_for(format_ip(ip))
    }

    /// Set the `host` parameter (the original `Host` header value).
    pub fn host(mut self, value: impl Into<String>) -> Self {
        self.host = Some(value.into());
        self
    }

    /// Set the `proto` parameter (`http` or `https`).
    pub fn proto(mut self, value: impl Into<String>) -> Self {
        self.proto = Some(value.into());
        self
    }

    /// Produce the header value string.
    pub fn to_header_value(&self) -> String {
        self.to_string()
    }
}

fn format_ip(ip: IpAddr) -> String {
    match ip {
        IpAddr::V4(v4) => v4.to_string(),
        IpAddr::V6(v6) => format!("\"[{v6}]\""),
    }
}

fn needs_quoting(s: &str) -> bool {
    s.contains(|c: char| !c.is_ascii_alphanumeric() && !matches!(c, '.' | '-' | '_' | ':'))
}

fn write_param(
    f: &mut fmt::Formatter<'_>,
    key: &str,
    value: &str,
    first: &mut bool,
) -> fmt::Result {
    if !*first {
        f.write_str(";")?;
    }
    *first = false;
    write!(f, "{key}=")?;
    if value.starts_with('"') || !needs_quoting(value) {
        f.write_str(value)
    } else {
        write!(f, "\"{value}\"")
    }
}

impl fmt::Display for ForwardedElement {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut first = true;
        if let Some(ref v) = self.by {
            write_param(f, "by", v, &mut first)?;
        }
        if let Some(ref v) = self.forwarded_for {
            write_param(f, "for", v, &mut first)?;
        }
        if let Some(ref v) = self.host {
            write_param(f, "host", v, &mut first)?;
        }
        if let Some(ref v) = self.proto {
            write_param(f, "proto", v, &mut first)?;
        }
        Ok(())
    }
}

/// Build a `Forwarded` header value from multiple elements.
///
/// Elements are joined with `, ` per RFC 7239 §4.
pub fn format_forwarded(elements: &[ForwardedElement]) -> String {
    elements
        .iter()
        .map(|e| e.to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

/// Parse a `Forwarded` header value into elements.
///
/// Handles both single and multi-element header values. Malformed elements
/// are silently skipped.
pub fn parse_forwarded(value: &str) -> Vec<ForwardedElement> {
    value
        .split(',')
        .filter_map(|segment| {
            let segment = segment.trim();
            if segment.is_empty() {
                return None;
            }
            let mut elem = ForwardedElement::new();
            let mut has_any = false;
            for pair in segment.split(';') {
                let pair = pair.trim();
                let (key, val) = pair.split_once('=')?;
                let key = key.trim().to_lowercase();
                let val = val.trim().trim_matches('"');
                has_any = true;
                match key.as_str() {
                    "by" => elem.by = Some(val.to_owned()),
                    "for" => elem.forwarded_for = Some(val.to_owned()),
                    "host" => elem.host = Some(val.to_owned()),
                    "proto" => elem.proto = Some(val.to_owned()),
                    _ => {}
                }
            }
            has_any.then_some(elem)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, Ipv6Addr};

    #[test]
    fn single_for() {
        let elem = ForwardedElement::new().forwarded_for("192.0.2.60");
        assert_eq!(elem.to_header_value(), "for=192.0.2.60");
    }

    #[test]
    fn full_element() {
        let elem = ForwardedElement::new()
            .by("203.0.113.43")
            .forwarded_for("198.51.100.17")
            .host("example.com")
            .proto("https");
        assert_eq!(
            elem.to_header_value(),
            "by=203.0.113.43;for=198.51.100.17;host=example.com;proto=https"
        );
    }

    #[test]
    fn ipv6_bracketed_and_quoted() {
        let ip = IpAddr::V6(Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 1));
        let elem = ForwardedElement::new().forwarded_for_ip(ip);
        assert_eq!(elem.to_header_value(), "for=\"[2001:db8::1]\"");
    }

    #[test]
    fn ipv4_not_quoted() {
        let ip = IpAddr::V4(Ipv4Addr::new(192, 0, 2, 60));
        let elem = ForwardedElement::new().by_ip(ip);
        assert_eq!(elem.to_header_value(), "by=192.0.2.60");
    }

    #[test]
    fn multiple_elements() {
        let elems = vec![
            ForwardedElement::new().forwarded_for("192.0.2.43"),
            ForwardedElement::new().forwarded_for("198.51.100.17"),
        ];
        assert_eq!(
            format_forwarded(&elems),
            "for=192.0.2.43, for=198.51.100.17"
        );
    }

    #[test]
    fn parse_single() {
        let elems = parse_forwarded("for=192.0.2.60;proto=https");
        assert_eq!(elems.len(), 1);
        assert_eq!(elems[0].forwarded_for.as_deref(), Some("192.0.2.60"));
        assert_eq!(elems[0].proto.as_deref(), Some("https"));
    }

    #[test]
    fn parse_multiple() {
        let elems = parse_forwarded("for=192.0.2.43, for=198.51.100.17");
        assert_eq!(elems.len(), 2);
        assert_eq!(elems[0].forwarded_for.as_deref(), Some("192.0.2.43"));
        assert_eq!(elems[1].forwarded_for.as_deref(), Some("198.51.100.17"));
    }

    #[test]
    fn parse_quoted_ipv6() {
        let elems = parse_forwarded("for=\"[2001:db8::1]\"");
        assert_eq!(elems.len(), 1);
        assert_eq!(elems[0].forwarded_for.as_deref(), Some("[2001:db8::1]"));
    }

    #[test]
    fn parse_empty_string() {
        let elems = parse_forwarded("");
        assert!(elems.is_empty());
    }

    #[test]
    fn roundtrip() {
        let original = ForwardedElement::new()
            .by("proxy.example.com")
            .forwarded_for("192.0.2.60")
            .host("example.com")
            .proto("https");
        let s = original.to_header_value();
        let parsed = parse_forwarded(&s);
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].by.as_deref(), Some("proxy.example.com"));
        assert_eq!(parsed[0].forwarded_for.as_deref(), Some("192.0.2.60"));
        assert_eq!(parsed[0].host.as_deref(), Some("example.com"));
        assert_eq!(parsed[0].proto.as_deref(), Some("https"));
    }
}
