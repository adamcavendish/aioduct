use std::time::{Duration, SystemTime};

use http::HeaderMap;
use http::header::{CACHE_CONTROL, EXPIRES};

pub(crate) struct CacheDirectives {
    pub(crate) max_age: Option<Duration>,
    pub(crate) s_maxage_set: bool,
    pub(crate) no_store: bool,
    pub(crate) no_cache: bool,
    pub(crate) private: bool,
    pub(crate) must_revalidate: bool,
    pub(crate) immutable: bool,
    pub(crate) stale_while_revalidate: Option<Duration>,
    pub(crate) stale_if_error: Option<Duration>,
}

pub(crate) fn parse_cache_control(headers: &HeaderMap) -> CacheDirectives {
    let mut directives = CacheDirectives {
        max_age: None,
        s_maxage_set: false,
        no_store: false,
        no_cache: false,
        private: false,
        must_revalidate: false,
        immutable: false,
        stale_while_revalidate: None,
        stale_if_error: None,
    };

    let Some(value) = headers.get(CACHE_CONTROL) else {
        return directives;
    };
    let Ok(s) = value.to_str() else {
        return directives;
    };

    for part in s.split(',') {
        let part = part.trim().to_lowercase();
        if part == "no-store" {
            directives.no_store = true;
        } else if part == "no-cache" {
            directives.no_cache = true;
            directives.must_revalidate = true;
        } else if part == "private" {
            directives.private = true;
        } else if part == "must-revalidate" {
            directives.must_revalidate = true;
        } else if let Some(age_str) = part.strip_prefix("max-age=")
            && let Ok(secs) = age_str.trim().parse::<u64>()
        {
            if !directives.s_maxage_set {
                directives.max_age = Some(Duration::from_secs(secs));
            }
        } else if let Some(age_str) = part.strip_prefix("s-maxage=")
            && let Ok(secs) = age_str.trim().parse::<u64>()
        {
            directives.max_age = Some(Duration::from_secs(secs));
            directives.s_maxage_set = true;
        } else if part == "immutable" {
            directives.immutable = true;
        } else if let Some(age_str) = part.strip_prefix("stale-while-revalidate=")
            && let Ok(secs) = age_str.trim().parse::<u64>()
        {
            directives.stale_while_revalidate = Some(Duration::from_secs(secs));
        } else if let Some(age_str) = part.strip_prefix("stale-if-error=")
            && let Ok(secs) = age_str.trim().parse::<u64>()
        {
            directives.stale_if_error = Some(Duration::from_secs(secs));
        }
    }

    directives
}

pub(super) fn parse_expires(headers: &HeaderMap) -> Option<SystemTime> {
    let value = headers.get(EXPIRES)?;
    let s = value.to_str().ok()?;
    httpdate_parse(s)
}

pub(crate) fn httpdate_parse(s: &str) -> Option<SystemTime> {
    let s = s.trim();
    // RFC 7231 date format: "Sun, 06 Nov 1994 08:49:37 GMT"
    // Simplified parser — handles the most common format
    let parts: Vec<&str> = s.split_whitespace().collect();
    if parts.len() < 6 {
        return None;
    }

    let day: u32 = parts[1].parse().ok()?;
    let month = match parts[2].to_lowercase().as_str() {
        "jan" => 1,
        "feb" => 2,
        "mar" => 3,
        "apr" => 4,
        "may" => 5,
        "jun" => 6,
        "jul" => 7,
        "aug" => 8,
        "sep" => 9,
        "oct" => 10,
        "nov" => 11,
        "dec" => 12,
        _ => return None,
    };
    let year: i32 = parts[3].parse().ok()?;
    let time_parts: Vec<&str> = parts[4].split(':').collect();
    if time_parts.len() != 3 {
        return None;
    }
    let hour: u32 = time_parts[0].parse().ok()?;
    let minute: u32 = time_parts[1].parse().ok()?;
    let second: u32 = time_parts[2].parse().ok()?;

    // Convert to duration since UNIX_EPOCH using a simplified calculation
    let days_since_epoch = days_from_civil(year, month, day)?;
    let secs =
        days_since_epoch as u64 * 86400 + hour as u64 * 3600 + minute as u64 * 60 + second as u64;
    Some(SystemTime::UNIX_EPOCH + Duration::from_secs(secs))
}

fn days_from_civil(y: i32, m: u32, d: u32) -> Option<i64> {
    let y = y as i64;
    let m = m as i64;
    let d = d as i64;
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as u64;
    let doy = ((153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1) as u64;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some(era * 146097 + doe as i64 - 719468)
}
