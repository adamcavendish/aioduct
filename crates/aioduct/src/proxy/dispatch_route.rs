use http::Uri;
use http::uri::{Authority, Scheme};

use crate::error::Error;
use crate::pool::{ProtocolHint, ProxyRoute};

use super::{ProxyChain, ProxyConfig, ProxyEstablishmentPlan, ProxySettings};

#[derive(Clone)]
pub(crate) struct ProxyDestination {
    scheme: Scheme,
    authority: Authority,
    effective_port: u16,
}

impl ProxyDestination {
    pub(super) fn from_uri(uri: &Uri) -> Result<Self, Error> {
        let parsed_scheme = uri
            .scheme()
            .cloned()
            .ok_or_else(|| Error::InvalidUrl("missing scheme".into()))?;
        let authority = uri
            .authority()
            .cloned()
            .ok_or_else(|| Error::InvalidUrl("missing authority".into()))?;
        if authority.as_str().contains('@') {
            return Err(Error::InvalidUrl(
                "destination authority must not contain userinfo".into(),
            ));
        }
        let (scheme, default_port) = match parsed_scheme.as_str() {
            value if value.eq_ignore_ascii_case("http") => (Scheme::HTTP, 80),
            value if value.eq_ignore_ascii_case("https") => (Scheme::HTTPS, 443),
            other => {
                return Err(Error::Unsupported(format!(
                    "proxy routing does not support URI scheme `{other}`"
                )));
            }
        };
        let effective_port = match authority.port_u16() {
            Some(port) => port,
            None if authority.as_str() != authority.host() => {
                return Err(Error::InvalidUrl(format!(
                    "invalid port in authority `{authority}`"
                )));
            }
            None => default_port,
        };
        Ok(Self {
            scheme,
            authority,
            effective_port,
        })
    }

    pub(crate) fn scheme(&self) -> &Scheme {
        &self.scheme
    }

    pub(crate) fn authority(&self) -> &Authority {
        &self.authority
    }

    pub(crate) fn effective_port(&self) -> u16 {
        self.effective_port
    }
}

impl std::fmt::Debug for ProxyDestination {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProxyDestination")
            .field("scheme", &self.scheme)
            .field("host", &self.authority.host())
            .field("effective_port", &self.effective_port)
            .finish()
    }
}

#[derive(Clone, Debug)]
enum ProxySelection {
    Direct,
    Single(ProxyConfig),
    Chain(ProxyChain),
}

#[derive(Clone, Debug)]
pub(crate) struct ProxyDispatchRoute {
    destination: ProxyDestination,
    selection: ProxySelection,
    pool_identity: ProxyRoute,
    protocol_hint: ProtocolHint,
}

impl ProxyDispatchRoute {
    pub(crate) fn resolve(
        uri: &Uri,
        chain: Option<&ProxyChain>,
        settings: Option<&ProxySettings>,
        requested_protocol: ProtocolHint,
        cached_h2c: Option<bool>,
    ) -> Result<Self, Error> {
        let destination = ProxyDestination::from_uri(uri)?;
        let selection = if let Some(chain) = chain {
            let mut chain = chain.clone();
            if let Some(settings) = settings {
                for proxy in &mut chain.proxies {
                    settings.resolve_credentials(proxy);
                }
            }
            ProxySelection::Chain(chain)
        } else if let Some(settings) = settings {
            settings.validate_for_uri(uri)?;
            match settings.proxy_for(uri) {
                Some(proxy) => ProxySelection::Single(proxy),
                None => ProxySelection::Direct,
            }
        } else {
            ProxySelection::Direct
        };
        let pool_identity = match &selection {
            ProxySelection::Direct => ProxyRoute::DIRECT,
            ProxySelection::Single(proxy) => ProxyRoute::proxied(proxy.route_identity()),
            ProxySelection::Chain(chain) => ProxyRoute::proxied(chain.route_identity()),
        };
        let is_proxied = !matches!(selection, ProxySelection::Direct);
        let protocol_hint = match requested_protocol {
            ProtocolHint::AdaptiveH2c => match cached_h2c {
                Some(true) => ProtocolHint::H2c,
                Some(false) => ProtocolHint::Auto,
                None if is_proxied => ProtocolHint::Auto,
                None => ProtocolHint::AdaptiveH2c,
            },
            other => other,
        };

        Ok(Self {
            destination,
            selection,
            pool_identity,
            protocol_hint,
        })
    }

