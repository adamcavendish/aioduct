use super::*;
use http::header::HeaderValue;
use std::time::{Duration, SystemTime};

fn headers_with_cookies(cookies: &[&str]) -> HeaderMap {
    let mut headers = HeaderMap::new();
    for c in cookies {
        headers.append(SET_COOKIE, HeaderValue::from_str(c).unwrap());
    }
    headers
}

#[test]
fn store_and_apply_roundtrip() {
    let jar = CookieJar::new();
    let headers = headers_with_cookies(&["foo=bar"]);
    jar.store_from_response("example.com", "/", &headers);

    let mut req_headers = HeaderMap::new();
    jar.apply_to_request("example.com", false, "/", None, &mut req_headers);
    assert_eq!(req_headers.get(COOKIE).unwrap(), "foo=bar");
}

#[test]
fn multiple_cookies_joined() {
    let jar = CookieJar::new();
    let headers = headers_with_cookies(&["a=1", "b=2"]);
    jar.store_from_response("example.com", "/", &headers);

    let mut req_headers = HeaderMap::new();
    jar.apply_to_request("example.com", false, "/", None, &mut req_headers);
    let cookie_str = req_headers.get(COOKIE).unwrap().to_str().unwrap();
    assert!(cookie_str.contains("a=1"));
    assert!(cookie_str.contains("b=2"));
    assert!(cookie_str.contains("; "));
}

#[test]
fn cookie_update_overwrites_existing() {
    let jar = CookieJar::new();
    jar.store_from_response("example.com", "/", &headers_with_cookies(&["k=old"]));
    jar.store_from_response("example.com", "/", &headers_with_cookies(&["k=new"]));

    let mut req_headers = HeaderMap::new();
    jar.apply_to_request("example.com", false, "/", None, &mut req_headers);
    assert_eq!(req_headers.get(COOKIE).unwrap(), "k=new");
}

#[test]
fn secure_cookie_excluded_on_insecure() {
    let jar = CookieJar::new();
    let headers = headers_with_cookies(&["s=secret; Secure"]);
    jar.store_from_response("example.com", "/", &headers);

    let mut req_headers = HeaderMap::new();
    jar.apply_to_request("example.com", false, "/", None, &mut req_headers);
    assert!(req_headers.get(COOKIE).is_none());
}

#[test]
fn secure_cookie_included_on_secure() {
    let jar = CookieJar::new();
    let headers = headers_with_cookies(&["s=secret; Secure"]);
    jar.store_from_response("example.com", "/", &headers);

    let mut req_headers = HeaderMap::new();
    jar.apply_to_request("example.com", true, "/", None, &mut req_headers);
    assert_eq!(req_headers.get(COOKIE).unwrap(), "s=secret");
}

#[test]
fn clear_empties_jar() {
    let jar = CookieJar::new();
    jar.store_from_response("example.com", "/", &headers_with_cookies(&["x=y"]));
    jar.clear();

    let mut req_headers = HeaderMap::new();
    jar.apply_to_request("example.com", false, "/", None, &mut req_headers);
    assert!(req_headers.get(COOKIE).is_none());
}

#[test]
fn empty_name_ignored() {
    let jar = CookieJar::new();
    let headers = headers_with_cookies(&["=value"]);
    jar.store_from_response("example.com", "/", &headers);

    let mut req_headers = HeaderMap::new();
    jar.apply_to_request("example.com", false, "/", None, &mut req_headers);
    assert!(req_headers.get(COOKIE).is_none());
}

#[test]
fn domain_attribute_with_leading_dot_stripped() {
    let cookie = parse_set_cookie("a=b; Domain=.foo.com", "sub.foo.com", "/");
    let c = cookie.unwrap();
    assert_eq!(c.domain.as_deref(), Some("foo.com"));
}

#[test]
fn domain_defaults_to_request_domain() {
    let cookie = parse_set_cookie("a=b", "request.com", "/");
    let c = cookie.unwrap();
    assert_eq!(c.domain.as_deref(), Some("request.com"));
}

#[test]
fn host_only_cookie_not_sent_to_subdomain() {
    let jar = CookieJar::new();
    jar.store_from_response("example.com", "/", &headers_with_cookies(&["hostonly=1"]));

    let mut req_headers = HeaderMap::new();
    jar.apply_to_request("sub.example.com", false, "/", None, &mut req_headers);

    assert!(
        req_headers.get(COOKIE).is_none(),
        "cookie without Domain must be scoped to the exact origin host",
    );
}

