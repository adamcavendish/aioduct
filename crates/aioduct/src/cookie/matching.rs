pub(super) fn path_matches(request_path: &str, cookie_path: &str) -> bool {
    if request_path == cookie_path {
        return true;
    }
    if !request_path.starts_with(cookie_path) {
        return false;
    }
    cookie_path.ends_with('/') || request_path.as_bytes().get(cookie_path.len()) == Some(&b'/')
}

/// Compute the default cookie path per RFC 6265 Section 5.1.4.
pub(super) fn default_cookie_path(request_path: &str) -> String {
    if request_path.is_empty() || !request_path.starts_with('/') {
        return "/".to_owned();
    }
    match request_path.rfind('/') {
        Some(0) | None => "/".to_owned(),
        Some(pos) => request_path[..pos].to_owned(),
    }
}

pub(super) fn domain_matches(request_domain: &str, cookie_domain: &str) -> bool {
    if request_domain.eq_ignore_ascii_case(cookie_domain) {
        return true;
    }
    let rd = request_domain.as_bytes();
    let cd = cookie_domain.as_bytes();
    if rd.len() <= cd.len() {
        return false;
    }
    let suffix_start = rd.len() - cd.len();
    rd[suffix_start - 1] == b'.' && rd[suffix_start..].eq_ignore_ascii_case(cd)
}

/// Determine if two domains are "same-site" by comparing their registrable
/// domain (approximated as the last two domain labels). This is a simplified
/// heuristic that works for common single-part TLDs (.com, .org, .net) but
/// does NOT consult the public suffix list — country-code second-level domains
/// like .co.uk or .com.au are not handled correctly and will be treated as
/// registrable domains themselves.
pub(super) fn is_same_site(domain_a: &str, domain_b: &str) -> bool {
    registrable_domain(domain_a).eq_ignore_ascii_case(registrable_domain(domain_b))
}

fn registrable_domain(domain: &str) -> &str {
    let domain = domain.strip_suffix('.').unwrap_or(domain);
    let mut labels = domain.rsplit('.');
    let tld = labels.next().unwrap_or(domain);
    match labels.next() {
        Some(sld) => {
            let start = domain.len() - tld.len() - 1 - sld.len();
            &domain[start..]
        }
        None => domain,
    }
}