    pub(crate) fn destination(&self) -> &ProxyDestination {
        &self.destination
    }

    pub(crate) fn is_proxied(&self) -> bool {
        !matches!(self.selection, ProxySelection::Direct)
    }

    pub(crate) fn pool_identity(&self) -> ProxyRoute {
        self.pool_identity.clone()
    }

    pub(crate) fn protocol_hint(&self) -> ProtocolHint {
        self.protocol_hint
    }

    #[cfg(test)]
    pub(crate) fn establishment_plan(&self) -> Result<Option<ProxyEstablishmentPlan>, Error> {
        self.establishment_plan_with_protocol(self.protocol_hint)
    }

    pub(crate) fn establishment_plan_with_protocol(
        &self,
        protocol_hint: ProtocolHint,
    ) -> Result<Option<ProxyEstablishmentPlan>, Error> {
        let proxies = match &self.selection {
            ProxySelection::Direct => return Ok(None),
            ProxySelection::Single(proxy) => vec![proxy.clone()],
            ProxySelection::Chain(chain) => chain.proxies.clone(),
        };
        ProxyEstablishmentPlan::new(&self.destination, proxies, protocol_hint).map(Some)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use crate::proxy_credential::CredentialResolver;

    use super::*;

    struct RecordingCredentialResolver {
        calls: Arc<Mutex<Vec<String>>>,
        username: &'static str,
        password: String,
    }

    impl CredentialResolver for RecordingCredentialResolver {
        fn resolve(&self, key: &str) -> Option<(String, String)> {
            self.calls.lock().unwrap().push(key.to_owned());
            Some((self.username.to_owned(), self.password.clone()))
        }
    }

    fn test_secret(byte: u8) -> String {
        String::from_utf8(vec![byte; 12]).unwrap()
    }

    #[test]
    fn destination_materializes_default_ports_for_all_host_forms() {
        for (uri, port) in [
            ("http://example.test/path", 80),
            ("https://example.test/path", 443),
            ("http://127.0.0.1/path", 80),
            ("https://[2001:db8::1]/path", 443),
            ("https://example.test:8443/path", 8443),
        ] {
            let destination = ProxyDestination::from_uri(&uri.parse().unwrap()).unwrap();
            assert_eq!(destination.effective_port(), port);
        }
    }

    #[test]
    fn destination_rejects_invalid_explicit_ports() {
        for value in [
            "http://example.test:99999/path",
            "http://example.test:/path",
            "http://example.test:not-a-port/path",
            "https://[2001:db8::1]:99999/path",
            "https://[2001:db8::1]:/path",
            "https://[2001:db8::1]:not-a-port/path",
        ] {
            let uri = value
                .parse::<Uri>()
                .unwrap_or_else(|error| panic!("http::Uri rejected {value}: {error}"));
            let error = ProxyDestination::from_uri(&uri).unwrap_err();
            assert!(
                matches!(error, Error::InvalidUrl(ref message) if message.contains("invalid port")),
                "unexpected validation error for {value}: {error}"
            );
        }
    }

    #[test]
    fn destination_userinfo_is_rejected_without_disclosing_credentials() {
        let password = test_secret(b'z');
        let uri: Uri = format!("http://visible-user:{password}@example.test/path")
            .parse()
            .unwrap();
        let error = ProxyDestination::from_uri(&uri).unwrap_err();
        let message = error.to_string();
        assert!(message.contains("must not contain userinfo"));
        assert!(!message.contains("visible-user"));
        assert!(!message.contains(&password));

        let destination = ProxyDestination {
            scheme: Scheme::HTTP,
            authority: uri.authority().unwrap().clone(),
            effective_port: 80,
        };
        let debug = format!("{destination:?}");
        assert!(!debug.contains("visible-user"));
        assert!(!debug.contains(&password));
    }

    #[test]
    fn destination_canonicalizes_case_preserving_http_schemes() {
        let uppercase = "HTTP".parse::<Scheme>().unwrap();
        assert_eq!(uppercase.as_str(), "HTTP");
        assert_ne!(uppercase, Scheme::HTTP, "proof premise changed");

        for (value, expected_scheme, expected_port) in [
            ("HTTP://EXAMPLE.test/path", Scheme::HTTP, 80),
            ("HtTpS://EXAMPLE.test/path", Scheme::HTTPS, 443),
        ] {
            let uri = value.parse::<Uri>().unwrap();
            let destination = ProxyDestination::from_uri(&uri).unwrap();
            assert_eq!(destination.scheme(), &expected_scheme);
            assert_eq!(destination.effective_port(), expected_port);
        }
    }

    #[test]
    fn malformed_env_proxy_configuration_fails_during_pre_io_route_resolution() {
        let _guard = crate::proxy::tests::ENV_MUTEX.lock().unwrap();
        let uri = "http://would-require-dns.invalid/resource"
            .parse::<Uri>()
            .unwrap();
        for value in [
            "http://proxy.test:",
            "http://proxy.test:not-a-port",
            "http://proxy.test:99999",
        ] {
            unsafe {
                std::env::set_var("TEST_ROUTE_PROXY_UPPER", value);
                std::env::set_var("test_route_proxy_lower", "http://proxy.test:8080");
            }
            let settings = ProxySettings::from_env_variables(
                "TEST_ROUTE_PROXY_UPPER",
                "test_route_proxy_lower",
                "TEST_ROUTE_HTTPS_PROXY_UPPER",
                "test_route_https_proxy_lower",
                crate::proxy::NoProxy::default(),
            );

            let error =
                ProxyDispatchRoute::resolve(&uri, None, Some(&settings), ProtocolHint::Auto, None)
                    .unwrap_err();
            assert!(
                matches!(&error, Error::InvalidUrl(message) if message.contains("TEST_ROUTE_PROXY_UPPER")),
                "unexpected route error for {value}: {error}"
            );
        }

        unsafe {
            std::env::remove_var("TEST_ROUTE_PROXY_UPPER");
            std::env::remove_var("test_route_proxy_lower");
        }
    }

    #[test]
    fn explicit_proxy_precedence_does_not_surface_unused_env_errors() {
        let _guard = crate::proxy::tests::ENV_MUTEX.lock().unwrap();
        unsafe {
            std::env::set_var("TEST_PRECEDENCE_HTTP_PROXY", "http://proxy.test:99999");
            std::env::set_var("test_precedence_http_proxy", "http://proxy.test:8080");
            std::env::set_var(
                "TEST_PRECEDENCE_HTTPS_PROXY",
                "http://secure-proxy.test:8443",
            );
        }
        let settings = ProxySettings::from_env_variables(
            "TEST_PRECEDENCE_HTTP_PROXY",
            "test_precedence_http_proxy",
            "TEST_PRECEDENCE_HTTPS_PROXY",
            "test_precedence_https_proxy",
            crate::proxy::NoProxy::default(),
        );

        let https_uri = "https://origin.test/resource".parse::<Uri>().unwrap();
        assert!(
            ProxyDispatchRoute::resolve(
                &https_uri,
                None,
                Some(&settings),
                ProtocolHint::Auto,
                None,
            )
            .unwrap()
            .is_proxied(),
            "an invalid HTTP_PROXY must not poison a valid HTTPS_PROXY route"
        );

        let http_uri = "http://origin.test/resource".parse::<Uri>().unwrap();
        let chain = ProxyChain::new(vec![
            ProxyConfig::http("http://first.test:8080").unwrap(),
            ProxyConfig::socks5("socks5://second.test:1080").unwrap(),
        ]);
        assert!(
            ProxyDispatchRoute::resolve(
                &http_uri,
                Some(&chain),
                Some(&settings),
                ProtocolHint::Auto,
                None,
            )
            .is_ok(),
            "an explicit proxy chain must take precedence over environment proxies"
        );

        let bypassed = settings
            .clone()
            .no_proxy(crate::proxy::NoProxy::new("origin.test"));
        assert!(
            !ProxyDispatchRoute::resolve(
                &http_uri,
                None,
                Some(&bypassed),
                ProtocolHint::Auto,
                None,
            )
            .unwrap()
            .is_proxied(),
            "NO_PROXY must bypass an unused malformed proxy endpoint"
        );

        let overridden = settings.http(ProxyConfig::http("http://override.test:8080").unwrap());
        assert!(
            ProxyDispatchRoute::resolve(
                &http_uri,
                None,
                Some(&overridden),
                ProtocolHint::Auto,
                None,
            )
            .unwrap()
            .is_proxied(),
            "an explicit HTTP proxy must replace its stored environment error"
        );

        unsafe {
            std::env::remove_var("TEST_PRECEDENCE_HTTP_PROXY");
            std::env::remove_var("test_precedence_http_proxy");
            std::env::remove_var("TEST_PRECEDENCE_HTTPS_PROXY");
        }
    }

    #[test]
    fn route_owns_direct_single_and_chain_selections() {
        let uri: Uri = "https://example.test/path".parse().unwrap();
        let direct =
            ProxyDispatchRoute::resolve(&uri, None, None, ProtocolHint::Auto, None).unwrap();
        assert!(!direct.is_proxied());
        assert_eq!(direct.pool_identity(), ProxyRoute::DIRECT);

        let password = test_secret(b'a');
        let first = ProxyConfig::http("http://first.test:8080")
            .unwrap()
            .basic_auth("first", &password);
        let second = ProxyConfig::socks5("socks5://second.test:1080").unwrap();
        let settings = ProxySettings::all(first.clone());
        let single =
            ProxyDispatchRoute::resolve(&uri, None, Some(&settings), ProtocolHint::Auto, None)
                .unwrap();
        assert!(single.is_proxied());
        assert_ne!(single.pool_identity(), ProxyRoute::DIRECT);

        let chain = ProxyChain::new(vec![first, second]);
        let chained = ProxyDispatchRoute::resolve(
            &uri,
            Some(&chain),
            Some(&settings),
            ProtocolHint::Auto,
            None,
        )
        .unwrap();
        assert!(chained.is_proxied());
        assert_ne!(chained.pool_identity(), single.pool_identity());
    }

    #[test]
    fn route_identity_includes_resolved_credentials() {
        let uri: Uri = "https://example.test/path".parse().unwrap();
        let first_password = test_secret(b'a');
        let second_password = test_secret(b'b');
        let first = ProxySettings::all(
            ProxyConfig::http("http://proxy.test:8080")
                .unwrap()
                .basic_auth("user", &first_password),
        );
        let second = ProxySettings::all(
            ProxyConfig::http("http://proxy.test:8080")
                .unwrap()
                .basic_auth("user", &second_password),
        );
        let first = ProxyDispatchRoute::resolve(&uri, None, Some(&first), ProtocolHint::Auto, None)
            .unwrap();
        let second =
            ProxyDispatchRoute::resolve(&uri, None, Some(&second), ProtocolHint::Auto, None)
                .unwrap();
        assert_ne!(first.pool_identity(), second.pool_identity());
    }

    #[test]
    fn route_identity_equality_survives_hash_collisions() {
        #[derive(Default)]
        struct ConstantHasher;

        impl std::hash::Hasher for ConstantHasher {
            fn finish(&self) -> u64 {
                0
            }

            fn write(&mut self, _bytes: &[u8]) {}
        }

        let uri: Uri = "https://example.test/path".parse().unwrap();
        let first_password = test_secret(b'a');
        let second_password = test_secret(b'b');
        let first = ProxySettings::all(
            ProxyConfig::http("http://proxy.test:8080")
                .unwrap()
                .basic_auth("user", &first_password),
        );
        let second = ProxySettings::all(
            ProxyConfig::http("http://proxy.test:8080")
                .unwrap()
                .basic_auth("user", &second_password),
        );
        let first = ProxyDispatchRoute::resolve(&uri, None, Some(&first), ProtocolHint::Auto, None)
            .unwrap()
            .pool_identity();
        let second =
            ProxyDispatchRoute::resolve(&uri, None, Some(&second), ProtocolHint::Auto, None)
                .unwrap()
                .pool_identity();
        let mut routes: std::collections::HashMap<
            ProxyRoute,
            usize,
            std::hash::BuildHasherDefault<ConstantHasher>,
        > = std::collections::HashMap::default();

        routes.insert(first.clone(), 1);
        routes.insert(second.clone(), 2);

        assert_ne!(first, second);
        assert_eq!(routes.len(), 2);
        assert_eq!(routes[&first], 1);
        assert_eq!(routes[&second], 2);
    }

    #[test]
    fn two_hop_chain_resolves_each_missing_credential_once_before_planning() {
        let uri: Uri = "https://example.test/path".parse().unwrap();
        let calls = Arc::new(Mutex::new(Vec::new()));
        let password = test_secret(b'a');
        let settings =
            ProxySettings::default().proxy_credential_resolver(RecordingCredentialResolver {
                calls: calls.clone(),
                username: "resolved",
                password: password.clone(),
            });
        let chain = ProxyChain::new(vec![
            ProxyConfig::http("http://first.test:8080").unwrap(),
            ProxyConfig::socks5("socks5://second.test:1080").unwrap(),
        ]);

        let route = ProxyDispatchRoute::resolve(
            &uri,
            Some(&chain),
            Some(&settings),
            ProtocolHint::Auto,
            None,
        )
        .unwrap();
        let resolved_identity = route.pool_identity();
        let plan = route.establishment_plan().unwrap().unwrap();

        assert_eq!(
            calls.lock().unwrap().as_slice(),
            ["first.test:8080", "second.test:1080"]
        );
        for hop in [plan.first(), plan.second().unwrap()] {
            let auth = hop.proxy().auth.as_ref().unwrap();
            assert_eq!(auth.username, "resolved");
            assert_eq!(auth.password, password);
        }

        let explicit_chain = ProxyChain::new(vec![
            ProxyConfig::http("http://first.test:8080")
                .unwrap()
                .basic_auth("resolved", &password),
            ProxyConfig::socks5("socks5://second.test:1080")
                .unwrap()
                .basic_auth("resolved", &password),
        ]);
        let explicit_route = ProxyDispatchRoute::resolve(
            &uri,
            Some(&explicit_chain),
            None,
            ProtocolHint::Auto,
            None,
        )
        .unwrap();
        assert_eq!(resolved_identity, explicit_route.pool_identity());
        assert_eq!(calls.lock().unwrap().len(), 2);
    }

    #[test]
    fn chain_route_identity_segregates_resolved_credentials_and_preserves_explicit_auth() {
        let uri: Uri = "https://example.test/path".parse().unwrap();
        let explicit_password = test_secret(b'e');
        let first_password = test_secret(b'a');
        let second_password = test_secret(b'b');
        let chain = ProxyChain::new(vec![
            ProxyConfig::http("http://first.test:8080")
                .unwrap()
                .basic_auth("explicit", &explicit_password),
            ProxyConfig::http("http://second.test:8080").unwrap(),
        ]);
        let first_calls = Arc::new(Mutex::new(Vec::new()));
        let second_calls = Arc::new(Mutex::new(Vec::new()));
        let first_settings =
            ProxySettings::default().proxy_credential_resolver(RecordingCredentialResolver {
                calls: first_calls.clone(),
                username: "resolved",
                password: first_password.clone(),
            });
        let second_settings =
            ProxySettings::default().proxy_credential_resolver(RecordingCredentialResolver {
                calls: second_calls.clone(),
                username: "resolved",
                password: second_password.clone(),
            });

        let first = ProxyDispatchRoute::resolve(
            &uri,
            Some(&chain),
            Some(&first_settings),
            ProtocolHint::Auto,
            None,
        )
        .unwrap();
        let second = ProxyDispatchRoute::resolve(
            &uri,
            Some(&chain),
            Some(&second_settings),
            ProtocolHint::Auto,
            None,
        )
        .unwrap();

        assert_ne!(first.pool_identity(), second.pool_identity());
        assert_eq!(first_calls.lock().unwrap().as_slice(), ["second.test:8080"]);
        assert_eq!(
            second_calls.lock().unwrap().as_slice(),
            ["second.test:8080"]
        );

        for (route, expected_password) in [(first, first_password), (second, second_password)] {
            let plan = route.establishment_plan().unwrap().unwrap();
            let explicit = plan.first().proxy().auth.as_ref().unwrap();
            assert_eq!(explicit.username, "explicit");
            assert_eq!(explicit.password, explicit_password);
            let resolved = plan.second().unwrap().proxy().auth.as_ref().unwrap();
            assert_eq!(resolved.username, "resolved");
            assert_eq!(resolved.password, expected_password);
        }
    }

    #[test]
    fn route_resolves_adaptive_h2c_once_from_route_and_cache_state() {
        let uri: Uri = "http://example.test/path".parse().unwrap();
        let proxy = ProxySettings::all(ProxyConfig::http("http://proxy.test:8080").unwrap());

        for (settings, cached, expected) in [
            (None, None, ProtocolHint::AdaptiveH2c),
            (None, Some(true), ProtocolHint::H2c),
            (None, Some(false), ProtocolHint::Auto),
            (Some(&proxy), None, ProtocolHint::Auto),
            (Some(&proxy), Some(true), ProtocolHint::H2c),
        ] {
            let route = ProxyDispatchRoute::resolve(
                &uri,
                None,
                settings,
                ProtocolHint::AdaptiveH2c,
                cached,
            )
            .unwrap();
            assert_eq!(route.protocol_hint(), expected);
        }
    }
}
