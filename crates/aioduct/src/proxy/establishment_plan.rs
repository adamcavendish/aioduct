use std::net::IpAddr;

use http::uri::Authority;

use crate::error::Error;
use crate::pool::ProtocolHint;

use super::dispatch_route::ProxyDestination;
use super::{ProxyConfig, ProxyScheme};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ProxyEndpoint {
    host: String,
    port: u16,
    connect_target: String,
}

impl ProxyEndpoint {
    fn new(authority: Authority, port: u16) -> Self {
        let raw_host = authority.host();
        let host = raw_host
            .strip_prefix('[')
            .and_then(|host| host.strip_suffix(']'))
            .unwrap_or(raw_host)
            .to_owned();
        let connect_target = if host.contains(':') {
            format!("[{host}]:{port}")
        } else {
            format!("{host}:{port}")
        };
        Self {
            host,
            port,
            connect_target,
        }
    }

    fn for_proxy(proxy: &ProxyConfig) -> Result<Self, Error> {
        let authority = proxy.authority()?.clone();
        let port = proxy.effective_port()?;
        Ok(Self::new(authority, port))
    }

    pub(crate) fn host(&self) -> &str {
        &self.host
    }

    pub(crate) fn port(&self) -> u16 {
        self.port
    }

    pub(crate) fn connect_target(&self) -> &str {
        &self.connect_target
    }
}

#[derive(Clone, Debug)]
pub(crate) struct ProxyHopPlan {
    proxy: ProxyConfig,
    endpoint: ProxyEndpoint,
}

impl ProxyHopPlan {
    fn new(proxy: ProxyConfig) -> Result<Self, Error> {
        proxy.validate_for_use()?;
        let endpoint = ProxyEndpoint::for_proxy(&proxy)?;
        Ok(Self { proxy, endpoint })
    }

    pub(crate) fn proxy(&self) -> &ProxyConfig {
        &self.proxy
    }

    pub(crate) fn scheme(&self) -> &ProxyScheme {
        &self.proxy.scheme
    }

    pub(crate) fn endpoint(&self) -> &ProxyEndpoint {
        &self.endpoint
    }
}

#[derive(Clone, Debug)]
pub(crate) struct ProxyEstablishmentPlan {
    hops: Vec<ProxyHopPlan>,
    origin: ProxyEndpoint,
    origin_is_https: bool,
    protocol_hint: ProtocolHint,
}

impl ProxyEstablishmentPlan {
    pub(super) fn new(
        destination: &ProxyDestination,
        proxies: Vec<ProxyConfig>,
        protocol_hint: ProtocolHint,
    ) -> Result<Self, Error> {
        match proxies.len() {
            0 => return Err(Error::Other("empty proxy chain".into())),
            1 | 2 => {}
            count => {
                return Err(Error::Unsupported(format!(
                    "proxy chains longer than 2 hops are not supported (got {count})"
                )));
            }
        }
        if protocol_hint == ProtocolHint::Http3 {
            return Err(Error::Unsupported(
                "HTTP/3 through a proxy requires CONNECT-UDP and is not supported".to_owned(),
            ));
        }
        if protocol_hint == ProtocolHint::AdaptiveH2c {
            return Err(Error::Unsupported(
                "adaptive h2c must be resolved before proxy establishment".to_owned(),
            ));
        }

        let hops = proxies
            .into_iter()
            .map(ProxyHopPlan::new)
            .collect::<Result<Vec<_>, _>>()?;
        let origin = ProxyEndpoint::new(
            destination.authority().clone(),
            destination.effective_port(),
        );
        let origin_is_https = destination.scheme() == &http::uri::Scheme::HTTPS;

        for (index, hop) in hops.iter().enumerate().take(hops.len() - 1) {
            let target = hops[index + 1].endpoint();
            if matches!(hop.scheme(), ProxyScheme::Socks4 | ProxyScheme::Socks4a)
                && matches!(target.host().parse::<IpAddr>(), Ok(IpAddr::V6(_)))
            {
                return Err(Error::Unsupported(format!(
                    "SOCKS4 cannot connect to IPv6 destination {}",
                    target.connect_target()
                )));
            }
        }

        Ok(Self {
            hops,
            origin,
            origin_is_https,
            protocol_hint,
        })
    }

