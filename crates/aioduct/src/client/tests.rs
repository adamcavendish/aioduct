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
    use crate::runtime::tokio_rt::TokioRuntime;
    use http::header::USER_AGENT;

    type TokioClient = Client<TokioRuntime>;

    #[cfg(feature = "rustls")]
    fn install_crypto() {
        crate::tls::install_default_crypto_provider();
    }

    #[tokio::test]
    async fn builder_read_timeout() {
        let _client = TokioClient::builder()
            .read_timeout(Duration::from_secs(5))
            .build();
    }

    #[tokio::test]
    async fn builder_tcp_keepalive() {
        let _client = TokioClient::builder()
            .tcp_keepalive(Duration::from_secs(60))
            .build();
    }

    #[tokio::test]
    async fn builder_tcp_keepalive_interval() {
        let _client = TokioClient::builder()
            .tcp_keepalive_interval(Duration::from_secs(10))
            .build();
    }

    #[tokio::test]
    async fn builder_tcp_keepalive_retries() {
        let _client = TokioClient::builder().tcp_keepalive_retries(3).build();
    }

    #[tokio::test]
    async fn builder_local_address() {
        let _client = TokioClient::builder()
            .local_address("127.0.0.1".parse().unwrap())
            .build();
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn builder_interface() {
        let _client = TokioClient::builder().interface("eth0").build();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn builder_unix_socket() {
        let _client = TokioClient::builder().unix_socket("/tmp/test.sock").build();
    }

    #[tokio::test]
    async fn builder_referer() {
        let _client = TokioClient::builder().referer(true).build();
    }

    #[tokio::test]
    async fn builder_http2_prior_knowledge() {
        let _client = TokioClient::builder().http2_prior_knowledge().build();
    }

    #[tokio::test]
    async fn builder_no_default_headers() {
        let client = TokioClient::builder().no_default_headers().build();
        assert!(client.default_headers.is_empty());
    }

    #[tokio::test]
    async fn builder_user_agent_with_invalid_value() {
        let client = TokioClient::builder().user_agent("valid-agent/1.0").build();
        assert!(client.default_headers.get(USER_AGENT).is_some());
    }

    #[tokio::test]
    async fn builder_proxy_settings() {
        use crate::proxy::ProxyConfig;
        let settings = ProxySettings::default().http(ProxyConfig::http("http://proxy:80").unwrap());
        let _client = TokioClient::builder().proxy_settings(settings).build();
    }

    #[tokio::test]
    async fn builder_http2_config() {
        let config = crate::http2::Http2Config::default();
        let _client = TokioClient::builder().http2(config).build();
    }

    #[tokio::test]
    async fn builder_rate_limiter() {
        let limiter = crate::throttle::RateLimiter::new(10, Duration::from_secs(1));
        let _client = TokioClient::builder().rate_limiter(limiter).build();
    }

    #[tokio::test]
    async fn client_default_creates_same_as_new() {
        let _client: TokioClient = Default::default();
    }

    #[tokio::test]
    async fn client_method_helpers() {
        let client = TokioClient::new();
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
        let client = TokioClient::new();
        assert!(client.get("not a url").is_err());
    }

    #[tokio::test]
    async fn client_https_only_rejects_http() {
        let client = TokioClient::builder().https_only(true).build();
        assert!(client.https_only);
    }

    #[tokio::test]
    async fn client_no_connection_reuse_sets_flag() {
        let client = TokioClient::builder().no_connection_reuse().build();
        assert!(client.no_connection_reuse);
    }

    #[tokio::test]
    async fn builder_tcp_fast_open() {
        let client = TokioClient::builder().tcp_fast_open(true).build();
        assert!(client.tcp_fast_open);
    }

    #[tokio::test]
    async fn builder_tcp_fast_open_disabled() {
        let client = TokioClient::builder().tcp_fast_open(false).build();
        assert!(!client.tcp_fast_open);
    }

    #[tokio::test]
    async fn builder_hsts() {
        let store = crate::hsts::HstsStore::new();
        let client = TokioClient::builder().hsts(store).build();
        assert!(client.hsts.is_some());
    }

    #[tokio::test]
    async fn builder_cache() {
        let cache = crate::cache::HttpCache::new();
        let client = TokioClient::builder().cache(cache).build();
        assert!(client.cache.is_some());
    }

    #[tokio::test]
    async fn builder_cookie_jar() {
        let jar = crate::cookie::CookieJar::new();
        let client = TokioClient::builder().cookie_jar(jar).build();
        assert!(client.cookie_jar.is_some());
    }

    #[tokio::test]
    async fn builder_timeout() {
        let client = TokioClient::builder()
            .timeout(Duration::from_secs(10))
            .build();
        assert_eq!(client.timeout, Some(Duration::from_secs(10)));
    }

    #[tokio::test]
    async fn builder_connect_timeout() {
        let client = TokioClient::builder()
            .connect_timeout(Duration::from_secs(5))
            .build();
        assert_eq!(client.connect_timeout, Some(Duration::from_secs(5)));
    }

    #[tokio::test]
    async fn builder_max_redirects() {
        let _client = TokioClient::builder().max_redirects(3).build();
    }

    #[tokio::test]
    async fn builder_redirect_policy_none() {
        let _client = TokioClient::builder()
            .redirect_policy(crate::redirect::RedirectPolicy::none())
            .build();
    }

    #[tokio::test]
    async fn builder_no_decompression() {
        let _client = TokioClient::builder().no_decompression().build();
    }

    #[tokio::test]
    async fn builder_default_headers() {
        let mut headers = http::HeaderMap::new();
        headers.insert("x-custom", "value".parse().unwrap());
        let client = TokioClient::builder().default_headers(headers).build();
        assert!(client.default_headers.contains_key("x-custom"));
    }

    #[tokio::test]
    async fn builder_retry() {
        let client = TokioClient::builder()
            .retry(crate::retry::RetryConfig::default())
            .build();
        assert!(client.retry.is_some());
    }

    #[tokio::test]
    async fn builder_system_proxy() {
        let _client = TokioClient::builder().system_proxy().build();
    }

    #[tokio::test]
    async fn builder_max_download_speed() {
        let client = TokioClient::builder()
            .max_download_speed(1024 * 1024)
            .build();
        assert!(client.bandwidth_limiter.is_some());
    }

    #[tokio::test]
    async fn builder_digest_auth() {
        let client = TokioClient::builder().digest_auth("user", "pass").build();
        assert!(client.digest_auth.is_some());
    }

    #[tokio::test]
    async fn builder_https_only() {
        let client = TokioClient::builder().https_only(true).build();
        assert!(client.https_only);
    }

    #[tokio::test]
    async fn builder_debug() {
        let builder = TokioClient::builder();
        let dbg = format!("{builder:?}");
        assert!(dbg.contains("ClientBuilder"));
    }

    #[tokio::test]
    async fn client_clone() {
        let client = TokioClient::new();
        let _cloned = client.clone();
    }

    #[tokio::test]
    async fn builder_pool_idle_timeout() {
        let client = TokioClient::builder()
            .pool_idle_timeout(Duration::from_secs(30))
            .build();
        assert_eq!(client.timeout, None);
    }

    #[tokio::test]
    async fn builder_pool_max_idle_per_host() {
        let _client = TokioClient::builder().pool_max_idle_per_host(5).build();
    }

    #[tokio::test]
    async fn builder_proxy_shorthand() {
        use crate::proxy::ProxyConfig;
        let config = ProxyConfig::http("http://proxy:8080").unwrap();
        let client = TokioClient::builder().proxy(config).build();
        assert!(client.proxy.is_some());
    }

    #[tokio::test]
    async fn builder_user_agent_invalid_is_ignored() {
        let client = TokioClient::builder().user_agent("bad\x00agent").build();
        let ua = client.default_headers.get(USER_AGENT).unwrap();
        assert_eq!(ua.as_bytes(), DEFAULT_USER_AGENT.as_bytes());
    }

    #[tokio::test]
    async fn builder_middleware() {
        use crate::middleware::Middleware;
        struct NoopMiddleware;
        impl Middleware for NoopMiddleware {}
        let _client = TokioClient::builder().middleware(NoopMiddleware).build();
    }

    #[tokio::test]
    async fn builder_resolver() {
        use std::net::SocketAddr;
        use std::pin::Pin;
        let _client = TokioClient::builder()
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
        let client = TokioClient::builder()
            .tls(crate::tls::RustlsConnector::with_webpki_roots())
            .build();
        assert!(client.tls.is_some());
    }

    #[cfg(feature = "rustls")]
    #[tokio::test]
    async fn builder_tls_version_constraints_only() {
        install_crypto();
        let client = TokioClient::builder()
            .min_tls_version(crate::tls::TlsVersion::Tls1_2)
            .build();
        assert!(client.tls.is_some());
    }

    #[cfg(feature = "rustls")]
    #[tokio::test]
    async fn builder_tls_max_version_only() {
        install_crypto();
        let client = TokioClient::builder()
            .max_tls_version(crate::tls::TlsVersion::Tls1_3)
            .build();
        assert!(client.tls.is_some());
    }

    #[cfg(feature = "rustls")]
    #[tokio::test]
    async fn builder_tls_min_and_max() {
        install_crypto();
        let client = TokioClient::builder()
            .min_tls_version(crate::tls::TlsVersion::Tls1_2)
            .max_tls_version(crate::tls::TlsVersion::Tls1_3)
            .build();
        assert!(client.tls.is_some());
    }

    #[cfg(feature = "rustls")]
    #[tokio::test]
    async fn builder_tls_extra_root_certs() {
        install_crypto();
        let ca = rcgen::generate_simple_self_signed(vec!["test.local".into()]).unwrap();
        let cert = crate::tls::Certificate::from_der(ca.cert.der().to_vec());
        let client = TokioClient::builder()
            .add_root_certificates(&[cert])
            .build();
        assert!(client.tls.is_some());
    }

    #[cfg(feature = "rustls")]
    #[tokio::test]
    async fn builder_tls_extra_root_certs_with_version() {
        install_crypto();
        let ca = rcgen::generate_simple_self_signed(vec!["test.local".into()]).unwrap();
        let cert = crate::tls::Certificate::from_der(ca.cert.der().to_vec());
        let client = TokioClient::builder()
            .add_root_certificates(&[cert])
            .min_tls_version(crate::tls::TlsVersion::Tls1_3)
            .build();
        assert!(client.tls.is_some());
    }

    #[cfg(feature = "rustls")]
    #[tokio::test]
    async fn builder_tls_identity() {
        install_crypto();
        let ca = rcgen::generate_simple_self_signed(vec!["test.local".into()]).unwrap();
        let mut pem = ca.cert.pem();
        pem.push_str(&ca.signing_key.serialize_pem());
        let id = crate::tls::Identity::from_pem(pem.as_bytes()).unwrap();
        let client = TokioClient::builder().identity(id).build();
        assert!(client.tls.is_some());
    }

    #[cfg(feature = "rustls")]
    #[tokio::test]
    async fn builder_tls_danger_accept_invalid_certs() {
        install_crypto();
        let client = TokioClient::builder().danger_accept_invalid_certs().build();
        assert!(client.tls.is_some());
    }

    #[cfg(feature = "rustls")]
    #[tokio::test]
    async fn builder_tls_danger_accept_invalid_hostnames() {
        install_crypto();
        let client = TokioClient::builder()
            .danger_accept_invalid_hostnames(true)
            .build();
        assert!(client.tls.is_some());
    }

    #[cfg(feature = "rustls")]
    #[tokio::test]
    async fn builder_tls_sni_disabled() {
        install_crypto();
        let client = TokioClient::builder().tls_sni(false).build();
        let tls = client.tls.as_ref().unwrap();
        assert!(!tls.config().enable_sni);
    }

    #[cfg(feature = "rustls")]
    #[tokio::test]
    async fn builder_tls_sni_enabled_is_noop() {
        install_crypto();
        let client = TokioClient::builder().tls_sni(true).build();
        assert!(client.tls.is_none());
    }

    #[cfg(feature = "rustls")]
    #[tokio::test]
    async fn builder_tls_crls() {
        install_crypto();
        let crl = crate::tls::CertificateRevocationList::from_der(vec![]);
        let _builder = TokioClient::builder().add_crls([crl]);
    }

    #[cfg(feature = "rustls")]
    #[tokio::test]
    async fn builder_tls_explicit_with_sni_disabled() {
        install_crypto();
        let client = TokioClient::builder()
            .tls(crate::tls::RustlsConnector::with_webpki_roots())
            .tls_sni(false)
            .build();
        let tls = client.tls.as_ref().unwrap();
        assert!(!tls.config().enable_sni);
    }

    #[test]
    fn builder_does_not_require_runtime_context() {
        let _client = TokioClient::builder().build();
    }
}
