use super::*;

use super::settings::env_proxy;
use http::Uri;

/// Serializes env var mutations to prevent flakiness under `--test-threads > 1`.
static ENV_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[test]
fn no_proxy_wildcard_matches_everything() {
    let np = NoProxy::new("*");
    assert!(np.matches("anything.example.com"));
    assert!(np.matches("127.0.0.1"));
    assert!(np.matches("2001:db8::1"));
}

#[test]
fn no_proxy_exact_match() {
    let np = NoProxy::new("example.com");
    assert!(np.matches("example.com"));
    assert!(!np.matches("other.com"));
}

#[test]
fn no_proxy_suffix_with_leading_dot() {
    let np = NoProxy::new(".example.com");
    assert!(np.matches("sub.example.com"));
    assert!(np.matches("deep.sub.example.com"));
    assert!(!np.matches("example.com"));
}

#[test]
fn no_proxy_suffix_without_leading_dot() {
    let np = NoProxy::new("example.com");
    assert!(np.matches("sub.example.com"));
    assert!(np.matches("example.com"));
}

#[test]
fn no_proxy_case_insensitive() {
    let np = NoProxy::new("Example.COM");
    assert!(np.matches("EXAMPLE.com"));
    assert!(np.matches("example.com"));
}

#[test]
fn no_proxy_multiple_rules() {
    let np = NoProxy::new("a.com, b.com, .c.com");
    assert!(np.matches("a.com"));
    assert!(np.matches("b.com"));
    assert!(np.matches("sub.c.com"));
    assert!(!np.matches("d.com"));
}

#[test]
fn no_proxy_ip_address() {
    let np = NoProxy::new("127.0.0.1");
    assert!(np.matches("127.0.0.1"));
    assert!(!np.matches("127.0.0.2"));
}

#[test]
fn no_proxy_ipv6_exact_address() {
    let np = NoProxy::new("2001:db8::1");
    assert!(np.matches("2001:db8::1"));
    assert!(np.matches("[2001:db8::1]"));
    assert!(!np.matches("2001:db8::2"));
}

#[test]
fn no_proxy_ipv4_cidr() {
    let np = NoProxy::new("10.0.0.0/8");
    assert!(np.matches("10.1.2.3"));
    assert!(np.matches("10.255.255.255"));
    assert!(!np.matches("11.0.0.1"));
    assert!(!np.matches("example.com"));
}

#[test]
fn no_proxy_ipv6_cidr() {
    let np = NoProxy::new("2001:db8::/32");
    assert!(np.matches("2001:db8::1"));
    assert!(np.matches("[2001:db8:abcd::1]"));
    assert!(!np.matches("2001:db9::1"));
    assert!(!np.matches("example.com"));
}

#[test]
fn no_proxy_empty_matches_nothing() {
    let np = NoProxy::new("");
    assert!(!np.matches("anything"));
}

#[test]
fn no_proxy_with_port_matches_specific_port() {
    let np = NoProxy::new("example.com:8080");
    assert!(np.matches_with_port("example.com", Some(8080)));
    assert!(np.matches("example.com:8080"));
    assert!(!np.matches_with_port("example.com", Some(9090)));
    assert!(!np.matches_with_port("example.com", None));
}

#[test]
fn no_proxy_host_port_matches_subdomain_on_same_port() {
    let np = NoProxy::new("example.com:8080");
    assert!(np.matches_with_port("sub.example.com", Some(8080)));
    assert!(!np.matches_with_port("sub.example.com", Some(8081)));
    assert!(!np.matches_with_port("other.com", Some(8080)));
}

#[test]
fn no_proxy_without_port_matches_any_port() {
    let np = NoProxy::new("example.com");
    assert!(np.matches_with_port("example.com", Some(8080)));
    assert!(np.matches_with_port("example.com", Some(443)));
    assert!(np.matches_with_port("example.com", None));
}

#[test]
fn no_proxy_port_with_subdomain() {
    let np = NoProxy::new(".example.com:8080");
    assert!(np.matches_with_port("sub.example.com", Some(8080)));
    assert!(!np.matches_with_port("sub.example.com", Some(9090)));
}

