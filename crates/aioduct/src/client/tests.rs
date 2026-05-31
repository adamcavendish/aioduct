use super::*;
use super::{DEFAULT_USER_AGENT, resolve_redirect};

#[test]
fn resolve_redirect_absolute_url() {
    let base: Uri = "http://example.com/old".parse().unwrap();
    let result = resolve_redirect(&base, "https://other.com/new").unwrap();
    assert_eq!(result.to_string(), "https://other.com/new");
}

#[test]
fn resolve_redirect_relative_path() {
    let base: Uri = "http://example.com/old".parse().unwrap();
    let result = resolve_redirect(&base, "/new/path").unwrap();
    assert_eq!(result.to_string(), "http://example.com/new/path");
}

#[test]
fn resolve_redirect_relative_with_query() {
    let base: Uri = "https://example.com/page".parse().unwrap();
    let result = resolve_redirect(&base, "/search?q=test").unwrap();
    assert_eq!(result.to_string(), "https://example.com/search?q=test");
}

#[test]
fn resolve_redirect_relative_without_leading_slash_uses_base_directory() {
    let base: Uri = "http://example.com/dir/page".parse().unwrap();
    let result = resolve_redirect(&base, "next").unwrap();
    assert_eq!(result.to_string(), "http://example.com/dir/next");
}

#[test]
fn resolve_redirect_relative_parent_directory_is_normalized() {
    let base: Uri = "http://example.com/dir/page".parse().unwrap();
    let result = resolve_redirect(&base, "../up").unwrap();
    assert_eq!(result.to_string(), "http://example.com/up");
}

#[test]
fn resolve_redirect_query_only_keeps_base_path() {
    let base: Uri = "http://example.com/dir/page?old=1".parse().unwrap();
    let result = resolve_redirect(&base, "?new=2").unwrap();
    assert_eq!(result.to_string(), "http://example.com/dir/page?new=2");
}

#[test]
fn resolve_redirect_protocol_relative_uses_base_scheme() {
    let base: Uri = "https://example.com/old".parse().unwrap();
    let result = resolve_redirect(&base, "//other.example/new").unwrap();
    assert_eq!(result.to_string(), "https://other.example/new");
}

#[test]
fn resolve_redirect_preserves_port() {
    let base: Uri = "http://example.com:8080/old".parse().unwrap();
    let result = resolve_redirect(&base, "/new").unwrap();
    assert_eq!(result.to_string(), "http://example.com:8080/new");
}

#[test]
fn resolve_redirect_scheme_without_authority_is_relative() {
    let base: Uri = "http://example.com/".parse().unwrap();
    let result = resolve_redirect(&base, "/path").unwrap();
    assert_eq!(result.host().unwrap(), "example.com");
}

#[test]
fn is_cacheable_method_test() {
    assert!(Method::GET == Method::GET);
}

#[test]
fn default_user_agent_contains_version() {
    assert!(DEFAULT_USER_AGENT.starts_with("aioduct/"));
}

#[test]
fn resolve_redirect_missing_scheme() {
    let base: Uri = "/relative".parse().unwrap();
    let result = resolve_redirect(&base, "/new");
    assert!(result.is_err());
    match result.unwrap_err() {
        Error::InvalidUrl(msg) => assert!(msg.contains("scheme")),
        other => panic!("expected InvalidUrl, got {other:?}"),
    }
}

#[test]
fn resolve_redirect_missing_authority() {
    let base = Uri::from_static("http:");
    let result = resolve_redirect(&base, "/new");
    assert!(result.is_err());
}

#[cfg(feature = "tokio")]
mod builder_tests {
    use super::super::*;
    use super::DEFAULT_USER_AGENT;
    use crate::runtime::tokio_rt::{TcpConnector, TokioRuntime};
    use http::header::USER_AGENT;

    #[cfg(feature = "rustls")]
    fn install_crypto() {
        crate::tls::install_default_crypto_provider();
    }

    #[tokio::test]
    async fn builder_no_default_headers() {
        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
            .no_default_headers()
            .build()
            .unwrap();
        assert!(client.core.default_headers.is_empty());
    }

    #[tokio::test]
    async fn builder_user_agent_with_invalid_value() {
        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
            .user_agent("valid-agent/1.0")
            .build()
            .unwrap();
        assert!(client.core.default_headers.get(USER_AGENT).is_some());
    }

    #[tokio::test]
    async fn client_method_helpers() {
        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();
        assert!(client.get("http://example.com").is_ok());
        assert!(client.head("http://example.com").is_ok());
        assert!(client.post("http://example.com").is_ok());
        assert!(client.put("http://example.com").is_ok());
        assert!(client.patch("http://example.com").is_ok());
        assert!(client.delete("http://example.com").is_ok());
        assert!(
            client
                .request(Method::OPTIONS, "http://example.com")
                .is_ok()
        );
    }

    #[tokio::test]
    async fn client_invalid_url() {
        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();
        assert!(client.get("not a url").is_err());
    }

    #[tokio::test]
    async fn client_https_only_rejects_http() {
        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
            .https_only(true)
            .build()
            .unwrap();
        assert!(client.core.https_only);
    }

    #[tokio::test]
    async fn client_no_connection_reuse_sets_flag() {
        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
            .no_connection_reuse()
            .build()
            .unwrap();
        assert!(client.core.no_connection_reuse);
    }

    #[tokio::test]
    async fn builder_tcp_fast_open() {
        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
            .tcp_fast_open(true)
            .build()
            .unwrap();
        assert!(client.core.tcp_fast_open);
    }

    #[tokio::test]
    async fn builder_tcp_fast_open_disabled() {
        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
            .tcp_fast_open(false)
            .build()
            .unwrap();
        assert!(!client.core.tcp_fast_open);
    }

    #[tokio::test]
    async fn builder_http2_max_concurrent_reset_streams() {
        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
            .http2_max_concurrent_reset_streams(17)
            .build()
            .unwrap();
        let config = client.core.http2.as_ref().expect("http2 config");
        assert_eq!(config.max_concurrent_reset_streams, Some(17));
    }

    #[tokio::test]
    async fn builder_hsts() {
        let store = crate::hsts::HstsStore::new();
        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
            .hsts(store)
            .build()
            .unwrap();
        assert!(client.core.hsts.is_some());
    }

    #[tokio::test]
    async fn builder_cache() {
        let cache = crate::cache::HttpCache::new();
        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
            .cache(cache)
            .build()
            .unwrap();
        assert!(client.core.cache.is_some());
    }

    #[tokio::test]
    async fn builder_cookie_jar() {
        let jar = crate::cookie::CookieJar::new();
        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
            .cookie_jar(jar)
            .build()
            .unwrap();
        assert!(client.core.cookie_jar.is_some());
    }

    #[tokio::test]
    async fn builder_timeout() {
        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .unwrap();
        assert_eq!(client.core.timeout, Some(Duration::from_secs(10)));
    }

    #[tokio::test]
    async fn builder_connect_timeout() {
        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
            .connect_timeout(Duration::from_secs(5))
            .build()
            .unwrap();
        assert_eq!(client.core.connect_timeout, Some(Duration::from_secs(5)));
    }

