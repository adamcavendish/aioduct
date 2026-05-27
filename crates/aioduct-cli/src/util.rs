use std::time::Duration;

/// Parse a proxy URL string into an `aioduct::ProxyConfig` by detecting the scheme.
pub(crate) fn parse_proxy_url(url: &str) -> Option<aioduct::ProxyConfig> {
    aioduct::ProxyConfig::detect_from_url(url)
}

/// Convert a Duration to milliseconds as f64.
pub(crate) fn duration_ms(d: &Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

/// Format a byte count with IEC binary prefixes (space before unit).
/// Uses smart decimal: rounds to integer when within 0.05 of a whole number.
pub(crate) fn human_bytes(bytes: u64) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = KIB * 1024.0;
    const GIB: f64 = MIB * 1024.0;

    let b = bytes as f64;
    if b >= GIB {
        format_iec_unit(b / GIB, "GiB")
    } else if b >= MIB {
        format_iec_unit(b / MIB, "MiB")
    } else if b >= KIB {
        format_iec_unit(b / KIB, "KiB")
    } else {
        format!("{bytes} B")
    }
}

/// Format a bytes-per-second rate with IEC binary prefixes and "/s" suffix.
pub(crate) fn human_speed(bps: f64) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = KIB * 1024.0;

    if bps >= MIB {
        format!("{} MiB/s", format_iec_val(bps / MIB))
    } else if bps >= KIB {
        format!("{} KiB/s", format_iec_val(bps / KIB))
    } else {
        format!("{:.0} B/s", bps)
    }
}

fn format_iec_unit(value: f64, unit: &str) -> String {
    format!("{} {unit}", format_iec_val(value))
}

fn format_iec_val(value: f64) -> String {
    if (value - value.round()).abs() < 0.05 {
        format!("{value:.0}")
    } else {
        format!("{value:.1}")
    }
}

/// Redact a single header value if the header name is sensitive.
pub(crate) fn redact_header_value<'a>(value: &'a str, name: &str) -> &'a str {
    let lower = name.to_lowercase();
    if lower == "authorization"
        || lower == "proxy-authorization"
        || lower == "cookie"
        || lower == "set-cookie"
    {
        "***"
    } else {
        value
    }
}

/// Redact all sensitive header values in a slice.
pub(crate) fn redact_headers(headers: &[(String, String)]) -> Vec<(String, String)> {
    headers
        .iter()
        .map(|(k, v)| {
            let value = redact_header_value(v, k);
            (k.clone(), value.to_string())
        })
        .collect()
}

/// Check whether response headers indicate binary (non-text) content.
pub(crate) fn is_binary_content_type(headers: &[(String, String)]) -> bool {
    let ct = headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("content-type"))
        .map(|(_, v)| v.as_str())
        .unwrap_or("");

    let ct_lower = ct.to_lowercase();

    if ct_lower.starts_with("text/") || ct_lower == "application/json" {
        return false;
    }

    if ct_lower.starts_with("image/")
        || ct_lower.starts_with("audio/")
        || ct_lower.starts_with("video/")
        || ct_lower.starts_with("font/")
    {
        return true;
    }

    matches!(
        ct_lower.as_str(),
        "application/octet-stream"
            | "application/pdf"
            | "application/zip"
            | "application/gzip"
            | "application/x-tar"
            | "application/x-gtar"
            | "application/x-compressed"
            | "application/x-bzip2"
            | "application/x-xz"
            | "application/zstd"
            | "application/protobuf"
            | "application/x-protobuf"
            | "application/msgpack"
            | "application/x-msgpack"
            | "application/cbor"
            | "application/wasm"
    )
}

/// Truncate a &str to at most `max` bytes, respecting UTF-8 character boundaries.
pub(crate) fn truncate_str(s: &str, max: usize) -> &str {
    if s.len() <= max {
        s
    } else {
        let mut end = max;
        while end > 0 && !s.is_char_boundary(end) {
            end -= 1;
        }
        &s[..end]
    }
}