#[test]
fn no_proxy_bracketed_ipv6_with_port() {
    let np = NoProxy::new("[2001:db8::1]:443");
    assert!(np.matches_with_port("2001:db8::1", Some(443)));
    assert!(np.matches_with_port("[2001:db8::1]", Some(443)));
    assert!(np.matches("[2001:db8::1]:443"));
    assert!(!np.matches_with_port("2001:db8::1", Some(8443)));
    assert!(!np.matches_with_port("2001:db8::2", Some(443)));
}

#[test]
fn no_proxy_cidr_with_port_matches_specific_port() {
    let np = NoProxy::new("10.0.0.0/8:443,[2001:db8::/32]:8443");
    assert!(np.matches_with_port("10.1.2.3", Some(443)));
    assert!(!np.matches_with_port("10.1.2.3", Some(80)));
    assert!(np.matches_with_port("2001:db8::1", Some(8443)));
    assert!(!np.matches_with_port("2001:db8::1", Some(443)));
}

#[test]
fn no_proxy_invalid_cidr_prefix_does_not_match_ip() {
    let np = NoProxy::new("10.0.0.0/33,2001:db8::/129");
    assert!(!np.matches("10.1.2.3"));
    assert!(!np.matches("2001:db8::1"));
}

#[test]
fn proxy_config_http_valid() {
    let cfg = ProxyConfig::http("http://proxy:8080").unwrap();
    assert_eq!(cfg.scheme, ProxyScheme::Http);
    assert_eq!(cfg.default_port(), 80);
}

#[test]
fn proxy_config_http_wrong_scheme() {
    assert!(ProxyConfig::http("https://proxy:8080").is_err());
}

#[test]
fn proxy_config_socks5_valid() {
    let cfg = ProxyConfig::socks5("socks5://proxy:1080").unwrap();
    assert_eq!(cfg.scheme, ProxyScheme::Socks5);
    assert_eq!(cfg.default_port(), 1080);
}

#[test]
fn proxy_config_socks5_wrong_scheme() {
    assert!(ProxyConfig::socks5("http://proxy:1080").is_err());
}

#[test]
fn proxy_config_socks4_valid() {
    let cfg = ProxyConfig::socks4("socks4://proxy:1080").unwrap();
    assert_eq!(cfg.scheme, ProxyScheme::Socks4);
    assert_eq!(cfg.default_port(), 1080);
}

#[test]
fn proxy_config_socks4a_valid() {
    let cfg = ProxyConfig::socks4("socks4a://proxy:1080").unwrap();
    assert_eq!(cfg.scheme, ProxyScheme::Socks4);
}

#[test]
fn proxy_config_socks4_wrong_scheme() {
    assert!(ProxyConfig::socks4("http://proxy").is_err());
}

#[test]
fn proxy_config_socks5h_valid() {
    let cfg = ProxyConfig::socks5h("socks5h://proxy:1080").unwrap();
    assert_eq!(cfg.scheme, ProxyScheme::Socks5h);
    assert_eq!(cfg.default_port(), 1080);
}

#[test]
fn proxy_config_socks5h_wrong_scheme() {
    assert!(ProxyConfig::socks5h("socks5://proxy:1080").is_err());
    assert!(ProxyConfig::socks5h("http://proxy:1080").is_err());
}

#[test]
fn proxy_config_https_valid() {
    let cfg = ProxyConfig::https("https://proxy:443").unwrap();
    assert_eq!(cfg.scheme, ProxyScheme::Https);
    assert_eq!(cfg.default_port(), 443);
}

#[test]
fn proxy_config_https_wrong_scheme() {
    assert!(ProxyConfig::https("http://proxy:443").is_err());
    assert!(ProxyConfig::https("socks5://proxy:443").is_err());
}

#[test]
fn env_proxy_https_parsed_correctly() {
    // Verify https:// proxy URLs map to ProxyScheme::Https (not Http)
    let cfg = ProxyConfig::https("https://secure-proxy.example.com:443").unwrap();
    assert_eq!(cfg.scheme, ProxyScheme::Https);
}

#[test]
fn env_proxy_socks5h_parsed_correctly() {
    let cfg = ProxyConfig::socks5h("socks5h://dns-proxy.example.com:1080").unwrap();
    assert_eq!(cfg.scheme, ProxyScheme::Socks5h);
}

#[test]
fn proxy_config_basic_auth() {
    let cfg = ProxyConfig::http("http://proxy:8080")
        .unwrap()
        .basic_auth("user", "pass");
    let header = cfg.connect_header("target:443");
    assert!(header.is_some());
    assert!(header.unwrap().starts_with("Basic "));
}

