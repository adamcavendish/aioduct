use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use http::HeaderMap;
use http::header::{COOKIE, SET_COOKIE};

#[derive(Clone, Debug)]
pub struct Cookie {
    pub name: String,
    pub value: String,
    pub domain: Option<String>,
    pub path: Option<String>,
    pub secure: bool,
    pub http_only: bool,
}

#[derive(Clone, Default)]
pub struct CookieJar {
    inner: Arc<Mutex<HashMap<String, Vec<Cookie>>>>,
}

impl CookieJar {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn store_from_response(&self, domain: &str, headers: &HeaderMap) {
        let mut jar = self.inner.lock().unwrap();
        let cookies = jar.entry(domain.to_owned()).or_default();

        for value in headers.get_all(SET_COOKIE) {
            if let Ok(s) = value.to_str() {
                if let Some(cookie) = parse_set_cookie(s, domain) {
                    if let Some(existing) = cookies.iter_mut().find(|c| c.name == cookie.name) {
                        *existing = cookie;
                    } else {
                        cookies.push(cookie);
                    }
                }
            }
        }
    }

    pub fn apply_to_request(&self, domain: &str, is_secure: bool, headers: &mut HeaderMap) {
        let jar = self.inner.lock().unwrap();

        if let Some(cookies) = jar.get(domain) {
            let matching: Vec<&Cookie> =
                cookies.iter().filter(|c| !c.secure || is_secure).collect();

            if matching.is_empty() {
                return;
            }

            let cookie_header: String = matching
                .iter()
                .map(|c| format!("{}={}", c.name, c.value))
                .collect::<Vec<_>>()
                .join("; ");

            if let Ok(value) = cookie_header.parse() {
                headers.insert(COOKIE, value);
            }
        }
    }

    pub fn clear(&self) {
        self.inner.lock().unwrap().clear();
    }
}

fn parse_set_cookie(header: &str, request_domain: &str) -> Option<Cookie> {
    let mut parts = header.split(';');
    let name_value = parts.next()?.trim();
    let (name, value) = name_value.split_once('=')?;

    let name = name.trim().to_owned();
    let value = value.trim().to_owned();

    if name.is_empty() {
        return None;
    }

    let mut domain = None;
    let mut path = None;
    let mut secure = false;
    let mut http_only = false;

    for attr in parts {
        let attr = attr.trim();
        let lower = attr.to_lowercase();

        if lower == "secure" {
            secure = true;
        } else if lower == "httponly" {
            http_only = true;
        } else if let Some(val) = lower.strip_prefix("domain=") {
            domain = Some(val.trim_start_matches('.').to_owned());
        } else if let Some(val) = lower.strip_prefix("path=") {
            path = Some(val.to_owned());
        }
    }

    if domain.is_none() {
        domain = Some(request_domain.to_owned());
    }

    Some(Cookie {
        name,
        value,
        domain,
        path,
        secure,
        http_only,
    })
}