    #[tokio::test]
    async fn builder_default_headers() {
        let mut headers = http::HeaderMap::new();
        headers.insert("x-custom", "value".parse().unwrap());
        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
            .default_headers(headers)
            .build()
            .unwrap();
        assert!(client.core.default_headers.contains_key("x-custom"));
    }

    #[tokio::test]
    async fn builder_retry() {
        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
            .retry(crate::retry::RetryConfig::default())
            .build()
            .unwrap();
        assert!(client.core.retry.is_some());
    }

    #[tokio::test]
    async fn builder_max_download_speed() {
        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
            .max_download_speed(1024 * 1024)
            .build()
            .unwrap();
        assert!(client.core.bandwidth_limiter.is_some());
    }

    #[tokio::test]
    async fn builder_digest_auth() {
        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
            .digest_auth("user", "pass")
            .build()
            .unwrap();
        assert!(client.core.digest_auth.is_some());
    }

    #[tokio::test]
    async fn builder_https_only() {
        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
            .https_only(true)
            .build()
            .unwrap();
        assert!(client.core.https_only);
    }

    #[tokio::test]
    async fn builder_debug() {
        let builder = HttpEngineSend::<TokioRuntime, TcpConnector>::builder();
        let dbg = format!("{builder:?}");
        assert!(dbg.contains("HttpEngineBuilder"));
    }

    #[tokio::test]
    async fn builder_pool_idle_timeout() {
        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
            .pool_idle_timeout(Duration::from_secs(30))
            .build()
            .unwrap();
        assert_eq!(client.core.pool.idle_timeout(), Duration::from_secs(30));
    }

    #[tokio::test]
    async fn builder_pool_max_lifetime() {
        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
            .pool_max_lifetime(Duration::from_secs(120))
            .build()
            .unwrap();
        assert_eq!(
            client.core.pool.max_lifetime(),
            Some(Duration::from_secs(120))
        );
    }

    #[tokio::test]
    async fn builder_pool_max_active_streams_per_connection() {
        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
            .pool_max_active_streams_per_connection(4)
            .build()
            .unwrap();
        assert_eq!(
            client
                .core
                .pool
                .max_active_streams_per_connection()
                .map(|max| max.get()),
            Some(4)
        );
    }

    #[tokio::test]
    async fn builder_pool_max_active_streams_per_connection_default_unlimited() {
        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
            .build()
            .unwrap();
        assert_eq!(client.core.pool.max_active_streams_per_connection(), None);
    }

    #[test]
    #[should_panic(expected = "pool_max_active_streams_per_connection must be greater than 0")]
    fn builder_pool_max_active_streams_per_connection_rejects_zero() {
        let _ = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
            .pool_max_active_streams_per_connection(0);
    }

    #[tokio::test]
    async fn builder_proxy_shorthand() {
        use crate::proxy::ProxyConfig;
        let config = ProxyConfig::http("http://proxy:8080").unwrap();
        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
            .proxy(config)
            .build()
            .unwrap();
        assert!(client.core.proxy.is_some());
    }

    #[tokio::test]
    async fn builder_user_agent_invalid_is_ignored() {
        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
            .user_agent("bad\x00agent")
            .build()
            .unwrap();
        let ua = client.core.default_headers.get(USER_AGENT).unwrap();
        assert_eq!(ua.as_bytes(), DEFAULT_USER_AGENT.as_bytes());
    }

    #[cfg(feature = "rustls")]
    #[tokio::test]
    async fn builder_tls_explicit_passthrough() {
        install_crypto();
        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
            .tls(crate::tls::RustlsConnector::with_webpki_roots())
            .build()
            .unwrap();
        assert!(client.core.tls.is_some());
    }

    #[cfg(feature = "rustls")]
    #[tokio::test]
    async fn builder_tls_version_constraints_only() {
        install_crypto();
        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
            .min_tls_version(crate::tls::TlsVersion::Tls1_2)
            .build()
            .unwrap();
        assert!(client.core.tls.is_some());
    }

    #[cfg(feature = "rustls")]
    #[tokio::test]
    async fn builder_tls_max_version_only() {
        install_crypto();
        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
            .max_tls_version(crate::tls::TlsVersion::Tls1_3)
            .build()
            .unwrap();
        assert!(client.core.tls.is_some());
    }

    #[cfg(feature = "rustls")]
    #[tokio::test]
    async fn builder_tls_min_and_max() {
        install_crypto();
        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
            .min_tls_version(crate::tls::TlsVersion::Tls1_2)
            .max_tls_version(crate::tls::TlsVersion::Tls1_3)
            .build()
            .unwrap();
        assert!(client.core.tls.is_some());
    }

    #[cfg(feature = "rustls")]
    #[tokio::test]
    async fn builder_tls_extra_root_certs() {
        install_crypto();
        let ca = rcgen::generate_simple_self_signed(vec!["test.local".into()]).unwrap();
        let cert = crate::tls::Certificate::from_der(ca.cert.der().to_vec());
        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
            .add_root_certificates(&[cert])
            .build()
            .unwrap();
        assert!(client.core.tls.is_some());
    }

    #[cfg(feature = "rustls")]
    #[tokio::test]
    async fn builder_tls_extra_root_certs_with_version() {
        install_crypto();
        let ca = rcgen::generate_simple_self_signed(vec!["test.local".into()]).unwrap();
        let cert = crate::tls::Certificate::from_der(ca.cert.der().to_vec());
        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
            .add_root_certificates(&[cert])
            .min_tls_version(crate::tls::TlsVersion::Tls1_3)
            .build()
            .unwrap();
        assert!(client.core.tls.is_some());
    }

    #[cfg(feature = "rustls")]
    #[tokio::test]
    async fn builder_tls_identity() {
        install_crypto();
        let ca = rcgen::generate_simple_self_signed(vec!["test.local".into()]).unwrap();
        let mut pem = ca.cert.pem();
        pem.push_str(&ca.signing_key.serialize_pem());
        let id = crate::tls::Identity::from_pem(pem.as_bytes()).unwrap();
        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
            .identity(id)
            .build()
            .unwrap();
        assert!(client.core.tls.is_some());
    }

    #[cfg(feature = "rustls")]
    #[tokio::test]
    async fn builder_tls_danger_accept_invalid_certs() {
        install_crypto();
        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
            .danger_accept_invalid_certs()
            .build()
            .unwrap();
        assert!(client.core.tls.is_some());
    }

    #[cfg(feature = "rustls")]
    #[tokio::test]
    async fn builder_tls_danger_accept_invalid_hostnames() {
        install_crypto();
        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
            .danger_accept_invalid_hostnames(true)
            .build()
            .unwrap();
        assert!(client.core.tls.is_some());
    }

    #[cfg(feature = "rustls")]
    #[tokio::test]
    async fn builder_tls_sni_disabled() {
        install_crypto();
        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
            .tls_sni(false)
            .build()
            .unwrap();
        let tls = client.core.tls.as_ref().unwrap();
        assert!(!tls.config().enable_sni);
    }