#[test]
fn proxy_config_no_auth_connect_header() {
    let cfg = ProxyConfig::http("http://proxy:8080").unwrap();
    assert!(cfg.connect_header("target:443").is_none());
}

#[test]
fn proxy_config_authority() {
    let cfg = ProxyConfig::http("http://proxy:8080").unwrap();
    let auth = cfg.authority().unwrap();
    assert_eq!(auth.to_string(), "proxy:8080");
}

#[test]
fn proxy_settings_all() {
    let proxy = ProxyConfig::http("http://proxy:8080").unwrap();
    let settings = ProxySettings::all(proxy);
    assert!(settings.http_proxy.is_some());
    assert!(settings.https_proxy.is_some());
}

#[test]
fn proxy_settings_builder() {
    let settings = ProxySettings::default()
        .http(ProxyConfig::http("http://h:80").unwrap())
        .https(ProxyConfig::http("http://s:80").unwrap())
        .no_proxy(NoProxy::new("localhost"));
    assert!(settings.http_proxy.is_some());
    assert!(settings.https_proxy.is_some());
    assert!(settings.no_proxy.matches("localhost"));
}

#[test]
fn proxy_for_no_proxy_bypass() {
    let settings = ProxySettings::all(ProxyConfig::http("http://p:80").unwrap())
        .no_proxy(NoProxy::new("localhost"));
    let uri: Uri = "http://localhost/path".parse().unwrap();
    assert!(settings.proxy_for(&uri).is_none());

    let uri: Uri = "http://other.com/path".parse().unwrap();
    assert!(settings.proxy_for(&uri).is_some());
}

#[test]
fn proxy_for_scheme_dispatch() {
    let settings = ProxySettings::default()
        .http(ProxyConfig::http("http://http-proxy:80").unwrap())
        .https(ProxyConfig::http("http://https-proxy:80").unwrap());

    let http_uri: Uri = "http://example.com/".parse().unwrap();
    let https_uri: Uri = "https://example.com/".parse().unwrap();

    let http_proxy = settings.proxy_for(&http_uri).unwrap();
    assert!(http_proxy.uri.to_string().contains("http-proxy"));

    let https_proxy = settings.proxy_for(&https_uri).unwrap();
    assert!(https_proxy.uri.to_string().contains("https-proxy"));
}

#[test]
fn proxy_for_custom_takes_priority() {
    let settings =
        ProxySettings::all(ProxyConfig::http("http://p:80").unwrap()).custom(|_uri: &Uri| None);
    let uri: Uri = "http://example.com/".parse().unwrap();
    assert!(settings.proxy_for(&uri).is_none());
}

#[test]
fn proxy_config_http_invalid_uri() {
    assert!(ProxyConfig::http("://bad").is_err());
}

#[test]
fn proxy_config_socks5_invalid_uri() {
    assert!(ProxyConfig::socks5("://bad").is_err());
}

#[test]
fn proxy_config_socks4_invalid_uri() {
    assert!(ProxyConfig::socks4("://bad").is_err());
}

#[test]
fn proxy_settings_debug() {
    let settings = ProxySettings::all(ProxyConfig::http("http://p:80").unwrap());
    let dbg = format!("{settings:?}");
    assert!(dbg.contains("ProxySettings"));
    assert!(dbg.contains("http_proxy"));
    assert!(dbg.contains("https_proxy"));
    assert!(dbg.contains("no_proxy"));
}

#[test]
fn proxy_settings_debug_with_custom() {
    let settings =
        ProxySettings::all(ProxyConfig::http("http://p:80").unwrap()).custom(|_: &Uri| None);
    let dbg = format!("{settings:?}");
    assert!(dbg.contains("custom"));
}

#[test]
fn proxy_for_unknown_scheme_uses_http_proxy() {
    let settings = ProxySettings::default()
        .http(ProxyConfig::http("http://http-proxy:80").unwrap())
        .https(ProxyConfig::http("http://https-proxy:80").unwrap());
    let uri: Uri = "ftp://example.com/".parse().unwrap();
    let proxy = settings.proxy_for(&uri).unwrap();
    assert!(proxy.uri.to_string().contains("http-proxy"));
}