#[test]
fn domain_attribute_cookie_sent_to_subdomain() {
    let jar = CookieJar::new();
    jar.store_from_response(
        "example.com",
        "/",
        &headers_with_cookies(&["domain=1; Domain=example.com"]),
    );

    let mut req_headers = HeaderMap::new();
    jar.apply_to_request("sub.example.com", false, "/", None, &mut req_headers);

    assert_eq!(req_headers.get(COOKIE).unwrap(), "domain=1");
}

#[test]
fn httponly_attribute_parsed() {
    let cookie = parse_set_cookie("a=b; HttpOnly", "example.com", "/");
    assert!(cookie.unwrap().http_only);
}

#[test]
fn path_attribute_parsed() {
    let cookie = parse_set_cookie("a=b; Path=/api", "example.com", "/");
    assert_eq!(cookie.unwrap().path, "/api");
}

#[test]
fn no_equals_returns_none() {
    assert!(parse_set_cookie("invalid", "example.com", "/").is_none());
}

#[test]
fn different_domain_does_not_apply() {
    let jar = CookieJar::new();
    jar.store_from_response("a.com", "/", &headers_with_cookies(&["x=1"]));

    let mut req_headers = HeaderMap::new();
    jar.apply_to_request("b.com", false, "/", None, &mut req_headers);
    assert!(req_headers.get(COOKIE).is_none());
}

#[test]
fn clone_shares_state() {
    let jar = CookieJar::new();
    let jar2 = jar.clone();
    jar.store_from_response("example.com", "/", &headers_with_cookies(&["x=1"]));

    let mut req_headers = HeaderMap::new();
    jar2.apply_to_request("example.com", false, "/", None, &mut req_headers);
    assert_eq!(req_headers.get(COOKIE).unwrap(), "x=1");
}

#[test]
fn max_age_zero_removes_cookie() {
    let jar = CookieJar::new();
    jar.store_from_response("example.com", "/", &headers_with_cookies(&["k=v"]));
    jar.store_from_response(
        "example.com",
        "/",
        &headers_with_cookies(&["k=v; Max-Age=0"]),
    );

    let mut req_headers = HeaderMap::new();
    jar.apply_to_request("example.com", false, "/", None, &mut req_headers);
    assert!(req_headers.get(COOKIE).is_none());
}

#[test]
fn max_age_zero_not_stored() {
    let jar = CookieJar::new();
    jar.store_from_response(
        "example.com",
        "/",
        &headers_with_cookies(&["k=v; Max-Age=0"]),
    );

    let mut req_headers = HeaderMap::new();
    jar.apply_to_request("example.com", false, "/", None, &mut req_headers);
    assert!(req_headers.get(COOKIE).is_none());
}

#[test]
fn path_scoping() {
    let jar = CookieJar::new();
    jar.store_from_response(
        "example.com",
        "/",
        &headers_with_cookies(&["k=v; Path=/api"]),
    );

    let mut req_headers = HeaderMap::new();
    jar.apply_to_request("example.com", false, "/", None, &mut req_headers);
    assert!(req_headers.get(COOKIE).is_none());

    let mut req_headers = HeaderMap::new();
    jar.apply_to_request("example.com", false, "/api", None, &mut req_headers);
    assert_eq!(req_headers.get(COOKIE).unwrap(), "k=v");

    let mut req_headers = HeaderMap::new();
    jar.apply_to_request("example.com", false, "/api/sub", None, &mut req_headers);
    assert_eq!(req_headers.get(COOKIE).unwrap(), "k=v");
}

#[test]
fn domain_matching_exact() {
    assert!(domain_matches("example.com", "example.com"));
}

#[test]
fn domain_matching_subdomain() {
    assert!(domain_matches("sub.example.com", "example.com"));
}

#[test]
fn domain_matching_no_partial() {
    assert!(!domain_matches("notexample.com", "example.com"));
}

#[test]
fn domain_matching_different() {
    assert!(!domain_matches("other.com", "example.com"));
}

#[test]
fn subdomain_cookie_applied() {
    let jar = CookieJar::new();
    jar.store_from_response(
        "example.com",
        "/",
        &headers_with_cookies(&["k=v; Domain=example.com"]),
    );

    let mut req_headers = HeaderMap::new();
    jar.apply_to_request("sub.example.com", false, "/", None, &mut req_headers);
    assert_eq!(req_headers.get(COOKIE).unwrap(), "k=v");
}