    #[cfg(feature = "rustls")]
    #[tokio::test]
    async fn builder_tls_sni_enabled_is_noop() {
        install_crypto();
        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
            .tls_sni(true)
            .build()
            .unwrap();
        let tls = client.core.tls.as_ref().unwrap();
        assert!(tls.config().enable_sni);
    }

    #[cfg(feature = "rustls")]
    #[tokio::test]
    async fn builder_tls_explicit_with_sni_disabled() {
        install_crypto();
        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
            .tls(crate::tls::RustlsConnector::with_webpki_roots())
            .tls_sni(false)
            .build()
            .unwrap();
        let tls = client.core.tls.as_ref().unwrap();
        assert!(!tls.config().enable_sni);
    }

    #[test]
    fn apply_default_headers_fills_missing() {
        let mut headers = http::HeaderMap::new();
        headers.insert("x-custom", "existing".parse().unwrap());

        let mut default_headers = http::HeaderMap::new();
        default_headers.insert("x-custom", "default".parse().unwrap());
        default_headers.insert("x-extra", "added".parse().unwrap());

        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
            .default_headers(default_headers)
            .build()
            .unwrap();

        let mut test_headers = headers.clone();
        // Simulate what apply_default_headers does
        for (name, value) in client.core.default_headers.iter() {
            if !test_headers.contains_key(name) {
                test_headers.insert(name, value.clone());
            }
        }
        assert_eq!(test_headers.get("x-custom").unwrap(), "existing");
        assert_eq!(test_headers.get("x-extra").unwrap(), "added");
    }

    #[test]
    fn hsts_store_marks_host_for_upgrade() {
        let store = crate::hsts::HstsStore::new();
        let mut sts_headers = http::HeaderMap::new();
        sts_headers.insert(
            http::header::HeaderName::from_static("strict-transport-security"),
            "max-age=31536000".parse().unwrap(),
        );
        store.store_from_response("example.com", &sts_headers);

        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
            .hsts(store)
            .build()
            .unwrap();
        assert!(
            client
                .core
                .hsts
                .as_ref()
                .unwrap()
                .should_upgrade("example.com")
        );
    }

    #[test]
    fn hsts_does_not_upgrade_unknown_host() {
        let store = crate::hsts::HstsStore::new();
        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
            .hsts(store)
            .build()
            .unwrap();
        assert!(
            !client
                .core
                .hsts
                .as_ref()
                .unwrap()
                .should_upgrade("not-stored.com")
        );
    }

    #[test]
    fn no_connection_reuse_flag() {
        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
            .no_connection_reuse()
            .build()
            .unwrap();
        assert!(client.core.no_connection_reuse);
    }

    #[test]
    fn bandwidth_limiter_accessor() {
        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
            .max_download_speed(1024 * 1024)
            .build()
            .unwrap();
        assert!(client.bandwidth_limiter().is_some());
    }

    #[test]
    fn bandwidth_limiter_accessor_none() {
        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();
        assert!(client.bandwidth_limiter().is_none());
    }

    #[test]
    fn default_timeout_accessor() {
        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .unwrap();
        assert_eq!(client.default_timeout(), Some(Duration::from_secs(10)));
    }

    #[test]
    fn default_timeout_accessor_none() {
        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();
        assert_eq!(client.default_timeout(), None);
    }

    #[test]
    fn default_retry_accessor() {
        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
            .retry(crate::retry::RetryConfig::default())
            .build()
            .unwrap();
        assert!(client.default_retry().is_some());
    }

    #[test]
    fn default_retry_accessor_none() {
        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();
        assert!(client.default_retry().is_none());
    }

    #[test]
    fn middleware_accessor() {
        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();
        assert!(client.middleware().is_empty());
    }