#[test]
fn proxy_for_no_host_still_checks_scheme() {
    let settings = ProxySettings::default().http(ProxyConfig::http("http://hp:80").unwrap());
    let uri: Uri = "http://example.com/path".parse().unwrap();
    let proxy = settings.proxy_for(&uri);
    assert!(proxy.is_some());
}

#[test]
fn env_proxy_socks5() {
    let _guard = ENV_MUTEX.lock().unwrap();
    unsafe { std::env::set_var("TEST_SOCKS5_UPPER", "socks5://proxy:1080") };
    let result = env_proxy("TEST_SOCKS5_UPPER", "test_socks5_lower");
    assert!(result.is_some());
    assert_eq!(result.unwrap().scheme, ProxyScheme::Socks5);
    unsafe { std::env::remove_var("TEST_SOCKS5_UPPER") };
}

#[test]
fn env_proxy_socks4() {
    let _guard = ENV_MUTEX.lock().unwrap();
    unsafe { std::env::set_var("TEST_SOCKS4_UPPER", "socks4://proxy:1080") };
    let result = env_proxy("TEST_SOCKS4_UPPER", "test_socks4_lower");
    assert!(result.is_some());
    assert_eq!(result.unwrap().scheme, ProxyScheme::Socks4);
    unsafe { std::env::remove_var("TEST_SOCKS4_UPPER") };
}

#[test]
fn env_proxy_socks4a() {
    let _guard = ENV_MUTEX.lock().unwrap();
    unsafe { std::env::set_var("TEST_SOCKS4A_UPPER", "socks4a://proxy:1080") };
    let result = env_proxy("TEST_SOCKS4A_UPPER", "test_socks4a_lower");
    assert!(result.is_some());
    assert_eq!(result.unwrap().scheme, ProxyScheme::Socks4);
    unsafe { std::env::remove_var("TEST_SOCKS4A_UPPER") };
}

#[test]
fn env_proxy_http() {
    let _guard = ENV_MUTEX.lock().unwrap();
    unsafe { std::env::set_var("TEST_HTTP_PROXY_UPPER", "http://proxy:8080") };
    let result = env_proxy("TEST_HTTP_PROXY_UPPER", "test_http_proxy_lower");
    assert!(result.is_some());
    assert_eq!(result.unwrap().scheme, ProxyScheme::Http);
    unsafe { std::env::remove_var("TEST_HTTP_PROXY_UPPER") };
}

#[test]
fn env_proxy_https_scheme() {
    let _guard = ENV_MUTEX.lock().unwrap();
    unsafe { std::env::set_var("TEST_HTTPS_PROXY_VAL", "https://secure-proxy:443") };
    let result = env_proxy("TEST_HTTPS_PROXY_VAL", "test_https_proxy_val_lower");
    assert!(result.is_some());
    let cfg = result.unwrap();
    assert_eq!(cfg.scheme, ProxyScheme::Https);
    assert!(cfg.uri.to_string().contains("secure-proxy"));
    unsafe { std::env::remove_var("TEST_HTTPS_PROXY_VAL") };
}

#[test]
fn env_proxy_bare_hostname() {
    let _guard = ENV_MUTEX.lock().unwrap();
    unsafe { std::env::set_var("TEST_BARE_HOST_PROXY", "proxy-host:3128") };
    let result = env_proxy("TEST_BARE_HOST_PROXY", "test_bare_host_proxy_lower");
    assert!(result.is_some());
    let cfg = result.unwrap();
    assert_eq!(cfg.scheme, ProxyScheme::Http);
    assert!(cfg.uri.to_string().contains("proxy-host:3128"));
    unsafe { std::env::remove_var("TEST_BARE_HOST_PROXY") };
}

#[test]
fn env_proxy_empty_value() {
    let _guard = ENV_MUTEX.lock().unwrap();
    unsafe { std::env::set_var("TEST_EMPTY_PROXY", "") };
    let result = env_proxy("TEST_EMPTY_PROXY", "test_empty_proxy_lower");
    assert!(result.is_none());
    unsafe { std::env::remove_var("TEST_EMPTY_PROXY") };
}

#[test]
fn env_proxy_missing() {
    let _guard = ENV_MUTEX.lock().unwrap();
    unsafe { std::env::remove_var("TEST_MISSING_UPPER") };
    unsafe { std::env::remove_var("test_missing_lower") };
    let result = env_proxy("TEST_MISSING_UPPER", "test_missing_lower");
    assert!(result.is_none());
}