#[test]
fn samesite_strict_parsed() {
    let cookie = parse_set_cookie("a=b; SameSite=Strict", "example.com", "/");
    assert_eq!(cookie.unwrap().same_site, Some(SameSite::Strict));
}

#[test]
fn samesite_lax_parsed() {
    let cookie = parse_set_cookie("a=b; SameSite=Lax", "example.com", "/");
    assert_eq!(cookie.unwrap().same_site, Some(SameSite::Lax));
}

#[test]
fn samesite_none_parsed() {
    let cookie = parse_set_cookie("a=b; SameSite=None; Secure", "example.com", "/");
    let c = cookie.unwrap();
    assert_eq!(c.same_site, Some(SameSite::None));
    assert!(c.secure);
}

#[test]
fn samesite_unknown_value_ignored() {
    let cookie = parse_set_cookie("a=b; SameSite=Invalid", "example.com", "/");
    assert_eq!(cookie.unwrap().same_site, None);
}

#[test]
fn host_prefix_valid() {
    let cookie = parse_set_cookie("__Host-id=123; Secure; Path=/", "example.com", "/");
    let c = cookie.unwrap();
    assert_eq!(c.name, "__Host-id");
    assert!(c.secure);
    assert_eq!(c.path, "/");
}

#[test]
fn host_prefix_rejected_without_secure() {
    let cookie = parse_set_cookie("__Host-id=123; Path=/", "example.com", "/");
    assert!(cookie.is_none());
}

#[test]
fn host_prefix_rejected_without_root_path() {
    let cookie = parse_set_cookie("__Host-id=123; Secure; Path=/api", "example.com", "/");
    assert!(cookie.is_none());
}

#[test]
fn host_prefix_rejected_with_domain() {
    let cookie = parse_set_cookie(
        "__Host-id=123; Secure; Path=/; Domain=other.com",
        "example.com",
        "/",
    );
    assert!(cookie.is_none());
}

#[test]
fn secure_prefix_valid() {
    let cookie = parse_set_cookie("__Secure-token=abc; Secure", "example.com", "/");
    assert!(cookie.is_some());
}

#[test]
fn secure_prefix_rejected_without_secure() {
    let cookie = parse_set_cookie("__Secure-token=abc", "example.com", "/");
    assert!(cookie.is_none());
}

#[test]
fn expires_past_marks_expired() {
    let cookie = parse_set_cookie(
        "sid=abc; Expires=Wed, 01 Jan 2020 00:00:00 GMT",
        "example.com",
        "/",
    );
    assert!(cookie.is_some());
    assert!(cookie.unwrap().expired);
}

#[test]
fn expires_future_not_expired() {
    let cookie = parse_set_cookie(
        "sid=abc; Expires=Wed, 01 Jan 2099 00:00:00 GMT",
        "example.com",
        "/",
    );
    assert!(cookie.is_some());
    assert!(!cookie.unwrap().expired);
}

#[test]
fn max_age_positive_not_expired() {
    let cookie = parse_set_cookie("sid=abc; Max-Age=3600", "example.com", "/");
    assert!(cookie.is_some());
    assert!(!cookie.unwrap().expired);
}

#[test]
fn max_age_negative_expired() {
    let cookie = parse_set_cookie("sid=abc; Max-Age=-1", "example.com", "/");
    assert!(cookie.is_some());
    assert!(cookie.unwrap().expired);
}

#[test]
fn cookie_jar_debug() {
    let jar = CookieJar::new();
    let dbg = format!("{jar:?}");
    assert!(dbg.contains("CookieJar"));
}

#[test]
fn cookie_jar_cookies_returns_all() {
    let jar = CookieJar::new();
    let mut headers = HeaderMap::new();
    headers.append(SET_COOKIE, "a=1".parse().unwrap());
    headers.append(SET_COOKIE, "b=2".parse().unwrap());
    jar.store_from_response("example.com", "/", &headers);
    let cookies = jar.cookies();
    assert_eq!(cookies.len(), 2);
}