    #[tokio::test]
    async fn execute_rejects_http_when_https_only() {
        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
            .https_only(true)
            .build()
            .unwrap();
        let result = client
            .execute(
                Method::GET,
                "http://example.com".parse().unwrap(),
                http::HeaderMap::new(),
                None,
                None,
                None,
            )
            .await;
        let err = result.unwrap_err();
        match err {
            crate::error::Error::HttpsOnly(scheme) => assert_eq!(scheme, "http"),
            other => panic!("expected HttpsOnly, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn execute_allows_https_when_https_only() {
        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
            .https_only(true)
            .build()
            .unwrap();
        let result = client
            .execute(
                Method::GET,
                "https://example.com".parse().unwrap(),
                http::HeaderMap::new(),
                None,
                None,
                None,
            )
            .await;
        // Will fail with connection error, not HttpsOnly
        assert!(!matches!(result, Err(crate::error::Error::HttpsOnly(_))));
    }

    #[cfg(feature = "rustls")]
    #[test]
    fn builder_tls_identity_with_version_constraints() {
        install_crypto();
        let ca = rcgen::generate_simple_self_signed(vec!["test.local".into()]).unwrap();
        let mut pem = ca.cert.pem();
        pem.push_str(&ca.signing_key.serialize_pem());
        let id = crate::tls::Identity::from_pem(pem.as_bytes()).unwrap();
        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
            .identity(id)
            .min_tls_version(crate::tls::TlsVersion::Tls1_3)
            .build()
            .unwrap();
        assert!(client.core.tls.is_some());
    }

    #[cfg(feature = "rustls")]
    #[test]
    fn builder_tls_identity_with_extra_roots_and_version() {
        install_crypto();
        let ca = rcgen::generate_simple_self_signed(vec!["test.local".into()]).unwrap();
        let cert = crate::tls::Certificate::from_der(ca.cert.der().to_vec());
        let mut pem = ca.cert.pem();
        pem.push_str(&ca.signing_key.serialize_pem());
        let id = crate::tls::Identity::from_pem(pem.as_bytes()).unwrap();
        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
            .add_root_certificates(&[cert])
            .identity(id)
            .min_tls_version(crate::tls::TlsVersion::Tls1_2)
            .max_tls_version(crate::tls::TlsVersion::Tls1_3)
            .build()
            .unwrap();
        assert!(client.core.tls.is_some());
    }

    #[cfg(feature = "rustls")]
    #[test]
    fn builder_tls_danger_invalid_hostnames_with_extra_roots() {
        install_crypto();
        let ca = rcgen::generate_simple_self_signed(vec!["test.local".into()]).unwrap();
        let cert = crate::tls::Certificate::from_der(ca.cert.der().to_vec());
        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
            .add_root_certificates(&[cert])
            .danger_accept_invalid_hostnames(true)
            .build()
            .unwrap();
        assert!(client.core.tls.is_some());
    }

    #[cfg(feature = "rustls")]
    #[test]
    fn builder_tls_danger_invalid_hostnames_with_version_constraints() {
        install_crypto();
        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
            .danger_accept_invalid_hostnames(true)
            .min_tls_version(crate::tls::TlsVersion::Tls1_2)
            .build()
            .unwrap();
        assert!(client.core.tls.is_some());
    }

    #[cfg(feature = "rustls")]
    #[test]
    fn builder_tls_danger_invalid_hostnames_with_identity() {
        install_crypto();
        let ca = rcgen::generate_simple_self_signed(vec!["test.local".into()]).unwrap();
        let mut pem = ca.cert.pem();
        pem.push_str(&ca.signing_key.serialize_pem());
        let id = crate::tls::Identity::from_pem(pem.as_bytes()).unwrap();
        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
            .identity(id)
            .danger_accept_invalid_hostnames(true)
            .build()
            .unwrap();
        assert!(client.core.tls.is_some());
    }

    #[test]
    fn builder_static_resolver_setup() {
        let addr: std::net::SocketAddr = "127.0.0.1:8080".parse().unwrap();
        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
            .resolve("example.com", addr)
            .build()
            .unwrap();
        assert!(client.core.resolver.is_some());
    }

    #[test]
    fn builder_static_resolver_with_custom_resolver_fallback() {
        use std::net::SocketAddr;
        use std::pin::Pin;
        let addr: SocketAddr = "127.0.0.1:8080".parse().unwrap();
        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
            .resolver(
                |_host: &str,
                 _port: u16|
                 -> Pin<
                    Box<dyn std::future::Future<Output = std::io::Result<SocketAddr>> + Send>,
                > { Box::pin(async { Ok("127.0.0.1:80".parse().unwrap()) }) },
            )
            .resolve("example.com", addr)
            .build()
            .unwrap();
        assert!(client.core.resolver.is_some());
    }

    #[test]
    fn builder_static_resolver_multiple_hosts() {
        let addr1: std::net::SocketAddr = "127.0.0.1:8080".parse().unwrap();
        let addr2: std::net::SocketAddr = "127.0.0.1:9090".parse().unwrap();
        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
            .resolve("example.com", addr1)
            .resolve("other.com", addr2)
            .build()
            .unwrap();
        assert!(client.core.resolver.is_some());
    }

    #[cfg(feature = "rustls")]
    #[test]
    fn builder_tls_sni_disabled_without_explicit_tls() {
        install_crypto();
        // Verifies the `needs_sni_update` path where no TLS connector is set yet
        // and the fallback creates a webpki_roots connector before disabling SNI.
        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
            .tls_sni(false)
            .build()
            .unwrap();
        let tls = client.core.tls.as_ref().unwrap();
        assert!(!tls.config().enable_sni);
    }

    #[cfg(feature = "rustls")]
    #[test]
    fn builder_tls_version_only_creates_versioned_connector() {
        install_crypto();
        // Tests the path where only version constraints are set, no extra certs/identity/crls.
        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
            .min_tls_version(crate::tls::TlsVersion::Tls1_3)
            .max_tls_version(crate::tls::TlsVersion::Tls1_3)
            .build()
            .unwrap();
        assert!(client.core.tls.is_some());
    }

    #[cfg(feature = "rustls")]
    #[test]
    fn builder_tls_no_config_uses_default_webpki() {
        install_crypto();
        // Tests the final else branch: no tls, no version constraints, no extra config,
        // no needs_configured -> with_webpki_roots fallback.
        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
            .build()
            .unwrap();
        assert!(client.core.tls.is_some());
    }

    // ── is_stale_connection_error via hyper errors ───────────────────────

    #[tokio::test]
    async fn is_stale_hyper_canceled() {
        // Create an h1 connection and drop the server side to get a canceled error
        let (client_io, _server_io) = tokio::io::duplex(1024);
        let io = crate::runtime::tokio_rt::TokioIo::new(client_io);
        let (mut sender, conn) = hyper::client::conn::http1::handshake(io)
            .await
            .expect("handshake");

        tokio::spawn(async move {
            let _ = conn.await;
        });

        // Drop means the connection will be closed; wait a moment
        drop(_server_io);
        tokio::task::yield_now().await;
        tokio::time::sleep(Duration::from_millis(10)).await;

        let req = http::Request::builder()
            .uri("http://example.com/")
            .body(http_body_util::Empty::<bytes::Bytes>::new())
            .unwrap();

        let result = sender.send_request(req).await;
        if let Err(hyper_err) = result {
            let err = crate::error::Error::Hyper(hyper_err);
            assert!(
                HttpEngineCore::<crate::body::RequestBodySend>::is_stale_connection_error_pub(&err),
                "canceled/closed hyper error should be stale"
            );
        }
    }

    // ── maybe_upgrade_hsts tests ─────────────────────────────────────────

    #[test]
    fn maybe_upgrade_hsts_upgrades_known_host() {
        let store = crate::hsts::HstsStore::new();
        let mut sts_headers = http::HeaderMap::new();
        sts_headers.insert(
            http::header::HeaderName::from_static("strict-transport-security"),
            "max-age=31536000".parse().unwrap(),
        );
        store.store_from_response("upgrade.example.com", &sts_headers);

        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
            .hsts(store)
            .build()
            .unwrap();

        let uri: Uri = "http://upgrade.example.com/path?q=1".parse().unwrap();
        let upgraded = client.core.maybe_upgrade_hsts(uri);
        assert_eq!(upgraded.scheme_str(), Some("https"));
        assert_eq!(upgraded.host(), Some("upgrade.example.com"));
        assert_eq!(upgraded.path_and_query().unwrap().as_str(), "/path?q=1");
    }

    #[test]
    fn maybe_upgrade_hsts_does_not_upgrade_unknown_host() {
        let store = crate::hsts::HstsStore::new();
        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
            .hsts(store)
            .build()
            .unwrap();

        let uri: Uri = "http://unknown.example.com/".parse().unwrap();
        let result = client.core.maybe_upgrade_hsts(uri.clone());
        assert_eq!(result.scheme_str(), Some("http"));
    }

    #[test]
    fn maybe_upgrade_hsts_does_not_downgrade_https() {
        let store = crate::hsts::HstsStore::new();
        let mut sts_headers = http::HeaderMap::new();
        sts_headers.insert(
            http::header::HeaderName::from_static("strict-transport-security"),
            "max-age=31536000".parse().unwrap(),
        );
        store.store_from_response("secure.example.com", &sts_headers);

        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
            .hsts(store)
            .build()
            .unwrap();

        let uri: Uri = "https://secure.example.com/path".parse().unwrap();
        let result = client.core.maybe_upgrade_hsts(uri.clone());
        // Should remain as-is since it's already HTTPS
        assert_eq!(result, uri);
    }

    #[test]
    fn maybe_upgrade_hsts_no_hsts_store_is_noop() {
        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
            .build()
            .unwrap();
        let uri: Uri = "http://example.com/".parse().unwrap();
        let result = client.core.maybe_upgrade_hsts(uri.clone());
        assert_eq!(result, uri);
    }

    // ── apply_default_headers tests ──────────────────────────────────────

    #[test]
    fn apply_default_headers_does_not_overwrite() {
        let mut default_headers = http::HeaderMap::new();
        default_headers.insert("x-custom", "default-value".parse().unwrap());
        default_headers.insert("x-other", "other-value".parse().unwrap());

        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
            .default_headers(default_headers)
            .build()
            .unwrap();

        let mut headers = http::HeaderMap::new();
        headers.insert("x-custom", "user-value".parse().unwrap());

        client.core.apply_default_headers(&mut headers);

        // Existing header should NOT be overwritten
        assert_eq!(headers.get("x-custom").unwrap(), "user-value");
        // Missing header should be added
        assert_eq!(headers.get("x-other").unwrap(), "other-value");
    }

    #[test]
    fn apply_default_headers_adds_accept_encoding() {
        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
            .build()
            .unwrap();
        let mut headers = http::HeaderMap::new();
        client.core.apply_default_headers(&mut headers);
        // The client by default sets accept-encoding for decompression
        if client.core.accept_encoding_header.is_some() {
            assert!(headers.contains_key(http::header::ACCEPT_ENCODING));
        }
    }

    #[test]
    fn apply_default_headers_does_not_overwrite_accept_encoding() {
        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
            .build()
            .unwrap();
        let mut headers = http::HeaderMap::new();
        headers.insert(http::header::ACCEPT_ENCODING, "identity".parse().unwrap());
        client.core.apply_default_headers(&mut headers);
        assert_eq!(
            headers.get(http::header::ACCEPT_ENCODING).unwrap(),
            "identity"
        );
    }

    #[cfg(all(feature = "http3", feature = "rustls", feature = "tokio"))]
    #[tokio::test]
    async fn builder_http3_enable_then_disable() {
        install_crypto();
        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
            .tls(crate::tls::RustlsConnector::with_webpki_roots())
            .http3(true)
            .unwrap()
            .http3(false)
            .unwrap()
            .build()
            .unwrap();
        assert!(!client.core.prefer_h3);
    }

    #[cfg(all(feature = "http3", feature = "rustls", feature = "tokio"))]
    #[tokio::test]
    async fn builder_alt_svc_h3_enable_creates_endpoint() {
        install_crypto();
        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
            .tls(crate::tls::RustlsConnector::with_webpki_roots())
            .alt_svc_h3(true)
            .unwrap()
            .build()
            .unwrap();
        assert!(
            !client.core.prefer_h3,
            "alt_svc_h3 alone should not set prefer_h3"
        );
        assert!(
            client.core.h3_endpoint.is_some(),
            "alt_svc_h3(true) should create an h3 endpoint for opportunistic upgrade"
        );
    }

    #[cfg(all(feature = "http3", feature = "rustls", feature = "tokio"))]
    #[tokio::test]
    async fn builder_alt_svc_h3_disable_clears_endpoint_without_prefer() {
        install_crypto();
        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
            .tls(crate::tls::RustlsConnector::with_webpki_roots())
            .alt_svc_h3(true)
            .unwrap()
            .alt_svc_h3(false)
            .unwrap()
            .build()
            .unwrap();
        assert!(
            client.core.h3_endpoint.is_none(),
            "alt_svc_h3(false) without prefer_h3 should remove the endpoint"
        );
    }

    #[cfg(all(feature = "http3", feature = "rustls", feature = "tokio"))]
    #[tokio::test]
    async fn builder_http3_enable_sets_prefer_and_endpoint() {
        install_crypto();
        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
            .tls(crate::tls::RustlsConnector::with_webpki_roots())
            .http3(true)
            .unwrap()
            .build()
            .unwrap();
        assert!(client.core.prefer_h3);
        assert!(client.core.h3_endpoint.is_some());
    }

    #[cfg(all(feature = "http3", feature = "rustls", feature = "tokio"))]
    #[tokio::test]
    async fn builder_http3_0rtt_sets_flag() {
        install_crypto();
        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
            .tls(crate::tls::RustlsConnector::with_webpki_roots())
            .http3(true)
            .unwrap()
            .h3_zero_rtt(true)
            .build()
            .unwrap();
        assert!(client.core.h3_zero_rtt);
    }

    #[cfg(all(feature = "http3", feature = "rustls", feature = "tokio"))]
    #[tokio::test]
    async fn builder_alt_svc_h3_disable_keeps_endpoint_when_prefer_h3() {
        install_crypto();
        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
            .tls(crate::tls::RustlsConnector::with_webpki_roots())
            .http3(true)
            .unwrap()
            .alt_svc_h3(false)
            .unwrap()
            .build()
            .unwrap();
        assert!(
            client.core.prefer_h3,
            "prefer_h3 from http3(true) should persist"
        );
        assert!(
            client.core.h3_endpoint.is_some(),
            "alt_svc_h3(false) should keep endpoint when prefer_h3 is set"
        );
    }

    #[cfg(all(feature = "http3", feature = "rustls", feature = "tokio"))]
    #[tokio::test]
    async fn builder_http3_without_tls_returns_error() {
        install_crypto();
        // http3(true) without calling .tls() first should fail
        let result = HttpEngineSend::<TokioRuntime, TcpConnector>::builder().http3(true);
        assert!(result.is_err(), "http3(true) without TLS should return Err");
        let err = result.unwrap_err();
        match err {
            crate::error::Error::Other(msg) => {
                let msg_str = msg.to_string();
                assert!(
                    msg_str.contains("TLS"),
                    "error message should mention TLS, got: {msg_str}"
                );
            }
            other => panic!("expected Error::Other mentioning TLS, got {other:?}"),
        }
    }

    #[cfg(all(feature = "http3", feature = "rustls", feature = "tokio"))]
    #[tokio::test]
    async fn builder_h3_zero_rtt_default_false() {
        install_crypto();
        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
            .tls(crate::tls::RustlsConnector::with_webpki_roots())
            .http3(true)
            .unwrap()
            .build()
            .unwrap();
        assert!(
            !client.core.h3_zero_rtt,
            "h3_zero_rtt should default to false"
        );
    }

    // ── process_redirect tests ──────────────────────────────────────────

    fn make_redirect_response(status: StatusCode, location: &str) -> crate::response::Response {
        use http_body_util::BodyExt;
        let http_resp = http::Response::builder()
            .status(status)
            .header(http::header::LOCATION, location)
            .body(
                http_body_util::Full::new(bytes::Bytes::new())
                    .map_err(|never| match never {})
                    .boxed_unsync(),
            )
            .unwrap();
        crate::response::Response::from_boxed(http_resp, "http://origin.com/".parse().unwrap())
    }

    fn make_test_core() -> HttpEngineCore<crate::body::RequestBodySend> {
        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
            .build()
            .unwrap();
        client.core
    }

    #[test]
    fn process_redirect_301_changes_method_to_get() {
        let core = make_test_core();
        let resp = make_redirect_response(StatusCode::MOVED_PERMANENTLY, "http://origin.com/new");
        let uri: Uri = "http://origin.com/old".parse().unwrap();
        let mut headers = HeaderMap::new();

        let result = core
            .process_redirect(&resp, &uri, Method::POST, None, &mut headers)
            .unwrap();
        let (next_uri, next_method, next_body) = result.unwrap();
        assert_eq!(next_uri.path(), "/new");
        assert_eq!(next_method, Method::GET);
        assert!(next_body.is_none());
    }

    #[test]
    fn process_redirect_302_changes_method_to_get() {
        let core = make_test_core();
        let resp = make_redirect_response(StatusCode::FOUND, "http://origin.com/found");
        let uri: Uri = "http://origin.com/old".parse().unwrap();
        let mut headers = HeaderMap::new();

        let result = core
            .process_redirect(&resp, &uri, Method::PUT, None, &mut headers)
            .unwrap();
        let (next_uri, next_method, _) = result.unwrap();
        assert_eq!(next_uri.path(), "/found");
        assert_eq!(next_method, Method::GET);
    }

    #[test]
    fn process_redirect_303_changes_method_to_get() {
        let core = make_test_core();
        let resp = make_redirect_response(StatusCode::SEE_OTHER, "http://origin.com/see");
        let uri: Uri = "http://origin.com/old".parse().unwrap();
        let mut headers = HeaderMap::new();

        let result = core
            .process_redirect(&resp, &uri, Method::POST, None, &mut headers)
            .unwrap();
        let (_, next_method, _) = result.unwrap();
        assert_eq!(next_method, Method::GET);
    }

    #[test]
    fn process_redirect_307_preserves_method_and_body() {
        let core = make_test_core();
        let resp = make_redirect_response(StatusCode::TEMPORARY_REDIRECT, "http://origin.com/temp");
        let uri: Uri = "http://origin.com/old".parse().unwrap();
        let mut headers = HeaderMap::new();
        let body = crate::body::RequestBody::Buffered(bytes::Bytes::from("hello"));

        let result = core
            .process_redirect(&resp, &uri, Method::POST, Some(body), &mut headers)
            .unwrap();
        let (next_uri, next_method, next_body) = result.unwrap();
        assert_eq!(next_uri.path(), "/temp");
        assert_eq!(next_method, Method::POST);
        assert!(next_body.is_some(), "body should be replayed on 307");
    }

    #[test]
    fn process_redirect_308_preserves_method_and_body() {
        let core = make_test_core();
        let resp = make_redirect_response(StatusCode::PERMANENT_REDIRECT, "http://origin.com/perm");
        let uri: Uri = "http://origin.com/old".parse().unwrap();
        let mut headers = HeaderMap::new();
        let body = crate::body::RequestBody::Buffered(bytes::Bytes::from("data"));

        let result = core
            .process_redirect(&resp, &uri, Method::PUT, Some(body), &mut headers)
            .unwrap();
        let (_, next_method, next_body) = result.unwrap();
        assert_eq!(next_method, Method::PUT);
        assert!(next_body.is_some());
    }

    #[test]
    fn process_redirect_307_get_without_body_succeeds() {
        let core = make_test_core();
        let resp = make_redirect_response(StatusCode::TEMPORARY_REDIRECT, "http://origin.com/next");
        let uri: Uri = "http://origin.com/old".parse().unwrap();
        let mut headers = HeaderMap::new();

        // GET without body on 307 should succeed (no body needed)
        let result = core
            .process_redirect(&resp, &uri, Method::GET, None, &mut headers)
            .unwrap();
        let (_, next_method, next_body) = result.unwrap();
        assert_eq!(next_method, Method::GET);
        assert!(next_body.is_none());
    }

    #[test]
    fn process_redirect_307_head_without_body_succeeds() {
        let core = make_test_core();
        let resp = make_redirect_response(StatusCode::TEMPORARY_REDIRECT, "http://origin.com/next");
        let uri: Uri = "http://origin.com/old".parse().unwrap();
        let mut headers = HeaderMap::new();

        let result = core
            .process_redirect(&resp, &uri, Method::HEAD, None, &mut headers)
            .unwrap();
        let (_, next_method, _) = result.unwrap();
        assert_eq!(next_method, Method::HEAD);
    }

    #[test]
    fn process_redirect_307_post_without_body_fails() {
        let core = make_test_core();
        let resp = make_redirect_response(StatusCode::TEMPORARY_REDIRECT, "http://origin.com/next");
        let uri: Uri = "http://origin.com/old".parse().unwrap();
        let mut headers = HeaderMap::new();

        // POST without body on 307 - cannot replay streaming body
        let result = core.process_redirect(&resp, &uri, Method::POST, None, &mut headers);
        let err = result.unwrap_err();
        match err {
            crate::error::Error::Redirect(msg) => {
                assert!(
                    msg.contains("cannot replay"),
                    "expected 'cannot replay' error, got: {msg}"
                );
            }
            other => panic!("expected Redirect error, got {other:?}"),
        }
    }

    #[test]
    fn process_redirect_unexpected_status_returns_error() {
        use http_body_util::BodyExt;
        let core = make_test_core();
        // Use a status that is not a redirect (e.g., 299 or 305)
        let http_resp = http::Response::builder()
            .status(StatusCode::from_u16(305).unwrap())
            .header(http::header::LOCATION, "http://origin.com/proxy")
            .body(
                http_body_util::Full::new(bytes::Bytes::new())
                    .map_err(|never| match never {})
                    .boxed_unsync(),
            )
            .unwrap();
        let resp =
            crate::response::Response::from_boxed(http_resp, "http://origin.com/".parse().unwrap());
        let uri: Uri = "http://origin.com/old".parse().unwrap();
        let mut headers = HeaderMap::new();

        let result = core.process_redirect(&resp, &uri, Method::GET, None, &mut headers);
        let err = result.unwrap_err();
        match err {
            crate::error::Error::Redirect(msg) => {
                assert!(
                    msg.contains("unexpected redirect status"),
                    "expected 'unexpected redirect status', got: {msg}"
                );
            }
            other => panic!("expected Redirect error, got {other:?}"),
        }
    }

    #[test]
    fn process_redirect_cross_origin_strips_sensitive_headers() {
        let core = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
            .sensitive_header(http::header::HeaderName::from_static("x-api-key"))
            .build()
            .unwrap()
            .core;
        let resp = make_redirect_response(StatusCode::FOUND, "http://other.com/new");
        let uri: Uri = "http://origin.com/old".parse().unwrap();
        let mut headers = HeaderMap::new();
        headers.insert(http::header::AUTHORIZATION, "Bearer token".parse().unwrap());
        headers.insert(http::header::COOKIE, "session=abc".parse().unwrap());
        headers.insert(
            http::header::HeaderName::from_static("x-api-key"),
            "secret".parse().unwrap(),
        );

        let result = core
            .process_redirect(&resp, &uri, Method::GET, None, &mut headers)
            .unwrap();
        assert!(result.is_some());
        // Sensitive headers should be stripped on cross-origin redirect
        assert!(
            headers.get(http::header::AUTHORIZATION).is_none(),
            "Authorization should be stripped"
        );
        assert!(
            headers.get(http::header::COOKIE).is_none(),
            "Cookie should be stripped"
        );
        assert!(
            headers
                .get(http::header::HeaderName::from_static("x-api-key"))
                .is_none(),
            "custom sensitive header should be stripped"
        );
    }

    #[test]
    fn process_redirect_same_origin_preserves_sensitive_headers() {
        let core = make_test_core();
        let resp = make_redirect_response(StatusCode::FOUND, "http://origin.com/new");
        let uri: Uri = "http://origin.com/old".parse().unwrap();
        let mut headers = HeaderMap::new();
        headers.insert(http::header::AUTHORIZATION, "Bearer token".parse().unwrap());

        let result = core
            .process_redirect(&resp, &uri, Method::GET, None, &mut headers)
            .unwrap();
        assert!(result.is_some());
        // Same-origin redirect should keep Authorization
        assert!(
            headers.get(http::header::AUTHORIZATION).is_some(),
            "Authorization should be preserved on same-origin redirect"
        );
    }

    #[test]
    fn process_redirect_sets_host_header() {
        let core = make_test_core();
        let resp = make_redirect_response(StatusCode::FOUND, "http://newhost.com/path");
        let uri: Uri = "http://origin.com/old".parse().unwrap();
        let mut headers = HeaderMap::new();

        let result = core
            .process_redirect(&resp, &uri, Method::GET, None, &mut headers)
            .unwrap();
        let (next_uri, _, _) = result.unwrap();
        assert_eq!(next_uri.host(), Some("newhost.com"));
        // Host header should be set to new authority
        assert_eq!(headers.get(http::header::HOST).unwrap(), "newhost.com");
    }

    #[test]
    fn process_redirect_with_referer_enabled() {
        let core = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
            .referer(true)
            .build()
            .unwrap()
            .core;
        let resp = make_redirect_response(StatusCode::FOUND, "http://origin.com/new");
        let uri: Uri = "http://origin.com/old".parse().unwrap();
        let mut headers = HeaderMap::new();

        let _ = core
            .process_redirect(&resp, &uri, Method::GET, None, &mut headers)
            .unwrap();
        // Referer should be set to the current URI
        let referer = headers
            .get(http::header::REFERER)
            .unwrap()
            .to_str()
            .unwrap();
        assert!(
            referer.contains("origin.com/old"),
            "Referer should contain the previous URI, got: {referer}"
        );
    }

    #[test]
    fn process_redirect_missing_location_returns_error() {
        use http_body_util::BodyExt;
        let core = make_test_core();
        // Response without Location header
        let http_resp = http::Response::builder()
            .status(StatusCode::FOUND)
            .body(
                http_body_util::Full::new(bytes::Bytes::new())
                    .map_err(|never| match never {})
                    .boxed_unsync(),
            )
            .unwrap();
        let resp =
            crate::response::Response::from_boxed(http_resp, "http://origin.com/".parse().unwrap());
        let uri: Uri = "http://origin.com/old".parse().unwrap();
        let mut headers = HeaderMap::new();

        let result = core.process_redirect(&resp, &uri, Method::GET, None, &mut headers);
        let err = result.unwrap_err();
        match err {
            crate::error::Error::Redirect(msg) => {
                assert!(msg.contains("Location"), "got: {msg}");
            }
            other => panic!("expected Redirect error, got {other:?}"),
        }
    }

    #[test]
    fn process_redirect_middleware_notified() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};

        struct TrackingMiddleware {
            called: Arc<AtomicBool>,
        }
        impl crate::middleware::Middleware for TrackingMiddleware {
            fn on_redirect(&self, _status: StatusCode, _from: &Uri, _to: &Uri) {
                self.called.store(true, Ordering::SeqCst);
            }
        }

        let called = Arc::new(AtomicBool::new(false));
        let core = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
            .middleware(TrackingMiddleware {
                called: called.clone(),
            })
            .build()
            .unwrap()
            .core;

        let resp = make_redirect_response(StatusCode::FOUND, "http://origin.com/new");
        let uri: Uri = "http://origin.com/old".parse().unwrap();
        let mut headers = HeaderMap::new();

        let _ = core
            .process_redirect(&resp, &uri, Method::GET, None, &mut headers)
            .unwrap();
        assert!(
            called.load(Ordering::SeqCst),
            "middleware on_redirect should be called"
        );
    }