#[test]
fn env_proxy_lowercase_fallback() {
    let _guard = ENV_MUTEX.lock().unwrap();
    unsafe { std::env::remove_var("TEST_LOWER_UPPER") };
    unsafe { std::env::set_var("test_lower_lower", "http://proxy:80") };
    let result = env_proxy("TEST_LOWER_UPPER", "test_lower_lower");
    assert!(result.is_some());
    unsafe { std::env::remove_var("test_lower_lower") };
}

#[test]
fn proxy_config_authority_missing() {
    let cfg = ProxyConfig {
        uri: "/just-a-path".parse().unwrap(),
        scheme: ProxyScheme::Http,
        auth: None,
    };
    assert!(cfg.authority().is_err());
}

#[test]
fn proxy_settings_default_is_empty() {
    let settings = ProxySettings::default();
    assert!(settings.http_proxy.is_none());
    assert!(settings.https_proxy.is_none());
    let uri: Uri = "http://example.com/".parse().unwrap();
    assert!(settings.proxy_for(&uri).is_none());
}

#[test]
fn custom_proxy_trait_with_closure() {
    let f = |uri: &Uri| -> Option<ProxyConfig> {
        if uri.host() == Some("proxied.com") {
            Some(ProxyConfig::http("http://p:80").unwrap())
        } else {
            None
        }
    };
    assert!(
        f.proxy_for(&"http://proxied.com/".parse().unwrap())
            .is_some()
    );
    assert!(f.proxy_for(&"http://other.com/".parse().unwrap()).is_none());
}

#[test]
fn no_proxy_takes_precedence_over_custom() {
    let settings = ProxySettings::all(ProxyConfig::http("http://p:80").unwrap())
        .no_proxy(NoProxy::new("localhost"))
        .custom(|_uri: &Uri| Some(ProxyConfig::http("http://custom:80").unwrap()));
    let uri: Uri = "http://localhost/path".parse().unwrap();
    assert!(
        settings.proxy_for(&uri).is_none(),
        "no_proxy should bypass even custom proxy"
    );
    let uri: Uri = "http://example.com/path".parse().unwrap();
    assert!(
        settings.proxy_for(&uri).is_some(),
        "non-bypassed host should use custom proxy"
    );
}

// --- URI userinfo extraction tests ---

#[test]
fn extract_uri_auth_with_password() {
    let cfg = ProxyConfig::http("http://user:pass@proxy:8080").unwrap();
    let auth = cfg.auth.as_ref().unwrap();
    assert_eq!(auth.username, "user");
    assert_eq!(auth.password, "pass");
}

#[test]
fn extract_uri_auth_username_only() {
    let cfg = ProxyConfig::http("http://user@proxy:8080").unwrap();
    let auth = cfg.auth.as_ref().unwrap();
    assert_eq!(auth.username, "user");
    assert_eq!(auth.password, "");
}

#[test]
fn extract_uri_auth_no_userinfo() {
    let cfg = ProxyConfig::http("http://proxy:8080").unwrap();
    assert!(cfg.auth.is_none());
}

#[test]
fn extract_uri_auth_percent_encoded() {
    let cfg = ProxyConfig::http("http://user%40dom:pass%3Aword@proxy:8080").unwrap();
    let auth = cfg.auth.as_ref().unwrap();
    assert_eq!(auth.username, "user@dom");
    assert_eq!(auth.password, "pass:word");
}

#[test]
fn extract_uri_auth_socks5() {
    let cfg = ProxyConfig::socks5("socks5://admin:secret@proxy:1080").unwrap();
    let auth = cfg.auth.as_ref().unwrap();
    assert_eq!(auth.username, "admin");
    assert_eq!(auth.password, "secret");
}

#[test]
fn extract_uri_auth_https_proxy() {
    let cfg = ProxyConfig::https("https://u:p@secure-proxy:443").unwrap();
    let auth = cfg.auth.as_ref().unwrap();
    assert_eq!(auth.username, "u");
    assert_eq!(auth.password, "p");
}

#[test]
fn extract_uri_auth_socks5h() {
    let cfg = ProxyConfig::socks5h("socks5h://a:b@proxy:1080").unwrap();
    let auth = cfg.auth.as_ref().unwrap();
    assert_eq!(auth.username, "a");
    assert_eq!(auth.password, "b");
}