#[test]
fn apply_to_request_adds_cookie_header() {
    let jar = CookieJar::new();
    let mut headers = HeaderMap::new();
    headers.append(SET_COOKIE, "token=xyz".parse().unwrap());
    jar.store_from_response("example.com", "/", &headers);

    let mut req_headers = HeaderMap::new();
    jar.apply_to_request("example.com", false, "/", None, &mut req_headers);
    let cookie = req_headers.get("cookie").unwrap().to_str().unwrap();
    assert!(cookie.contains("token=xyz"));
}

#[test]
fn apply_to_request_no_match_no_header() {
    let jar = CookieJar::new();
    let mut headers = HeaderMap::new();
    headers.append(SET_COOKIE, "token=xyz".parse().unwrap());
    jar.store_from_response("example.com", "/", &headers);

    let mut req_headers = HeaderMap::new();
    jar.apply_to_request("other.com", false, "/", None, &mut req_headers);
    assert!(req_headers.get("cookie").is_none());
}

#[test]
fn secure_cookie_not_sent_on_http() {
    let jar = CookieJar::new();
    let mut headers = HeaderMap::new();
    headers.append(SET_COOKIE, "s=1; Secure".parse().unwrap());
    jar.store_from_response("example.com", "/", &headers);

    let mut req_headers = HeaderMap::new();
    jar.apply_to_request("example.com", false, "/", None, &mut req_headers);
    assert!(req_headers.get("cookie").is_none());
}

#[test]
fn secure_cookie_sent_on_https() {
    let jar = CookieJar::new();
    let mut headers = HeaderMap::new();
    headers.append(SET_COOKIE, "s=1; Secure".parse().unwrap());
    jar.store_from_response("example.com", "/", &headers);

    let mut req_headers = HeaderMap::new();
    jar.apply_to_request("example.com", true, "/", None, &mut req_headers);
    assert!(req_headers.get("cookie").is_some());
}

#[test]
fn cookie_accessors() {
    let cookie = parse_set_cookie(
        "name=value; Domain=example.com; Path=/api; Secure; HttpOnly; SameSite=Strict",
        "example.com",
        "/",
    )
    .unwrap();
    assert_eq!(cookie.name(), "name");
    assert_eq!(cookie.value(), "value");
    assert_eq!(cookie.domain(), Some("example.com"));
    assert_eq!(cookie.path(), "/api");
    assert!(cookie.secure());
    assert!(cookie.http_only());
    assert_eq!(cookie.same_site(), Some(&SameSite::Strict));
}

#[test]
fn expires_november() {
    let cookie = parse_set_cookie(
        "sid=abc; Expires=Sun, 06 Nov 2099 08:49:37 GMT",
        "example.com",
        "/",
    );
    assert!(cookie.is_some());
    assert!(!cookie.unwrap().expired);
}

#[test]
fn expires_december() {
    let cookie = parse_set_cookie(
        "sid=abc; Expires=Mon, 25 Dec 2099 12:00:00 GMT",
        "example.com",
        "/",
    );
    assert!(cookie.is_some());
    assert!(!cookie.unwrap().expired);
}

#[test]
fn expires_invalid_month() {
    let cookie = parse_set_cookie(
        "sid=abc; Expires=Wed, 21 Xyz 2025 07:28:00 GMT",
        "example.com",
        "/",
    );
    let c = cookie.unwrap();
    assert!(!c.expired);
}

#[test]
fn expires_bad_time_format() {
    let cookie = parse_set_cookie(
        "sid=abc; Expires=Wed, 21 Oct 2025 07:28 GMT",
        "example.com",
        "/",
    );
    let c = cookie.unwrap();
    assert!(!c.expired);
}

#[test]
fn expires_leap_year_after_february() {
    let cookie = parse_set_cookie(
        "sid=abc; Expires=Fri, 01 Mar 2096 00:00:00 GMT",
        "example.com",
        "/",
    );
    assert!(cookie.is_some());
    assert!(!cookie.unwrap().expired);
}

#[test]
fn expired_cookie_removes_existing() {
    let jar = CookieJar::new();
    jar.store_from_response("example.com", "/", &headers_with_cookies(&["k=v"]));
    let cookies = jar.cookies();
    assert_eq!(cookies.len(), 1);

    jar.store_from_response(
        "example.com",
        "/",
        &headers_with_cookies(&["k=v; Expires=Wed, 01 Jan 2020 00:00:00 GMT"]),
    );
    let cookies = jar.cookies();
    assert_eq!(cookies.len(), 0);
}

