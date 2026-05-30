use std::time::{Duration, SystemTime};

pub(super) fn parse_http_date(s: &str) -> Option<SystemTime> {
    parse_imf_fixdate(s)
        .or_else(|| parse_rfc850(s))
        .or_else(|| parse_asctime(s))
}

fn parse_imf_fixdate(s: &str) -> Option<SystemTime> {
    let parts: Vec<&str> = s.split_whitespace().collect();
    if parts.len() != 6 || parts[5] != "GMT" {
        return None;
    }

    let day: u64 = parts[1].parse().ok()?;
    let month = parse_month(parts[2])?;
    let year: u64 = parts[3].parse().ok()?;
    let (hour, min, sec) = parse_time(parts[4])?;

    compute_unix_time(year, month, day, hour, min, sec)
}

pub(super) fn parse_rfc850(s: &str) -> Option<SystemTime> {
    let (_, rest) = s.split_once(", ")?;
    let parts: Vec<&str> = rest.split_whitespace().collect();
    if parts.len() != 3 || parts[2] != "GMT" {
        return None;
    }
    let date_parts: Vec<&str> = parts[0].split('-').collect();
    if date_parts.len() != 3 {
        return None;
    }
    let day: u64 = date_parts[0].parse().ok()?;
    let month = parse_month(date_parts[1])?;
    let mut year: u64 = date_parts[2].parse().ok()?;
    if year < 70 {
        year += 2000;
    } else if year < 100 {
        year += 1900;
    }
    let (hour, min, sec) = parse_time(parts[1])?;

    compute_unix_time(year, month, day, hour, min, sec)
}

pub(super) fn parse_asctime(s: &str) -> Option<SystemTime> {
    let parts: Vec<&str> = s.split_whitespace().collect();
    if parts.len() != 5 {
        return None;
    }
    let month = parse_month(parts[1])?;
    let day: u64 = parts[2].parse().ok()?;
    let (hour, min, sec) = parse_time(parts[3])?;
    let year: u64 = parts[4].parse().ok()?;

    compute_unix_time(year, month, day, hour, min, sec)
}

fn parse_month(s: &str) -> Option<u64> {
    match s {
        "Jan" => Some(1),
        "Feb" => Some(2),
        "Mar" => Some(3),
        "Apr" => Some(4),
        "May" => Some(5),
        "Jun" => Some(6),
        "Jul" => Some(7),
        "Aug" => Some(8),
        "Sep" => Some(9),
        "Oct" => Some(10),
        "Nov" => Some(11),
        "Dec" => Some(12),
        _ => None,
    }
}

fn parse_time(s: &str) -> Option<(u64, u64, u64)> {
    let parts: Vec<&str> = s.split(':').collect();
    if parts.len() != 3 {
        return None;
    }
    Some((
        parts[0].parse().ok()?,
        parts[1].parse().ok()?,
        parts[2].parse().ok()?,
    ))
}

pub(super) fn compute_unix_time(
    year: u64,
    month: u64,
    day: u64,
    hour: u64,
    min: u64,
    sec: u64,
) -> Option<SystemTime> {
    if year < 1970 {
        return Some(SystemTime::UNIX_EPOCH);
    }

    let days_before_month = [0u64, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334];
    let m = month.checked_sub(1)? as usize;
    if m >= 12 {
        return None;
    }

    let mut days = (year - 1970) * 365;
    if year > 1970 {
        days += (year - 1) / 4 - 1969 / 4;
        days -= (year - 1) / 100 - 1969 / 100;
        days += (year - 1) / 400 - 1969 / 400;
    }
    days += days_before_month[m];
    if month > 2
        && (year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400)))
    {
        days += 1;
    }
    days += day - 1;

    let unix_secs = days * 86400 + hour * 3600 + min * 60 + sec;
    Some(SystemTime::UNIX_EPOCH + Duration::from_secs(unix_secs))
}