#[test]
fn basic_auth_overrides_uri_auth() {
    let cfg = ProxyConfig::http("http://uri-user:uri-pass@proxy:8080")
        .unwrap()
        .basic_auth("override-user", "override-pass");
    let auth = cfg.auth.as_ref().unwrap();
    assert_eq!(auth.username, "override-user");
    assert_eq!(auth.password, "override-pass");
}

// --- ProxyChain tests ---

#[test]
fn proxy_chain_new_and_len() {
    let p1 = ProxyConfig::http("http://p1:8080").unwrap();
    let p2 = ProxyConfig::socks5("socks5://p2:1080").unwrap();
    let chain = ProxyChain::new(vec![p1, p2]);
    assert_eq!(chain.len(), 2);
    assert!(!chain.is_empty());
}

#[test]
fn proxy_chain_empty() {
    let chain = ProxyChain::new(vec![]);
    assert!(chain.is_empty());
    assert_eq!(chain.len(), 0);
    assert!(chain.first().is_none());
}

#[test]
fn proxy_chain_single() {
    let p = ProxyConfig::http("http://proxy:8080").unwrap();
    let chain = ProxyChain::single(p.clone());
    assert_eq!(chain.len(), 1);
    assert_eq!(chain.first().unwrap().default_port(), p.default_port());
}

#[test]
fn proxy_chain_first() {
    let p1 = ProxyConfig::http("http://first:8080").unwrap();
    let p2 = ProxyConfig::socks5("socks5://second:1080").unwrap();
    let chain = ProxyChain::new(vec![p1, p2]);
    let first = chain.first().unwrap();
    assert_eq!(first.default_port(), 80);
}

#[test]
fn proxy_chain_iter() {
    let p1 = ProxyConfig::http("http://p1:8080").unwrap();
    let p2 = ProxyConfig::socks5("socks5://p2:1080").unwrap();
    let chain = ProxyChain::new(vec![p1, p2]);
    let mut iter = chain.iter();
    assert_eq!(iter.next().unwrap().default_port(), 80);
    assert_eq!(iter.next().unwrap().default_port(), 1080);
    assert!(iter.next().is_none());
}

#[test]
fn proxy_chain_clone() {
    let chain = ProxyChain::new(vec![ProxyConfig::http("http://proxy:8080").unwrap()]);
    let cloned = chain.clone();
    assert_eq!(cloned.len(), chain.len());
}

// --- ProxySettings credential resolver tests ---

#[test]
fn proxy_for_resolves_credentials_when_missing() {
    let _guard = ENV_MUTEX.lock().unwrap();
    use crate::proxy_credential::EnvCredentialResolver;
    unsafe {
        std::env::set_var("AIODUCT_PROXY_USER", "envuser");
        std::env::set_var("AIODUCT_PROXY_PASS", "envpass");
    }
    let settings = ProxySettings::all(ProxyConfig::http("http://proxy:8080").unwrap())
        .proxy_credential_resolver(EnvCredentialResolver);
    let uri: Uri = "http://example.com/path".parse().unwrap();
    let proxy = settings.proxy_for(&uri).unwrap();
    let auth = proxy.auth.as_ref().unwrap();
    assert_eq!(auth.username, "envuser");
    assert_eq!(auth.password, "envpass");
    unsafe {
        std::env::remove_var("AIODUCT_PROXY_USER");
        std::env::remove_var("AIODUCT_PROXY_PASS");
    }
}

#[test]
fn proxy_for_does_not_override_existing_auth() {
    let _guard = ENV_MUTEX.lock().unwrap();
    use crate::proxy_credential::EnvCredentialResolver;
    unsafe {
        std::env::set_var("AIODUCT_PROXY_USER", "envuser");
        std::env::set_var("AIODUCT_PROXY_PASS", "envpass");
    }
    let settings = ProxySettings::all(
        ProxyConfig::http("http://proxy:8080")
            .unwrap()
            .basic_auth("explicit", "auth"),
    )
    .proxy_credential_resolver(EnvCredentialResolver);
    let uri: Uri = "http://example.com/path".parse().unwrap();
    let proxy = settings.proxy_for(&uri).unwrap();
    let auth = proxy.auth.as_ref().unwrap();
    assert_eq!(auth.username, "explicit");
    assert_eq!(auth.password, "auth");
    unsafe {
        std::env::remove_var("AIODUCT_PROXY_USER");
        std::env::remove_var("AIODUCT_PROXY_PASS");
    }
}