    pub(crate) fn first(&self) -> &ProxyHopPlan {
        &self.hops[0]
    }

    pub(crate) fn second(&self) -> Option<&ProxyHopPlan> {
        self.hops.get(1)
    }

    pub(crate) fn origin(&self) -> &ProxyEndpoint {
        &self.origin
    }

    pub(crate) fn origin_is_https(&self) -> bool {
        self.origin_is_https
    }

    pub(crate) fn protocol_hint(&self) -> ProtocolHint {
        self.protocol_hint
    }

    pub(crate) fn requires_tls(&self) -> bool {
        self.origin_is_https
            || self
                .hops
                .iter()
                .any(|hop| hop.scheme() == &ProxyScheme::Https)
    }

    pub(crate) fn validate_force_addr(
        &self,
        force_addr: Option<std::net::SocketAddr>,
    ) -> Result<(), Error> {
        let final_hop_index = self.hops.len() - 1;
        for (index, hop) in self.hops.iter().enumerate() {
            let target = self
                .hops
                .get(index + 1)
                .map(ProxyHopPlan::endpoint)
                .unwrap_or(&self.origin);
            let target_is_forced = index == final_hop_index && force_addr.is_some();
            if hop.scheme() == &ProxyScheme::Socks5h && !target_is_forced {
                validate_socks5_remote_target(target)?;
            }
        }

        let final_hop = &self.hops[final_hop_index];
        if matches!(
            final_hop.scheme(),
            ProxyScheme::Socks4 | ProxyScheme::Socks4a
        ) {
            match force_addr {
                Some(force_addr) if force_addr.is_ipv4() => {}
                Some(_) => {
                    return Err(Error::Unsupported(
                        "SOCKS4 cannot connect to an IPv6 force_addr".to_owned(),
                    ));
                }
                None if matches!(self.origin.host().parse::<IpAddr>(), Ok(IpAddr::V6(_))) => {
                    return Err(Error::Unsupported(format!(
                        "SOCKS4 cannot connect to IPv6 destination {}",
                        self.origin.connect_target()
                    )));
                }
                None => {}
            }
        }
        Ok(())
    }
}

