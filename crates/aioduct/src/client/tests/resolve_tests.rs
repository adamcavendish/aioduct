#![cfg(all(test, feature = "tokio"))]
#[cfg(all(test, feature = "tokio"))]
mod tests {
    use crate::client::HttpEngineSend;
    use crate::runtime::tokio_rt::{TcpConnector, TokioRuntime};

    /// Build an engine with NO resolver configured.
    /// We achieve this by building normally then clearing the resolver field.
    fn engine_no_resolver() -> HttpEngineSend<TokioRuntime, TcpConnector> {
        let mut engine = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
            .build()
            .unwrap();
        engine.core.resolver = None;
        engine
    }

    /// Build an engine WITH a resolver.
    fn engine_with_resolver() -> HttpEngineSend<TokioRuntime, TcpConnector> {
        use std::future::Future;
        use std::io;
        use std::net::SocketAddr;
        use std::pin::Pin;

        let resolver =
            move |_host: &str,
                  _port: u16|
                  -> Pin<Box<dyn Future<Output = io::Result<SocketAddr>> + Send>> {
                Box::pin(async { Ok("10.0.0.1:80".parse().unwrap()) })
            };
        HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
            .resolver(resolver)
            .build()
            .unwrap()
    }

    #[tokio::test]
    async fn resolve_authority_with_ip_literal_skips_dns() {
        // When the host is already an IP address, no resolver is needed
        let engine = engine_no_resolver();
        let authority: http::uri::Authority = "127.0.0.1:8080".parse().unwrap();
        let result = engine.core.resolve_authority(&authority, 80).await;
        assert!(result.is_ok());
        let addr = result.unwrap();
        assert_eq!(
            addr,
            "127.0.0.1:8080".parse::<std::net::SocketAddr>().unwrap()
        );
    }

    #[tokio::test]
    async fn resolve_authority_uses_default_port_when_none_specified() {
        // Authority without explicit port should use the provided default_port
        let engine = engine_no_resolver();
        let authority: http::uri::Authority = "192.168.1.1".parse().unwrap();
        let result = engine.core.resolve_authority(&authority, 443).await;
        assert!(result.is_ok());
        let addr = result.unwrap();
        assert_eq!(
            addr,
            "192.168.1.1:443".parse::<std::net::SocketAddr>().unwrap()
        );
    }

    #[tokio::test]
    async fn resolve_authority_no_resolver_returns_error_for_hostname() {
        // When no resolver is configured and host is not an IP literal, should error
        let engine = engine_no_resolver();
        let authority: http::uri::Authority = "example.com:80".parse().unwrap();
        let result = engine.core.resolve_authority(&authority, 80).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("no DNS resolver configured"),
            "error message should mention missing resolver, got: {msg}"
        );
        assert!(
            msg.contains("example.com"),
            "error message should contain the host, got: {msg}"
        );
    }

    #[tokio::test]
    async fn resolve_all_authority_raw_no_resolver_returns_error() {
        let engine = engine_no_resolver();
        let result = engine
            .core
            .resolve_all_authority_raw("hostname.test", 443)
            .await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("hostname.test"));
    }

    #[tokio::test]
    async fn resolve_authority_with_resolver_resolves_hostname() {
        let engine = engine_with_resolver();
        let authority: http::uri::Authority = "example.com:80".parse().unwrap();
        let result = engine.core.resolve_authority(&authority, 80).await;
        assert!(result.is_ok());
        let addr = result.unwrap();
        assert_eq!(addr, "10.0.0.1:80".parse::<std::net::SocketAddr>().unwrap());
    }

    #[tokio::test]
    async fn resolve_all_authority_raw_with_ip_literal() {
        let engine = engine_no_resolver();
        let result = engine
            .core
            .resolve_all_authority_raw("10.20.30.40", 9090)
            .await;
        assert!(result.is_ok());
        let addrs = result.unwrap();
        assert_eq!(addrs.len(), 1);
        assert_eq!(
            addrs[0],
            "10.20.30.40:9090".parse::<std::net::SocketAddr>().unwrap()
        );
    }

    #[tokio::test]
    async fn resolve_all_authority_raw_with_bare_ipv6_literal() {
        let engine = engine_no_resolver();
        let result = engine.core.resolve_all_authority_raw("::1", 9090).await;
        assert!(result.is_ok());
        let addrs = result.unwrap();
        assert_eq!(addrs.len(), 1);
        assert_eq!(
            addrs[0],
            "[::1]:9090".parse::<std::net::SocketAddr>().unwrap()
        );
    }

    #[tokio::test]
    async fn resolve_authority_ipv6_literal() {
        let engine = engine_no_resolver();
        // IPv6 literal in authority uses brackets
        let authority: http::uri::Authority = "[::1]:8080".parse().unwrap();
        let result = engine.core.resolve_authority(&authority, 80).await;
        assert!(result.is_ok());
        let addr = result.unwrap();
        assert_eq!(addr, "[::1]:8080".parse::<std::net::SocketAddr>().unwrap());
    }

    #[cfg(all(feature = "http3", feature = "rustls"))]
    #[test]
    fn cache_alt_svc_stores_entries() {
        let engine = engine_with_resolver();
        let uri: http::Uri = "https://example.com/path".parse().unwrap();
        let mut headers = http::HeaderMap::new();
        headers.insert(
            http::header::ALT_SVC,
            http::HeaderValue::from_static("h3=\":443\"; ma=3600"),
        );
        engine.core.cache_alt_svc(&uri, &headers);
        // Verify the entry was cached via lookup
        let authority: http::uri::Authority = "example.com".parse().unwrap();
        let cached = engine.core.alt_svc_cache.lookup_h3(&authority);
        assert!(cached.is_some(), "alt-svc entry should be cached");
        let (host, port) = cached.unwrap();
        assert!(host.is_none(), "h3=\":443\" has no explicit host");
        assert_eq!(port, 443);
    }

    #[cfg(all(feature = "http3", feature = "rustls"))]
    #[test]
    fn cache_alt_svc_no_header_does_nothing() {
        let engine = engine_with_resolver();
        let uri: http::Uri = "https://example.com/path".parse().unwrap();
        let headers = http::HeaderMap::new(); // no ALT_SVC header
        engine.core.cache_alt_svc(&uri, &headers);
        let authority: http::uri::Authority = "example.com".parse().unwrap();
        let cached = engine.core.alt_svc_cache.lookup_h3(&authority);
        assert!(cached.is_none(), "no entry should be cached without header");
    }

    #[cfg(all(feature = "http3", feature = "rustls"))]
    #[test]
    fn cache_alt_svc_no_authority_in_uri_does_nothing() {
        let engine = engine_with_resolver();
        // A URI without authority (relative path)
        let uri: http::Uri = "/relative/path".parse().unwrap();
        let mut headers = http::HeaderMap::new();
        headers.insert(
            http::header::ALT_SVC,
            http::HeaderValue::from_static("h3=\":443\""),
        );
        engine.core.cache_alt_svc(&uri, &headers);
        // Nothing to assert on the cache since there's no authority key,
        // but we verify it doesn't panic
    }
}
