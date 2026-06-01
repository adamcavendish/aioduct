use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// Rules for bypassing the proxy for certain hosts.
#[derive(Clone, Debug, Default)]
pub struct NoProxy {
    rules: Vec<NoProxyRule>,
}

#[derive(Clone, Debug)]
enum NoProxyRule {
    Wildcard,
    Host {
        host: String,
        port: Option<u16>,
        include_exact: bool,
    },
    Ip {
        ip: IpAddr,
        port: Option<u16>,
    },
    Cidr {
        network: IpAddr,
        prefix: u8,
        port: Option<u16>,
    },
}

impl NoProxy {
    /// Parse a comma-separated list of no-proxy rules.
    ///
    /// Each rule can be:
    /// - A hostname: `example.com`
    /// - A domain suffix: `.example.com` (matches any subdomain)
    /// - A wildcard: `*` (matches everything)
    /// - An IP address: `127.0.0.1`
    /// - A CIDR: `10.0.0.0/8` or `2001:db8::/32`
    /// - A host with port: `example.com:8080` or `[2001:db8::1]:443`
    pub fn new(rules: &str) -> Self {
        let rules = rules
            .split(',')
            .map(|s| s.trim().to_lowercase())
            .filter_map(|rule| NoProxyRule::parse(&rule))
            .collect();
        Self { rules }
    }

    pub(super) fn from_env() -> Self {
        let val = std::env::var("NO_PROXY")
            .or_else(|_| std::env::var("no_proxy"))
            .unwrap_or_default();
        Self::new(&val)
    }

    /// Returns `true` if the given host (and optional port) matches any bypass rule.
    pub fn matches(&self, host: &str) -> bool {
        self.matches_with_port(host, None)
    }

    /// Returns `true` if the given host:port matches any bypass rule.
    pub(crate) fn matches_with_port(&self, host: &str, port: Option<u16>) -> bool {
        let normalized_host = host.to_lowercase();
        let (host, host_port) = split_host_port(&normalized_host);
        let port = port.or(host_port);
        let host_ip = host.parse::<IpAddr>().ok();

        self.rules
            .iter()
            .any(|rule| rule.matches(host, port, host_ip))
    }
}

impl NoProxyRule {
    fn parse(rule: &str) -> Option<Self> {
        if rule.is_empty() {
            return None;
        }
        if rule == "*" {
            return Some(Self::Wildcard);
        }

        let (host, port) = split_host_port(rule);
        if host.is_empty() {
            return None;
        }
        if let Ok(ip) = host.parse::<IpAddr>() {
            return Some(Self::Ip { ip, port });
        }
        if let Some((network, prefix)) = parse_cidr(host) {
            return Some(Self::Cidr {
                network,
                prefix,
                port,
            });
        }

        let include_exact = !host.starts_with('.');
        let host = host.trim_start_matches('.').to_string();
        if host.is_empty() {
            return None;
        }

        Some(Self::Host {
            host,
            port,
            include_exact,
        })
    }

    fn matches(&self, host: &str, port: Option<u16>, host_ip: Option<IpAddr>) -> bool {
        match self {
            Self::Wildcard => true,
            Self::Host {
                host: rule_host,
                port: rule_port,
                include_exact,
            } => {
                port_matches(*rule_port, port)
                    && ((*include_exact && host == rule_host)
                        || host_is_subdomain_of(host, rule_host))
            }
            Self::Ip {
                ip,
                port: rule_port,
            } => port_matches(*rule_port, port) && host_ip.is_some_and(|host_ip| host_ip == *ip),
            Self::Cidr {
                network,
                prefix,
                port: rule_port,
            } => {
                port_matches(*rule_port, port)
                    && host_ip.is_some_and(|host_ip| ip_in_cidr(host_ip, *network, *prefix))
            }
        }
    }
}

fn port_matches(rule_port: Option<u16>, request_port: Option<u16>) -> bool {
    rule_port.is_none_or(|rule_port| Some(rule_port) == request_port)
}

fn host_is_subdomain_of(host: &str, domain: &str) -> bool {
    host.strip_suffix(domain)
        .is_some_and(|prefix| prefix.ends_with('.'))
}

fn split_host_port(value: &str) -> (&str, Option<u16>) {
    if let Some(rest) = value.strip_prefix('[')
        && let Some((host, tail)) = rest.split_once(']')
    {
        let port = tail.strip_prefix(':').and_then(|p| p.parse::<u16>().ok());
        return (host, port);
    }

    if value.matches(':').count() == 1
        && let Some((host, port)) = value.rsplit_once(':')
        && let Ok(port) = port.parse::<u16>()
    {
        return (host, Some(port));
    }

    (value, None)
}

fn parse_cidr(rule: &str) -> Option<(IpAddr, u8)> {
    let (network, prefix) = rule.split_once('/')?;
    let network = network.parse::<IpAddr>().ok()?;
    let prefix = prefix.parse::<u8>().ok()?;

    match network {
        IpAddr::V4(_) if prefix <= 32 => Some((network, prefix)),
        IpAddr::V6(_) if prefix <= 128 => Some((network, prefix)),
        _ => None,
    }
}

fn ip_in_cidr(ip: IpAddr, network: IpAddr, prefix: u8) -> bool {
    match (ip, network) {
        (IpAddr::V4(ip), IpAddr::V4(network)) => ipv4_in_cidr(ip, network, prefix),
        (IpAddr::V6(ip), IpAddr::V6(network)) => ipv6_in_cidr(ip, network, prefix),
        _ => false,
    }
}

fn ipv4_in_cidr(ip: Ipv4Addr, network: Ipv4Addr, prefix: u8) -> bool {
    let mask = if prefix == 0 {
        0
    } else {
        u32::MAX << (32 - prefix)
    };
    u32::from(ip) & mask == u32::from(network) & mask
}

fn ipv6_in_cidr(ip: Ipv6Addr, network: Ipv6Addr, prefix: u8) -> bool {
    let mask = if prefix == 0 {
        0
    } else {
        u128::MAX << (128 - prefix)
    };
    u128::from(ip) & mask == u128::from(network) & mask
}