#[test]
fn parse_http_date_all_months() {
    let months = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    for m in &months {
        let date = format!("Mon, 15 {m} 2099 10:30:00 GMT");
        let cookie = parse_set_cookie(&format!("sid=abc; Expires={date}"), "example.com", "/");
        assert!(cookie.is_some(), "cookie with month {m} should parse");
        assert!(
            !cookie.unwrap().expired,
            "future date with month {m} should not be expired"
        );
    }
}

#[test]
fn rfc850_date_format_expired() {
    let cookie = parse_set_cookie(
        "sid=abc; Expires=Sunday, 06-Nov-94 08:49:37 GMT",
        "example.com",
        "/",
    );
    assert!(cookie.is_some());
    assert!(cookie.unwrap().expired, "1994 date should be expired");
}

#[test]
fn rfc850_date_format_future_two_digit_year() {
    let cookie = parse_set_cookie(
        "sid=abc; Expires=Monday, 01-Jan-69 00:00:00 GMT",
        "example.com",
        "/",
    );
    let c = cookie.unwrap();
    assert!(
        !c.expired,
        "two-digit year < 70 should be interpreted as 2069"
    );
}

#[test]
fn asctime_date_format_expired() {
    let cookie = parse_set_cookie(
        "sid=abc; Expires=Sun Nov  6 08:49:37 1994",
        "example.com",
        "/",
    );
    assert!(cookie.is_some());
    assert!(
        cookie.unwrap().expired,
        "1994 asctime date should be expired"
    );
}

#[test]
fn asctime_date_format_future() {
    let cookie = parse_set_cookie(
        "sid=abc; Expires=Thu Jan  1 00:00:00 2099",
        "example.com",
        "/",
    );
    let c = cookie.unwrap();
    assert!(!c.expired, "2099 asctime date should not be expired");
}

#[test]
fn apply_to_request_merges_with_existing_cookie_header() {
    let jar = CookieJar::new();
    jar.store_from_response("example.com", "/", &headers_with_cookies(&["k=v"]));

    let mut req_headers = HeaderMap::new();
    req_headers.insert(COOKIE, "existing=cookie".parse().unwrap());
    jar.apply_to_request("example.com", false, "/", None, &mut req_headers);
    let cookie = req_headers.get(COOKIE).unwrap().to_str().unwrap();
    assert!(
        cookie.contains("existing=cookie"),
        "should preserve existing cookie"
    );
    assert!(cookie.contains("k=v"), "should add jar cookie");
}

#[test]
fn compute_unix_time_pre_1970_returns_epoch() {
    let result = compute_unix_time(1969, 12, 31, 23, 59, 59);
    assert_eq!(result, Some(SystemTime::UNIX_EPOCH));
}

#[test]
fn compute_unix_time_epoch_exactly() {
    let result = compute_unix_time(1970, 1, 1, 0, 0, 0).unwrap();
    assert_eq!(result, SystemTime::UNIX_EPOCH);
}

#[test]
fn compute_unix_time_invalid_month_13_returns_none() {
    // month=13 means m=12 which is >= 12, should return None (line 377)
    let result = compute_unix_time(2025, 13, 1, 0, 0, 0);
    assert_eq!(result, None, "month 13 should return None");
}

#[test]
fn compute_unix_time_invalid_month_99_returns_none() {
    // month=99 means m=98 which is >= 12, should return None
    let result = compute_unix_time(2025, 99, 1, 0, 0, 0);
    assert_eq!(result, None, "month 99 should return None");
}

/// BUG(#67): compute_unix_time panics on month=0 due to unchecked subtraction.
/// month=0 causes `month - 1` to underflow. Should return None instead.
#[test]
fn compute_unix_time_month_zero_returns_none() {
    assert_eq!(compute_unix_time(2025, 0, 1, 0, 0, 0), None);
}

#[test]
fn parse_rfc850_missing_date_parts() {
    // "Sunday, 06-Nov 08:49:37 GMT" has only 2 parts in the date
    // date_parts.len() != 3 should return None (line 304)
    assert!(parse_rfc850("Sunday, 06-Nov 08:49:37 GMT").is_none());
}

#[test]
fn parse_rfc850_two_digit_year_below_70() {
    // Two-digit year < 70 gets 2000 added (line 310: year += 2000)
    let result = parse_rfc850("Monday, 15-Jan-25 10:30:00 GMT");
    assert!(result.is_some(), "rfc850 with 2-digit year 25 should parse");
    // Year 25 -> 2025, which is in the past relative to 2026
    // 2025-01-15 should parse to a valid time
    let t = result.unwrap();
    assert!(t > SystemTime::UNIX_EPOCH);
}

