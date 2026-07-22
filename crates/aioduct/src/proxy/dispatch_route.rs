use http::Uri;
use http::uri::{Authority, Scheme};

use crate::error::Error;
use crate::pool::{ProtocolHint, ProxyRoute};

use super::{ProxyChain, ProxyConfig, ProxySettings};

#[derive(Clone)]
pub(crate) struct ProxyDestination {
    scheme: Scheme,
    authority: Authority,
    effective_port: u16,
}

impl ProxyDestination {
    fn from_uri(uri: &Uri) -> Result<Self, Error> {
        let scheme = uri
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
        let (scheme, default_port) = match scheme.as_str() {
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
            ProxySelection::Chain(chain.clone())
        } else if let Some(proxy) = settings.and_then(|settings| settings.proxy_for(uri)) {
            ProxySelection::Single(proxy)
        } else {
            ProxySelection::Direct
        };
        let pool_identity = match &selection {
            ProxySelection::Direct => ProxyRoute::DIRECT,
            ProxySelection::Single(proxy) => ProxyRoute::from_hash(proxy.route_hash()),
            ProxySelection::Chain(chain) => ProxyRoute::from_hash(chain.route_hash()),
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

    pub(crate) fn single_proxy(&self) -> Option<&ProxyConfig> {
        match &self.selection {
            ProxySelection::Single(proxy) => Some(proxy),
            ProxySelection::Direct | ProxySelection::Chain(_) => None,
        }
    }

    pub(crate) fn chain(&self) -> Option<&ProxyChain> {
        match &self.selection {
            ProxySelection::Chain(chain) => Some(chain),
            ProxySelection::Direct | ProxySelection::Single(_) => None,
        }
    }

    pub(crate) fn pool_identity(&self) -> ProxyRoute {
        self.pool_identity
    }

    pub(crate) fn protocol_hint(&self) -> ProtocolHint {
        self.protocol_hint
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn route_owns_direct_single_and_chain_selections() {
        let uri: Uri = "https://example.test/path".parse().unwrap();
        let direct =
            ProxyDispatchRoute::resolve(&uri, None, None, ProtocolHint::Auto, None).unwrap();
        assert!(!direct.is_proxied());
        assert!(direct.single_proxy().is_none());
        assert!(direct.chain().is_none());
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
        assert_eq!(
            single.single_proxy().map(ProxyConfig::route_hash),
            Some(first.route_hash())
        );
        assert!(single.chain().is_none());
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
        assert!(chained.single_proxy().is_none());
        assert_eq!(
            chained.chain().map(ProxyChain::route_hash),
            Some(chain.route_hash())
        );
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