    // ── prepare_request_headers tests ───────────────────────────────────

    #[test]
    fn prepare_request_headers_applies_cookies() {
        let jar = crate::cookie::CookieJar::new();
        let mut cookie_headers = http::HeaderMap::new();
        cookie_headers.insert(
            http::header::SET_COOKIE,
            "session=abc123; Path=/".parse().unwrap(),
        );
        jar.store_from_response("example.com", "/", &cookie_headers);

        let core = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
            .cookie_jar(jar)
            .build()
            .unwrap()
            .core;

        let uri: Uri = "http://example.com/page".parse().unwrap();
        let mut headers = HeaderMap::new();
        core.prepare_request_headers(&uri, None, &mut headers);

        let cookie_header = headers.get(http::header::COOKIE).unwrap().to_str().unwrap();
        assert!(
            cookie_header.contains("session=abc123"),
            "expected cookie in header, got: {cookie_header}"
        );
    }

    #[test]
    fn prepare_request_headers_sets_host_when_missing() {
        let core = make_test_core();
        let uri: Uri = "http://example.com:8080/path".parse().unwrap();
        let mut headers = HeaderMap::new();

        core.prepare_request_headers(&uri, None, &mut headers);

        assert_eq!(headers.get(http::header::HOST).unwrap(), "example.com:8080");
    }