#[test]
fn parse_rfc850_two_digit_year_exactly_69() {
    // Year 69 < 70, so gets 2000 added -> 2069 (future)
    let result = parse_rfc850("Wednesday, 01-Mar-69 00:00:00 GMT");
    assert!(result.is_some());
    let t = result.unwrap();
    // 2069 is well in the future
    assert!(t > SystemTime::now());
}

#[test]
fn parse_rfc850_two_digit_year_70_gets_1900() {
    // Year 70 >= 70 and < 100, so gets 1900 added -> 1970
    let result = parse_rfc850("Thursday, 01-Jan-70 00:00:01 GMT");
    assert!(result.is_some());
    let t = result.unwrap();
    // 1970-01-01 00:00:01 is UNIX_EPOCH + 1 second
    assert_eq!(t, SystemTime::UNIX_EPOCH + Duration::from_secs(1));
}

#[test]
fn parse_asctime_valid_format() {
    // Standard asctime format: "Wdy Mon DD HH:MM:SS YYYY"
    let result = parse_asctime("Thu Jan  1 00:00:00 2099");
    assert!(result.is_some());
    let t = result.unwrap();
    assert!(t > SystemTime::now(), "2099 should be in the future");
}

#[test]
fn parse_asctime_past_date() {
    let result = parse_asctime("Wed Jan  1 00:00:00 2020");
    assert!(result.is_some());
    let t = result.unwrap();
    assert!(t < SystemTime::now(), "2020 should be in the past");
}

#[test]
fn expires_rfc850_two_digit_year_below_70_not_expired() {
    // This exercises the full path through parse_set_cookie -> parse_http_date -> parse_rfc850
    // with a two-digit year < 70 (interpreted as 2000+year)
    let cookie = parse_set_cookie(
        "sid=abc; Expires=Wednesday, 01-Mar-69 00:00:00 GMT",
        "example.com",
        "/",
    );
    let c = cookie.unwrap();
    assert!(
        !c.expired,
        "year 69 should be interpreted as 2069 (not expired)"
    );
}

#[test]
fn domain_matches_subdomain_not_at_dot_boundary() {
    // "fooexample.com" should NOT match "example.com"
    // because the character before "example.com" is not a dot
    assert!(!domain_matches("fooexample.com", "example.com"));
}

#[test]
fn host_only_cookie_exact_match_applies() {
    // A cookie without Domain attribute (host_only=true) should match exact domain
    let jar = CookieJar::new();
    jar.store_from_response("exact.example.com", "/", &headers_with_cookies(&["h=1"]));

    let mut req_headers = HeaderMap::new();
    jar.apply_to_request("exact.example.com", false, "/", None, &mut req_headers);
    assert_eq!(req_headers.get(COOKIE).unwrap(), "h=1");
}

#[test]
fn domain_cookie_does_not_apply_to_unrelated() {
    // A cookie with Domain=example.com should NOT match totally-different.com
    let jar = CookieJar::new();
    jar.store_from_response(
        "example.com",
        "/",
        &headers_with_cookies(&["k=v; Domain=example.com"]),
    );

    let mut req_headers = HeaderMap::new();
    jar.apply_to_request("totally-different.com", false, "/", None, &mut req_headers);
    assert!(req_headers.get(COOKIE).is_none());
}

#[test]
fn cookies_accessor_returns_cloned_cookies() {
    let jar = CookieJar::new();
    jar.store_from_response(
        "example.com",
        "/",
        &headers_with_cookies(&["name=value; Secure; HttpOnly; Path=/api"]),
    );
    let cookies = jar.cookies();
    assert_eq!(cookies.len(), 1);
    let c = &cookies[0];
    assert_eq!(c.name(), "name");
    assert_eq!(c.value(), "value");
    assert!(c.secure());
    assert!(c.http_only());
    assert_eq!(c.path(), "/api");
}

#[test]
fn clear_on_empty_jar_is_noop() {
    let jar = CookieJar::new();
    jar.clear();
    assert!(jar.cookies().is_empty());
}