#[test]
fn proxy_for_env_credential_resolver() {
    struct StaticResolver(&'static str, &'static str);
    impl crate::proxy_credential::CredentialResolver for StaticResolver {
        fn resolve(&self, _key: &str) -> Option<(String, String)> {
            Some((self.0.to_string(), self.1.to_string()))
        }
    }

    let settings = ProxySettings::all(ProxyConfig::http("http://proxy:9090").unwrap())
        .proxy_credential_resolver(StaticResolver("resolved", "creds"));
    let uri: Uri = "http://example.com".parse().unwrap();
    let proxy = settings.proxy_for(&uri).unwrap();
    let auth = proxy.auth.unwrap();
    assert_eq!(auth.username, "resolved");
    assert_eq!(auth.password, "creds");
}

// --- detect_from_url tests ---

#[test]
fn detect_from_url_empty_returns_none() {
    assert!(ProxyConfig::detect_from_url("").is_none());
}

#[test]
fn detect_from_url_http() {
    let cfg = ProxyConfig::detect_from_url("http://proxy:8080").unwrap();
    assert_eq!(cfg.scheme, ProxyScheme::Http);
}

#[test]
fn detect_from_url_https() {
    let cfg = ProxyConfig::detect_from_url("https://secure-proxy:443").unwrap();
    assert_eq!(cfg.scheme, ProxyScheme::Https);
}

#[test]
fn detect_from_url_socks5() {
    let cfg = ProxyConfig::detect_from_url("socks5://proxy:1080").unwrap();
    assert_eq!(cfg.scheme, ProxyScheme::Socks5);
}

#[test]
fn detect_from_url_socks5h() {
    let cfg = ProxyConfig::detect_from_url("socks5h://proxy:1080").unwrap();
    assert_eq!(cfg.scheme, ProxyScheme::Socks5h);
}

#[test]
fn detect_from_url_socks4() {
    let cfg = ProxyConfig::detect_from_url("socks4://proxy:1080").unwrap();
    assert_eq!(cfg.scheme, ProxyScheme::Socks4);
}

#[test]
fn detect_from_url_socks4a() {
    let cfg = ProxyConfig::detect_from_url("socks4a://proxy:1080").unwrap();
    assert_eq!(cfg.scheme, ProxyScheme::Socks4);
}

#[test]
fn detect_from_url_bare_hostname() {
    let cfg = ProxyConfig::detect_from_url("proxy-host:3128").unwrap();
    assert_eq!(cfg.scheme, ProxyScheme::Http);
    assert!(cfg.uri.to_string().contains("proxy-host:3128"));
}

#[test]
fn detect_from_url_bare_ip() {
    let cfg = ProxyConfig::detect_from_url("127.0.0.1:8888").unwrap();
    assert_eq!(cfg.scheme, ProxyScheme::Http);
}

#[test]
fn detect_from_url_bare_hostname_no_port() {
    let cfg = ProxyConfig::detect_from_url("proxy").unwrap();
    assert_eq!(cfg.scheme, ProxyScheme::Http);
}

#[test]
fn detect_from_url_extracts_auth() {
    let cfg = ProxyConfig::detect_from_url("http://user:pass@proxy:8080").unwrap();
    let auth = cfg.auth.unwrap();
    assert_eq!(auth.username, "user");
    assert_eq!(auth.password, "pass");
}

#[test]
fn detect_from_url_extracts_auth_socks5() {
    let cfg = ProxyConfig::detect_from_url("socks5://admin:secret@proxy:1080").unwrap();
    let auth = cfg.auth.unwrap();
    assert_eq!(auth.username, "admin");
    assert_eq!(auth.password, "secret");
}

#[test]
fn detect_from_url_extracts_auth_https() {
    let cfg = ProxyConfig::detect_from_url("https://u:p@secure-proxy:443").unwrap();
    let auth = cfg.auth.unwrap();
    assert_eq!(auth.username, "u");
    assert_eq!(auth.password, "p");
}

#[test]
fn detect_from_url_extracts_auth_socks5h() {
    let cfg = ProxyConfig::detect_from_url("socks5h://a:b@proxy:1080").unwrap();
    let auth = cfg.auth.unwrap();
    assert_eq!(auth.username, "a");
    assert_eq!(auth.password, "b");
}