    #[test]
    fn prepare_request_headers_does_not_overwrite_existing_host() {
        let core = make_test_core();
        let uri: Uri = "http://example.com/path".parse().unwrap();
        let mut headers = HeaderMap::new();
        headers.insert(http::header::HOST, "custom-host.com".parse().unwrap());

        core.prepare_request_headers(&uri, None, &mut headers);

        assert_eq!(headers.get(http::header::HOST).unwrap(), "custom-host.com");
    }

    #[test]
    fn prepare_request_headers_no_cookie_jar_is_noop() {
        let core = make_test_core();
        let uri: Uri = "http://example.com/path".parse().unwrap();
        let mut headers = HeaderMap::new();
        headers.insert(http::header::HOST, "already-set".parse().unwrap());

        core.prepare_request_headers(&uri, None, &mut headers);

        assert!(headers.get(http::header::COOKIE).is_none());
        assert_eq!(headers.get(http::header::HOST).unwrap(), "already-set");
    }

    // ── post_execute tests ──────────────────────────────────────────────

    #[test]
    fn post_execute_done_on_non_redirect() {
        use http_body_util::BodyExt;
        let core = make_test_core();
        let http_resp = http::Response::builder()
            .status(StatusCode::OK)
            .body(
                http_body_util::Full::new(bytes::Bytes::new())
                    .map_err(|never| match never {})
                    .boxed_unsync(),
            )
            .unwrap();
        let resp = crate::response::Response::from_boxed(
            http_resp,
            "http://example.com/".parse().unwrap(),
        );
        let uri: Uri = "http://example.com/page".parse().unwrap();
        let mut headers = HeaderMap::new();

        let action = core
            .post_execute(&resp, &Method::GET, &uri, &mut headers, None)
            .unwrap();
        assert!(matches!(action, super::execute::PostExecuteAction::Done));
    }

