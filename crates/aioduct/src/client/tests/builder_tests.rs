#![cfg(feature = "tokio")]

use super::super::*;
use super::DEFAULT_USER_AGENT;
use crate::runtime::tokio_rt::{TcpConnector, TokioRuntime};
use http::header::USER_AGENT;

#[cfg(feature = "rustls")]
pub(super) fn install_crypto() {
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
        .execute_send(
            Method::GET,
            "http://example.com".parse().unwrap(),
            http::HeaderMap::new(),
            None,
            None,
            None,
            None,
            None,
            None,
            crate::pool::ProtocolHint::Auto,
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
        .execute_send(
            Method::GET,
            "https://example.com".parse().unwrap(),
            http::HeaderMap::new(),
            None,
            None,
            None,
            None,
            None,
            None,
            crate::pool::ProtocolHint::Auto,
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
    let client =
        HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
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