/// Truncate a string to at most `max_chars` characters, appending '…' if truncated.
pub(crate) fn truncate_chars(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_string()
    } else {
        s.chars()
            .take(max_chars.saturating_sub(1))
            .collect::<String>()
            + "…"
    }
}

/// Find the nearest UTF-8 character boundary at or before `target` byte offset.
pub(crate) fn find_split_point(s: &str, target: usize) -> usize {
    if s.is_char_boundary(target) {
        return target;
    }
    (0..target)
        .rev()
        .find(|&i| s.is_char_boundary(i))
        .unwrap_or(target)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duration_ms_converts_correctly() {
        assert_eq!(duration_ms(&Duration::from_millis(100)), 100.0);
        assert_eq!(duration_ms(&Duration::from_secs(1)), 1000.0);
    }

    #[test]
    fn human_bytes_formats() {
        assert_eq!(human_bytes(0), "0 B");
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(1024), "1 KiB");
        assert_eq!(human_bytes(1536), "1.5 KiB");
        assert_eq!(human_bytes(1024 * 1024), "1 MiB");
        assert_eq!(human_bytes(1024 * 1024 * 1024), "1 GiB");
    }

    #[test]
    fn human_speed_formats() {
        assert_eq!(human_speed(100.0), "100 B/s");
        assert_eq!(human_speed(0.0), "0 B/s");
        assert_eq!(human_speed(1024.0), "1 KiB/s");
        assert_eq!(human_speed(1024.0 * 1024.0), "1 MiB/s");
    }

    #[test]
    fn redact_header_value_hides_sensitive() {
        assert_eq!(redact_header_value("secret", "authorization"), "***");
        assert_eq!(redact_header_value("secret", "Authorization"), "***");
        assert_eq!(redact_header_value("secret", "proxy-authorization"), "***");
        assert_eq!(redact_header_value("secret", "cookie"), "***");
        assert_eq!(redact_header_value("secret", "set-cookie"), "***");
        assert_eq!(
            redact_header_value("text/html", "content-type"),
            "text/html"
        );
    }

    #[test]
    fn redact_headers_redacts_all_sensitive() {
        let headers = vec![
            ("content-type".into(), "text/html".into()),
            ("authorization".into(), "bearer token".into()),
        ];
        let result = redact_headers(&headers);
        assert_eq!(result[0].1, "text/html");
        assert_eq!(result[1].1, "***");
    }

    #[test]
    fn is_binary_detects_types() {
        assert!(!is_binary_content_type(&[(
            "content-type".into(),
            "text/html".into()
        )]));
        assert!(!is_binary_content_type(&[(
            "content-type".into(),
            "application/json".into()
        )]));
        assert!(is_binary_content_type(&[(
            "content-type".into(),
            "image/png".into()
        )]));
        assert!(is_binary_content_type(&[(
            "content-type".into(),
            "application/octet-stream".into()
        )]));
        assert!(!is_binary_content_type(&[]));
    }

    #[test]
    fn truncate_str_preserves_boundaries() {
        assert_eq!(truncate_str("hello", 10), "hello");
        assert_eq!(truncate_str("hello", 5), "hello");
        assert_eq!(truncate_str("hello world", 5), "hello");
        assert_eq!(truncate_str("", 5), "");
        assert_eq!(truncate_str("éclair", 2), "é");
    }

    #[test]
    fn truncate_chars_adds_ellipsis() {
        assert_eq!(truncate_chars("abcdef", 4), "abc…");
        assert_eq!(truncate_chars("hello", 10), "hello");
    }

    #[test]
    fn find_split_point_on_and_off_boundary() {
        // "héllo": h(1B) é(2B, starts at byte 1) l(1B) l(1B) o(1B)
        let s = "héllo";
        assert_eq!(find_split_point(s, 0), 0); // on 'h'
        assert_eq!(find_split_point(s, 1), 1); // start of 'é', IS a char boundary
        assert_eq!(find_split_point(s, 2), 1); // mid-'é', walks back to start of 'é'
        assert_eq!(find_split_point(s, 3), 3); // start of 'l', IS a char boundary
    }
}