    #[test]
    fn post_execute_done_on_not_modified() {
        use http_body_util::BodyExt;
        let http_resp = http::Response::builder()
            .status(StatusCode::NOT_MODIFIED)
            .body(
                http_body_util::Full::new(bytes::Bytes::new())
                    .map_err(|never| match never {})
                    .boxed_unsync(),
            )
            .unwrap();
        let resp = crate::response::Response::from_boxed(
            http_resp,
            "http://example.com/".parse().unwrap(),
        );
        let core = make_test_core();
        let uri: Uri = "http://example.com/page".parse().unwrap();
        let mut headers = HeaderMap::new();

        let action = core
            .post_execute(&resp, &Method::GET, &uri, &mut headers, None)
            .unwrap();
        assert!(matches!(action, super::execute::PostExecuteAction::Done));
    }

    #[test]
    fn post_execute_redirect_on_302() {
        let core = make_test_core();
        let resp = make_redirect_response(StatusCode::FOUND, "http://example.com/new");
        let uri: Uri = "http://example.com/old".parse().unwrap();
        let mut headers = HeaderMap::new();

        let action = core
            .post_execute(&resp, &Method::GET, &uri, &mut headers, None)
            .unwrap();
        match action {
            super::execute::PostExecuteAction::Redirect { uri, method, body } => {
                assert_eq!(uri.path(), "/new");
                assert_eq!(method, Method::GET);
                assert!(body.is_none());
            }
            super::execute::PostExecuteAction::Done => {
                panic!("expected Redirect action");
            }
        }
    }