#[test]
fn parse_rfc850_invalid_format() {
    assert!(parse_rfc850("not a date").is_none());
    assert!(parse_rfc850("Sunday, 06-Nov-94 08:49:37 EST").is_none());
    assert!(parse_rfc850("Sunday, 06-Nov 08:49:37 GMT").is_none());
}

#[test]
fn parse_asctime_invalid_format() {
    assert!(parse_asctime("not a date").is_none());
    assert!(parse_asctime("Sun Nov").is_none());
}

#[test]
fn path_matches_trailing_slash() {
    assert!(path_matches("/api/v1", "/api/"));
    assert!(!path_matches("/api-v2", "/api/"));
}

#[test]
fn path_matches_exact_with_subpath() {
    assert!(path_matches("/foo/bar", "/foo"));
    assert!(!path_matches("/foobar", "/foo"));
}

#[test]
fn default_cookie_path_rfc6265() {
    assert_eq!(default_cookie_path("/api/v1/users"), "/api/v1");
    assert_eq!(default_cookie_path("/api"), "/");
    assert_eq!(default_cookie_path("/"), "/");
    assert_eq!(default_cookie_path(""), "/");
    assert_eq!(default_cookie_path("relative"), "/");
    assert_eq!(default_cookie_path("/a/b/c"), "/a/b");
}

#[test]
fn cookie_without_path_gets_default_path() {
    let cookie = parse_set_cookie("a=b", "example.com", "/api/v1/users").unwrap();
    assert_eq!(cookie.path, "/api/v1");
}

#[test]
fn cookie_with_explicit_path_preserved() {
    let cookie = parse_set_cookie("a=b; Path=/custom", "example.com", "/api/v1/users").unwrap();
    assert_eq!(cookie.path, "/custom");
}

#[test]
fn domain_mismatch_rejects_cookie() {
    assert!(parse_set_cookie("a=b; Domain=evil.com", "example.com", "/").is_none());
}

#[test]
fn domain_match_parent_accepts_cookie() {
    let cookie = parse_set_cookie("a=b; Domain=example.com", "sub.example.com", "/").unwrap();
    assert_eq!(cookie.domain.as_deref(), Some("example.com"));
}

#[test]
fn domain_match_exact_accepts_cookie() {
    let cookie = parse_set_cookie("a=b; Domain=example.com", "example.com", "/").unwrap();
    assert_eq!(cookie.domain.as_deref(), Some("example.com"));
}

#[test]
fn domain_mismatch_not_stored_in_jar() {
    let jar = CookieJar::new();
    let headers = headers_with_cookies(&["a=b; Domain=evil.com"]);
    jar.store_from_response("example.com", "/", &headers);
    assert!(jar.cookies().is_empty());
}

/// BUG(#139): SameSite=Strict cookies must not be sent on cross-site requests.
#[test]
fn samesite_strict_not_sent_cross_site() {
    let jar = CookieJar::new();
    jar.store_from_response(
        "example.com",
        "/",
        &headers_with_cookies(&["session=abc; SameSite=Strict"]),
    );

    // Same-site request: cookie should be sent
    let mut req_headers = HeaderMap::new();
    jar.apply_to_request(
        "example.com",
        false,
        "/",
        Some("example.com"),
        &mut req_headers,
    );
    assert_eq!(req_headers.get(COOKIE).unwrap(), "session=abc");

    // Cross-site request: cookie must NOT be sent
    let mut req_headers = HeaderMap::new();
    jar.apply_to_request(
        "example.com",
        false,
        "/",
        Some("other.com"),
        &mut req_headers,
    );
    assert!(
        req_headers.get(COOKIE).is_none(),
        "SameSite=Strict cookie must not be sent on cross-site request"
    );
}

/// BUG(#139): SameSite=Lax cookies must not be sent on cross-site non-GET requests.
/// For an HTTP client, we treat all cross-site requests as "unsafe" (not top-level nav).
#[test]
fn samesite_lax_not_sent_cross_site() {
    let jar = CookieJar::new();
    jar.store_from_response(
        "example.com",
        "/",
        &headers_with_cookies(&["token=xyz; SameSite=Lax"]),
    );

    // Same-site: cookie should be sent
    let mut req_headers = HeaderMap::new();
    jar.apply_to_request(
        "example.com",
        false,
        "/",
        Some("example.com"),
        &mut req_headers,
    );
    assert_eq!(req_headers.get(COOKIE).unwrap(), "token=xyz");

    // Cross-site: Lax cookies not sent (HTTP client has no concept of top-level navigation)
    let mut req_headers = HeaderMap::new();
    jar.apply_to_request(
        "example.com",
        false,
        "/",
        Some("other.com"),
        &mut req_headers,
    );
    assert!(
        req_headers.get(COOKIE).is_none(),
        "SameSite=Lax cookie must not be sent on cross-site request"
    );
}

