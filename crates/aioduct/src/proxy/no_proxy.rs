/// Rules for bypassing the proxy for certain hosts.
#[derive(Clone, Debug, Default)]
pub struct NoProxy {
    rules: Vec<String>,
}

impl NoProxy {
    /// Parse a comma-separated list of no-proxy rules.
    ///
    /// Each rule can be:
    /// - A hostname: `example.com`
    /// - A domain suffix: `.example.com` (matches any subdomain)
    /// - A wildcard: `*` (matches everything)
    /// - An IP address: `127.0.0.1`
    /// - A CIDR (stored as-is, matched literally against the host string)
    pub fn new(rules: &str) -> Self {
        let rules: Vec<String> = rules
            .split(',')
            .map(|s| s.trim().to_lowercase())
            .filter(|s| !s.is_empty())
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
        let host = host.to_lowercase();
        for rule in &self.rules {
            if rule == "*" {
                return true;
            }

            let (rule_host, rule_port) = if let Some((h, p)) = rule.rsplit_once(':') {
                if let Ok(port_num) = p.parse::<u16>() {
                    (h, Some(port_num))
                } else {
                    (rule.as_str(), None)
                }
            } else {
                (rule.as_str(), None)
            };

            if rule_port.is_some() && rule_port != port {
                continue;
            }

            if rule_host == host {
                return true;
            }
            if rule_host.starts_with('.') && host.ends_with(rule_host) {
                return true;
            }
            if !rule_host.starts_with('.') && host.ends_with(&format!(".{rule_host}")) {
                return true;
            }
        }
        false
    }
}