fn validate_socks5_remote_target(target: &ProxyEndpoint) -> Result<(), Error> {
    if target.host().parse::<IpAddr>().is_err() && target.host().len() > u8::MAX as usize {
        return Err(Error::InvalidUrl(format!(
            "SOCKS5 remote target name exceeds 255 bytes: {}",
            target.connect_target()
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn destination(uri: &str) -> ProxyDestination {
        ProxyDestination::from_uri(&uri.parse().unwrap()).unwrap()
    }

    fn proxy(scheme: &str, name: &str) -> ProxyConfig {
        let uri = format!("{scheme}://{name}.test");
        match scheme {
            "http" => ProxyConfig::http(&uri).unwrap(),
            "https" => ProxyConfig::https(&uri).unwrap(),
            "socks4" | "socks4a" => ProxyConfig::socks4(&uri).unwrap(),
            "socks5" => ProxyConfig::socks5(&uri).unwrap(),
            "socks5h" => ProxyConfig::socks5h(&uri).unwrap(),
            _ => unreachable!(),
        }
    }

    #[test]
    fn materializes_default_ports_and_ipv6_connect_targets() {
        let destination = destination("https://[2001:db8::1]/path");
        let plan = ProxyEstablishmentPlan::new(
            &destination,
            vec![ProxyConfig::https("https://[2001:db8::2]").unwrap()],
            ProtocolHint::Http1,
        )
        .unwrap();

        assert_eq!(
            plan.first().endpoint().connect_target(),
            "[2001:db8::2]:443"
        );
        assert_eq!(plan.origin().connect_target(), "[2001:db8::1]:443");
    }

    #[test]
    fn accepts_every_supported_two_hop_scheme_pair() {
        let destination = destination("http://origin.test/path");
        for first in ["http", "https", "socks4", "socks4a", "socks5", "socks5h"] {
            for second in ["http", "https", "socks4", "socks4a", "socks5", "socks5h"] {
                let plan = ProxyEstablishmentPlan::new(
                    &destination,
                    vec![proxy(first, "first"), proxy(second, "second")],
                    ProtocolHint::Auto,
                )
                .unwrap_or_else(|error| panic!("rejected {first} -> {second}: {error}"));
                assert!(plan.second().is_some());
            }
        }
    }

    #[test]
    fn rejects_invalid_chain_shapes_and_unresolved_protocols() {
        let destination = destination("http://origin.test/path");
        assert!(ProxyEstablishmentPlan::new(&destination, Vec::new(), ProtocolHint::Auto).is_err());
        assert!(
            ProxyEstablishmentPlan::new(
                &destination,
                vec![
                    proxy("http", "one"),
                    proxy("http", "two"),
                    proxy("http", "three"),
                ],
                ProtocolHint::Auto,
            )
            .is_err()
        );
        assert!(
            ProxyEstablishmentPlan::new(
                &destination,
                vec![proxy("http", "one")],
                ProtocolHint::Http3,
            )
            .is_err()
        );
        assert!(
            ProxyEstablishmentPlan::new(
                &destination,
                vec![proxy("http", "one")],
                ProtocolHint::AdaptiveH2c,
            )
            .is_err()
        );
    }

    #[test]
    fn socks4_ipv6_targets_require_an_ipv4_effective_destination() {
        let ipv6_origin = destination("http://[2001:db8::1]/path");
        for scheme in ["socks4", "socks4a"] {
            let plan = ProxyEstablishmentPlan::new(
                &ipv6_origin,
                vec![proxy(scheme, "first")],
                ProtocolHint::Auto,
            )
            .unwrap();
            let error = plan.validate_force_addr(None).unwrap_err();
            assert!(error.to_string().contains("SOCKS4"));
            assert!(error.to_string().contains("IPv6"));
            assert!(
                plan.validate_force_addr(Some("127.0.0.1:80".parse().unwrap()))
                    .is_ok()
            );
            let error = plan
                .validate_force_addr(Some("[::1]:80".parse().unwrap()))
                .unwrap_err();
            assert!(error.to_string().contains("SOCKS4"));
            assert!(error.to_string().contains("force_addr"));
        }

        let origin = destination("http://origin.test/path");
        let error = ProxyEstablishmentPlan::new(
            &origin,
            vec![
                proxy("socks4", "first"),
                ProxyConfig::http("http://[2001:db8::2]").unwrap(),
            ],
            ProtocolHint::Auto,
        )
        .unwrap_err();
        assert!(error.to_string().contains("SOCKS4"));
        assert!(error.to_string().contains("IPv6"));

        let plan = ProxyEstablishmentPlan::new(
            &origin,
            vec![proxy("socks4a", "first")],
            ProtocolHint::Auto,
        )
        .unwrap();
        assert!(
            plan.validate_force_addr(Some("[::1]:80".parse().unwrap()))
                .is_err()
        );
    }

    #[test]
    fn rejects_unencodable_proxy_configuration_during_planning() {
        let destination = destination("https://origin.test/path");

        let non_text_header = ProxyConfig::http("http://proxy.test").unwrap().header(
            http::header::HeaderName::from_static("x-binary"),
            http::HeaderValue::from_bytes(&[0x80]).unwrap(),
        );
        assert!(
            ProxyEstablishmentPlan::new(&destination, vec![non_text_header], ProtocolHint::Auto,)
                .is_err()
        );

        let nul_user = ProxyConfig::socks4("socks4://user%00name@proxy.test").unwrap();
        assert!(
            ProxyEstablishmentPlan::new(&destination, vec![nul_user], ProtocolHint::Auto,).is_err()
        );

        let long_user = "u".repeat(256);
        let long_credentials = ProxyConfig::socks5("socks5://proxy.test")
            .unwrap()
            .basic_auth(&long_user, "password");
        assert!(
            ProxyEstablishmentPlan::new(&destination, vec![long_credentials], ProtocolHint::Auto,)
                .is_err()
        );
    }
}