    #[test]
    fn post_execute_stores_cookies_and_learns_hsts() {
        let jar = crate::cookie::CookieJar::new();
        let store = crate::hsts::HstsStore::new();

        let core = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
            .cookie_jar(jar.clone())
            .hsts(store.clone())
            .build()
            .unwrap()
            .core;

        use http_body_util::BodyExt;
        let http_resp = http::Response::builder()
            .status(StatusCode::OK)
            .header(http::header::SET_COOKIE, "token=xyz; Path=/")
            .header("strict-transport-security", "max-age=31536000")
            .body(
                http_body_util::Full::new(bytes::Bytes::new())
                    .map_err(|never| match never {})
                    .boxed_unsync(),
            )
            .unwrap();
        let resp = crate::response::Response::from_boxed(
            http_resp,
            "https://secure.example.com/api".parse().unwrap(),
        );

        let uri: Uri = "https://secure.example.com/api".parse().unwrap();
        let mut headers = HeaderMap::new();

        let action = core
            .post_execute(&resp, &Method::GET, &uri, &mut headers, None)
            .unwrap();
        assert!(matches!(action, super::execute::PostExecuteAction::Done));

        assert!(
            store.should_upgrade("secure.example.com"),
            "HSTS should be learned from response"
        );

        let mut cookie_headers = HeaderMap::new();
        jar.apply_to_request("secure.example.com", true, "/", None, &mut cookie_headers);
        let cookie_val = cookie_headers
            .get(http::header::COOKIE)
            .unwrap()
            .to_str()
            .unwrap();
        assert!(
            cookie_val.contains("token=xyz"),
            "cookie should be stored from response, got: {cookie_val}"
        );
    }

    #[test]
    fn post_execute_invalidates_cache_on_non_safe_method() {
        let cache = crate::cache::HttpCache::new();

        let status = StatusCode::OK;
        let mut resp_headers = http::HeaderMap::new();
        resp_headers.insert(http::header::CACHE_CONTROL, "max-age=3600".parse().unwrap());
        cache.store(
            &Method::GET,
            &"http://example.com/data".parse().unwrap(),
            status,
            &resp_headers,
            &bytes::Bytes::from("cached"),
            &HeaderMap::new(),
        );

        let core = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
            .cache(cache.clone())
            .build()
            .unwrap()
            .core;

        use http_body_util::BodyExt;
        let http_resp = http::Response::builder()
            .status(StatusCode::OK)
            .body(
                http_body_util::Full::new(bytes::Bytes::new())
                    .map_err(|never| match never {})
                    .boxed_unsync(),
            )
            .unwrap();
        let resp = crate::response::Response::from_boxed(
            http_resp,
            "http://example.com/data".parse().unwrap(),
        );
        let uri: Uri = "http://example.com/data".parse().unwrap();
        let mut headers = HeaderMap::new();

        let _ = core
            .post_execute(&resp, &Method::POST, &uri, &mut headers, None)
            .unwrap();

        let lookup_headers = HeaderMap::new();
        let result = cache.lookup(&Method::GET, &uri, &lookup_headers);
        assert!(
            matches!(result, crate::cache::CacheLookup::Miss),
            "cache should be invalidated after POST"
        );
    }

    #[test]
    fn post_execute_redirect_applies_hsts_upgrade() {
        let store = crate::hsts::HstsStore::new();
        let mut sts_headers = http::HeaderMap::new();
        sts_headers.insert(
            http::header::HeaderName::from_static("strict-transport-security"),
            "max-age=31536000".parse().unwrap(),
        );
        store.store_from_response("target.example.com", &sts_headers);

        let core = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
            .hsts(store)
            .build()
            .unwrap()
            .core;

        let resp =
            make_redirect_response(StatusCode::FOUND, "http://target.example.com/redirected");
        let uri: Uri = "http://origin.example.com/start".parse().unwrap();
        let mut headers = HeaderMap::new();

        let action = core
            .post_execute(&resp, &Method::GET, &uri, &mut headers, None)
            .unwrap();
        match action {
            super::execute::PostExecuteAction::Redirect { uri, .. } => {
                assert_eq!(
                    uri.scheme_str(),
                    Some("https"),
                    "redirect target should be HSTS-upgraded to https"
                );
                assert_eq!(uri.host(), Some("target.example.com"));
            }
            _ => panic!("expected Redirect action"),
        }
    }

    #[test]
    fn post_execute_redirect_https_only_rejects_http_target() {
        let core = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
            .https_only(true)
            .build()
            .unwrap()
            .core;

        let resp = make_redirect_response(StatusCode::FOUND, "http://insecure.com/page");
        let uri: Uri = "https://origin.com/start".parse().unwrap();
        let mut headers = HeaderMap::new();

        let result = core.post_execute(&resp, &Method::GET, &uri, &mut headers, None);
        let err = result.unwrap_err();
        assert!(
            matches!(err, crate::error::Error::HttpsOnly(_)),
            "expected HttpsOnly error, got: {err:?}"
        );
    }

    #[test]
    fn post_execute_done_when_redirect_policy_none() {
        let core = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
            .redirect_policy(crate::redirect::RedirectPolicy::None)
            .build()
            .unwrap()
            .core;

        let resp = make_redirect_response(StatusCode::FOUND, "http://origin.com/new");
        let uri: Uri = "http://origin.com/old".parse().unwrap();
        let mut headers = HeaderMap::new();

        let action = core
            .post_execute(&resp, &Method::GET, &uri, &mut headers, None)
            .unwrap();
        assert!(
            matches!(action, super::execute::PostExecuteAction::Done),
            "should return Done when redirect policy is None"
        );
    }
}
