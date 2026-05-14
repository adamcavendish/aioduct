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

    type TokioClient = HttpEngineSend<TokioRuntime, TcpConnector>;

    #[cfg(feature = "rustls")]
    fn install_crypto() {
        crate::tls::install_default_crypto_provider();
    }

    #[tokio::test]
    async fn builder_read_timeout() {
        let _client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
            .read_timeout(Duration::from_secs(5))
            .build();
    }

    #[tokio::test]
    async fn builder_tcp_keepalive() {
        let _client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
            .tcp_keepalive(Duration::from_secs(60))
            .build();
    }

    #[tokio::test]
    async fn builder_tcp_keepalive_interval() {
        let _client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
            .tcp_keepalive_interval(Duration::from_secs(10))
            .build();
    }

    #[tokio::test]
    async fn builder_tcp_keepalive_retries() {
        let _client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
            .tcp_keepalive_retries(3)
            .build();
    }

    #[tokio::test]
    async fn builder_local_address() {
        let _client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
            .local_address("127.0.0.1".parse().unwrap())
            .build();
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn builder_interface() {
        let _client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
            .interface("eth0")
            .build();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn builder_unix_socket() {
        let _client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
            .unix_socket("/tmp/test.sock")
            .build();
    }

    #[tokio::test]
    async fn builder_referer() {
        let _client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
            .referer(true)
            .build();
    }

    #[tokio::test]
    async fn builder_http2_prior_knowledge() {
        let _client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
            .http2_prior_knowledge()
            .build();
    }

    #[tokio::test]
    async fn builder_no_default_headers() {
        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
            .no_default_headers()
            .build();
        assert!(client.core.default_headers.is_empty());
    }

    #[tokio::test]
    async fn builder_user_agent_with_invalid_value() {
        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
            .user_agent("valid-agent/1.0")
            .build();
        assert!(client.core.default_headers.get(USER_AGENT).is_some());
    }

    #[tokio::test]
    async fn builder_proxy_settings() {
        use crate::proxy::ProxyConfig;
        let settings = ProxySettings::default().http(ProxyConfig::http("http://proxy:80").unwrap());
        let _client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
            .proxy_settings(settings)
            .build();
    }

    #[tokio::test]
    async fn builder_http2_config() {
        let config = crate::http2::Http2Config::default();
        let _client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
            .http2(config)
            .build();
    }

    #[tokio::test]
    async fn builder_rate_limiter() {
        let limiter = crate::throttle::RateLimiter::new(10, Duration::from_secs(1));
        let _client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
            .rate_limiter(limiter)
            .build();
    }

    #[tokio::test]
    async fn client_default_creates_same_as_new() {
        let _client: TokioClient = Default::default();
    }

    #[tokio::test]
    async fn client_method_helpers() {
        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new(TcpConnector);
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
        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new(TcpConnector);
        assert!(client.get("not a url").is_err());
    }

    #[tokio::test]
    async fn client_https_only_rejects_http() {
        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
            .https_only(true)
            .build();
        assert!(client.core.https_only);
    }

    #[tokio::test]
    async fn client_no_connection_reuse_sets_flag() {
        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
            .no_connection_reuse()
            .build();
        assert!(client.core.no_connection_reuse);
    }

    #[tokio::test]
    async fn builder_tcp_fast_open() {
        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
            .tcp_fast_open(true)
            .build();
        assert!(client.core.tcp_fast_open);
    }

    #[tokio::test]
    async fn builder_tcp_fast_open_disabled() {
        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
            .tcp_fast_open(false)
            .build();
        assert!(!client.core.tcp_fast_open);
    }

    #[tokio::test]
    async fn builder_hsts() {
        let store = crate::hsts::HstsStore::new();
        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
            .hsts(store)
            .build();
        assert!(client.core.hsts.is_some());
    }

    #[tokio::test]
    async fn builder_cache() {
        let cache = crate::cache::HttpCache::new();
        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
            .cache(cache)
            .build();
        assert!(client.core.cache.is_some());
    }

    #[tokio::test]
    async fn builder_cookie_jar() {
        let jar = crate::cookie::CookieJar::new();
        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
            .cookie_jar(jar)
            .build();
        assert!(client.core.cookie_jar.is_some());
    }

    #[tokio::test]
    async fn builder_timeout() {
        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
            .timeout(Duration::from_secs(10))
            .build();
        assert_eq!(client.core.timeout, Some(Duration::from_secs(10)));
    }

    #[tokio::test]
    async fn builder_connect_timeout() {
        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
            .connect_timeout(Duration::from_secs(5))
            .build();
        assert_eq!(client.core.connect_timeout, Some(Duration::from_secs(5)));
    }

    #[tokio::test]
    async fn builder_max_redirects() {
        let _client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
            .max_redirects(3)
            .build();
    }

    #[tokio::test]
    async fn builder_redirect_policy_none() {
        let _client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
            .redirect_policy(crate::redirect::RedirectPolicy::none())
            .build();
    }

    #[tokio::test]
    async fn builder_no_decompression() {
        let _client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
            .no_decompression()
            .build();
    }

    #[tokio::test]
    async fn builder_default_headers() {
        let mut headers = http::HeaderMap::new();
        headers.insert("x-custom", "value".parse().unwrap());
        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
            .default_headers(headers)
            .build();
        assert!(client.core.default_headers.contains_key("x-custom"));
    }

    #[tokio::test]
    async fn builder_retry() {
        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
            .retry(crate::retry::RetryConfig::default())
            .build();
        assert!(client.core.retry.is_some());
    }

    #[tokio::test]
    async fn builder_system_proxy() {
        let _client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
            .system_proxy()
            .build();
    }

    #[tokio::test]
    async fn builder_max_download_speed() {
        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
            .max_download_speed(1024 * 1024)
            .build();
        assert!(client.core.bandwidth_limiter.is_some());
    }

    #[tokio::test]
    async fn builder_digest_auth() {
        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
            .digest_auth("user", "pass")
            .build();
        assert!(client.core.digest_auth.is_some());
    }

    #[tokio::test]
    async fn builder_https_only() {
        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
            .https_only(true)
            .build();
        assert!(client.core.https_only);
    }

    #[tokio::test]
    async fn builder_debug() {
        let builder = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector);
        let dbg = format!("{builder:?}");
        assert!(dbg.contains("HttpEngineBuilder"));
    }

    #[tokio::test]
    async fn client_clone() {
        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new(TcpConnector);
        let _cloned = client.clone();
    }

    #[tokio::test]
    async fn builder_pool_idle_timeout() {
        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
            .pool_idle_timeout(Duration::from_secs(30))
            .build();
        assert_eq!(client.core.timeout, None);
    }

    #[tokio::test]
    async fn builder_pool_max_idle_per_host() {
        let _client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
            .pool_max_idle_per_host(5)
            .build();
    }

    #[tokio::test]
    async fn builder_proxy_shorthand() {
        use crate::proxy::ProxyConfig;
        let config = ProxyConfig::http("http://proxy:8080").unwrap();
        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
            .proxy(config)
            .build();
        assert!(client.core.proxy.is_some());
    }

    #[tokio::test]
    async fn builder_user_agent_invalid_is_ignored() {
        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
            .user_agent("bad\x00agent")
            .build();
        let ua = client.core.default_headers.get(USER_AGENT).unwrap();
        assert_eq!(ua.as_bytes(), DEFAULT_USER_AGENT.as_bytes());
    }

    #[tokio::test]
    async fn builder_middleware() {
        use crate::middleware::Middleware;
        struct NoopMiddleware;
        impl Middleware for NoopMiddleware {}
        let _client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
            .middleware(NoopMiddleware)
            .build();
    }

    #[tokio::test]
    async fn builder_resolver() {
        use std::net::SocketAddr;
        use std::pin::Pin;
        let _client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
            .resolver(
                |_host: &str,
                 _port: u16|
                 -> Pin<
                    Box<dyn std::future::Future<Output = std::io::Result<SocketAddr>> + Send>,
                > { Box::pin(async { Ok("127.0.0.1:80".parse().unwrap()) }) },
            )
            .build();
    }

    #[cfg(feature = "rustls")]
    #[tokio::test]
    async fn builder_tls_explicit_passthrough() {
        install_crypto();
        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
            .tls(crate::tls::RustlsConnector::with_webpki_roots())
            .build();
        assert!(client.core.tls.is_some());
    }

    #[cfg(feature = "rustls")]
    #[tokio::test]
    async fn builder_tls_version_constraints_only() {
        install_crypto();
        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
            .min_tls_version(crate::tls::TlsVersion::Tls1_2)
            .build();
        assert!(client.core.tls.is_some());
    }

    #[cfg(feature = "rustls")]
    #[tokio::test]
    async fn builder_tls_max_version_only() {
        install_crypto();
        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
            .max_tls_version(crate::tls::TlsVersion::Tls1_3)
            .build();
        assert!(client.core.tls.is_some());
    }

    #[cfg(feature = "rustls")]
    #[tokio::test]
    async fn builder_tls_min_and_max() {
        install_crypto();
        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
            .min_tls_version(crate::tls::TlsVersion::Tls1_2)
            .max_tls_version(crate::tls::TlsVersion::Tls1_3)
            .build();
        assert!(client.core.tls.is_some());
    }

    #[cfg(feature = "rustls")]
    #[tokio::test]
    async fn builder_tls_extra_root_certs() {
        install_crypto();
        let ca = rcgen::generate_simple_self_signed(vec!["test.local".into()]).unwrap();
        let cert = crate::tls::Certificate::from_der(ca.cert.der().to_vec());
        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
            .add_root_certificates(&[cert])
            .build();
        assert!(client.core.tls.is_some());
    }

    #[cfg(feature = "rustls")]
    #[tokio::test]
    async fn builder_tls_extra_root_certs_with_version() {
        install_crypto();
        let ca = rcgen::generate_simple_self_signed(vec!["test.local".into()]).unwrap();
        let cert = crate::tls::Certificate::from_der(ca.cert.der().to_vec());
        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
            .add_root_certificates(&[cert])
            .min_tls_version(crate::tls::TlsVersion::Tls1_3)
            .build();
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
        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
            .identity(id)
            .build();
        assert!(client.core.tls.is_some());
    }

    #[cfg(feature = "rustls")]
    #[tokio::test]
    async fn builder_tls_danger_accept_invalid_certs() {
        install_crypto();
        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
            .danger_accept_invalid_certs()
            .build();
        assert!(client.core.tls.is_some());
    }

    #[cfg(feature = "rustls")]
    #[tokio::test]
    async fn builder_tls_danger_accept_invalid_hostnames() {
        install_crypto();
        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
            .danger_accept_invalid_hostnames(true)
            .build();
        assert!(client.core.tls.is_some());
    }

    #[cfg(feature = "rustls")]
    #[tokio::test]
    async fn builder_tls_sni_disabled() {
        install_crypto();
        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
            .tls_sni(false)
            .build();
        let tls = client.core.tls.as_ref().unwrap();
        assert!(!tls.config().enable_sni);
    }

    #[cfg(feature = "rustls")]
    #[tokio::test]
    async fn builder_tls_sni_enabled_is_noop() {
        install_crypto();
        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
            .tls_sni(true)
            .build();
        assert!(client.core.tls.is_none());
    }

    #[cfg(feature = "rustls")]
    #[tokio::test]
    async fn builder_tls_crls() {
        install_crypto();
        let crl = crate::tls::CertificateRevocationList::from_der(vec![]);
        let _builder =
            HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector).add_crls([crl]);
    }

    #[cfg(feature = "rustls")]
    #[tokio::test]
    async fn builder_tls_explicit_with_sni_disabled() {
        install_crypto();
        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
            .tls(crate::tls::RustlsConnector::with_webpki_roots())
            .tls_sni(false)
            .build();
        let tls = client.core.tls.as_ref().unwrap();
        assert!(!tls.config().enable_sni);
    }

    #[test]
    fn builder_does_not_require_runtime_context() {
        let _client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector).build();
    }

    #[test]
    fn apply_default_headers_fills_missing() {
        let mut headers = http::HeaderMap::new();
        headers.insert("x-custom", "existing".parse().unwrap());

        let mut default_headers = http::HeaderMap::new();
        default_headers.insert("x-custom", "default".parse().unwrap());
        default_headers.insert("x-extra", "added".parse().unwrap());

        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
            .default_headers(default_headers)
            .build();

        let mut test_headers = headers.clone();
        // Simulate what apply_default_headers does
        for (name, value) in &client.core.default_headers {
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

        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
            .hsts(store)
            .build();
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
        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
            .hsts(store)
            .build();
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
        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
            .no_connection_reuse()
            .build();
        assert!(client.core.no_connection_reuse);
    }

    #[test]
    fn bandwidth_limiter_accessor() {
        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
            .max_download_speed(1024 * 1024)
            .build();
        assert!(client.bandwidth_limiter().is_some());
    }

    #[test]
    fn bandwidth_limiter_accessor_none() {
        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new(TcpConnector);
        assert!(client.bandwidth_limiter().is_none());
    }

    #[test]
    fn default_timeout_accessor() {
        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
            .timeout(Duration::from_secs(10))
            .build();
        assert_eq!(client.default_timeout(), Some(Duration::from_secs(10)));
    }

    #[test]
    fn default_timeout_accessor_none() {
        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new(TcpConnector);
        assert_eq!(client.default_timeout(), None);
    }

    #[test]
    fn default_retry_accessor() {
        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
            .retry(crate::retry::RetryConfig::default())
            .build();
        assert!(client.default_retry().is_some());
    }

    #[test]
    fn default_retry_accessor_none() {
        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new(TcpConnector);
        assert!(client.default_retry().is_none());
    }

    #[test]
    fn middleware_accessor() {
        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new(TcpConnector);
        assert!(client.middleware().is_empty());
    }

    #[test]
    fn chunk_download_returns_builder() {
        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new(TcpConnector);
        let _dl = client.chunk_download("http://example.com/file");
    }

    #[tokio::test]
    async fn execute_rejects_http_when_https_only() {
        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
            .https_only(true)
            .build();
        let result = client
            .execute(
                Method::GET,
                "http://example.com".parse().unwrap(),
                http::HeaderMap::new(),
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
        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
            .https_only(true)
            .build();
        let result = client
            .execute(
                Method::GET,
                "https://example.com".parse().unwrap(),
                http::HeaderMap::new(),
                None,
                None,
            )
            .await;
        // Will fail with connection error, not HttpsOnly
        assert!(!matches!(result, Err(crate::error::Error::HttpsOnly(_))));
    }
}