/// BUG(#139): SameSite=None cookies are always sent, even cross-site.
#[test]
fn samesite_none_sent_cross_site() {
    let jar = CookieJar::new();
    jar.store_from_response(
        "example.com",
        "/",
        &headers_with_cookies(&["tracking=123; SameSite=None; Secure"]),
    );

    // Cross-site but SameSite=None: cookie should still be sent
    let mut req_headers = HeaderMap::new();
    jar.apply_to_request(
        "example.com",
        true,
        "/",
        Some("other.com"),
        &mut req_headers,
    );
    assert_eq!(req_headers.get(COOKIE).unwrap(), "tracking=123");
}

/// BUG(#139): Cookies without SameSite attribute default to Lax behavior.
#[test]
fn samesite_default_lax_cross_site() {
    let jar = CookieJar::new();
    jar.store_from_response("example.com", "/", &headers_with_cookies(&["legacy=old"]));

    // Same-site: sent
    let mut req_headers = HeaderMap::new();
    jar.apply_to_request(
        "example.com",
        false,
        "/",
        Some("example.com"),
        &mut req_headers,
    );
    assert_eq!(req_headers.get(COOKIE).unwrap(), "legacy=old");

    // Cross-site: default-Lax means not sent
    let mut req_headers = HeaderMap::new();
    jar.apply_to_request(
        "example.com",
        false,
        "/",
        Some("other.com"),
        &mut req_headers,
    );
    assert!(
        req_headers.get(COOKIE).is_none(),
        "Cookie without SameSite should default to Lax (not sent cross-site)"
    );
}

/// When site_for_cookies is None, SameSite is not enforced (backward compat).
#[test]
fn samesite_not_enforced_when_no_site_for_cookies() {
    let jar = CookieJar::new();
    jar.store_from_response(
        "example.com",
        "/",
        &headers_with_cookies(&["session=abc; SameSite=Strict"]),
    );

    let mut req_headers = HeaderMap::new();
    jar.apply_to_request("example.com", false, "/", None, &mut req_headers);
    assert_eq!(
        req_headers.get(COOKIE).unwrap(),
        "session=abc",
        "When site_for_cookies is None, SameSite should not be enforced"
    );
}

/// BUG(#139): Subdomain of same registrable domain counts as same-site.
#[test]
fn samesite_subdomain_is_same_site() {
    let jar = CookieJar::new();
    jar.store_from_response(
        "example.com",
        "/",
        &headers_with_cookies(&["session=abc; SameSite=Strict; Domain=example.com"]),
    );

    // Request from sub.example.com to example.com — same site
    let mut req_headers = HeaderMap::new();
    jar.apply_to_request(
        "api.example.com",
        false,
        "/",
        Some("www.example.com"),
        &mut req_headers,
    );
    assert_eq!(
        req_headers.get(COOKIE).unwrap(),
        "session=abc",
        "Subdomains of same registrable domain should be same-site"
    );
}

/// BUG(#152): Cookie set from different subdomains with same Domain attribute
/// should be deduplicated, not stored twice.
#[test]
fn cross_subdomain_cookie_deduplication() {
    let jar = CookieJar::new();
    // Set session=old from api.example.com with Domain=example.com
    let headers = headers_with_cookies(&["session=old; Domain=example.com"]);
    jar.store_from_response("api.example.com", "/", &headers);

    // Set session=new from www.example.com with Domain=example.com
    // This should UPDATE the existing cookie, not create a duplicate
    let headers = headers_with_cookies(&["session=new; Domain=example.com"]);
    jar.store_from_response("www.example.com", "/", &headers);

    let mut req_headers = HeaderMap::new();
    jar.apply_to_request("example.com", false, "/", None, &mut req_headers);
    let cookie_str = req_headers.get(COOKIE).unwrap().to_str().unwrap();
    assert_eq!(
        cookie_str, "session=new",
        "cookie should be deduplicated: got '{cookie_str}' (expected only session=new)"
    );
}
