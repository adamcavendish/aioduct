//! Address-family preference for DNS-resolved connection candidates.

use std::net::SocketAddr;

/// Controls which IP address families are used when a hostname resolves to
/// multiple addresses.
///
/// Applied to resolver results before Happy Eyeballs racing. IP-literal request
/// URLs (e.g. `http://[::1]/`) bypass this filter — an explicit literal is
/// treated as the caller's deliberate choice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum AddressFamily {
    /// Use all resolved addresses (default). Happy Eyeballs interleaves
    /// IPv6 and IPv4 and races them.
    #[default]
    Any,
    /// Use only IPv4 addresses; drop IPv6. If none remain, the connection
    /// fails.
    Ipv4Only,
    /// Use only IPv6 addresses; drop IPv4. If none remain, the connection
    /// fails.
    Ipv6Only,
    /// Use all addresses but try IPv4 first, keeping IPv6 as fallback.
    PreferIpv4,
    /// Use all addresses but try IPv6 first, keeping IPv4 as fallback.
    PreferIpv6,
}

impl AddressFamily {
    /// Apply the family preference to a list of resolved addresses.
    ///
    /// `Only` variants filter out the other family; `Prefer` variants stably
    /// reorder so the preferred family comes first; `Any` is unchanged.
    /// Order within each family is preserved.
    pub(crate) fn apply(self, addrs: Vec<SocketAddr>) -> Vec<SocketAddr> {
        match self {
            AddressFamily::Any => addrs,
            AddressFamily::Ipv4Only => addrs.into_iter().filter(|a| a.is_ipv4()).collect(),
            AddressFamily::Ipv6Only => addrs.into_iter().filter(|a| a.is_ipv6()).collect(),
            AddressFamily::PreferIpv4 => {
                let (v4, v6): (Vec<_>, Vec<_>) = addrs.into_iter().partition(|a| a.is_ipv4());
                v4.into_iter().chain(v6).collect()
            }
            AddressFamily::PreferIpv6 => {
                let (v6, v4): (Vec<_>, Vec<_>) = addrs.into_iter().partition(|a| a.is_ipv6());
                v6.into_iter().chain(v4).collect()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v4(n: u8) -> SocketAddr {
        format!("10.0.0.{n}:80").parse().unwrap()
    }
    fn v6(n: u8) -> SocketAddr {
        format!("[::{n}]:80").parse().unwrap()
    }

    #[test]
    fn any_is_unchanged() {
        let addrs = vec![v6(1), v4(1), v6(2)];
        assert_eq!(AddressFamily::Any.apply(addrs.clone()), addrs);
    }

    #[test]
    fn ipv4_only_drops_ipv6() {
        let out = AddressFamily::Ipv4Only.apply(vec![v6(1), v4(1), v6(2), v4(2)]);
        assert_eq!(out, vec![v4(1), v4(2)]);
    }

    #[test]
    fn ipv6_only_drops_ipv4() {
        let out = AddressFamily::Ipv6Only.apply(vec![v6(1), v4(1), v6(2), v4(2)]);
        assert_eq!(out, vec![v6(1), v6(2)]);
    }

    #[test]
    fn only_can_produce_empty() {
        assert!(AddressFamily::Ipv6Only.apply(vec![v4(1), v4(2)]).is_empty());
        assert!(AddressFamily::Ipv4Only.apply(vec![v6(1)]).is_empty());
    }

    #[test]
    fn prefer_ipv4_reorders_keeping_fallback() {
        let out = AddressFamily::PreferIpv4.apply(vec![v6(1), v4(1), v6(2), v4(2)]);
        assert_eq!(out, vec![v4(1), v4(2), v6(1), v6(2)]);
    }

    #[test]
    fn prefer_ipv6_reorders_keeping_fallback() {
        let out = AddressFamily::PreferIpv6.apply(vec![v4(1), v6(1), v4(2)]);
        assert_eq!(out, vec![v6(1), v4(1), v4(2)]);
    }

    #[test]
    fn prefer_preserves_within_family_order() {
        let out = AddressFamily::PreferIpv4.apply(vec![v4(3), v6(9), v4(1), v4(2)]);
        assert_eq!(out, vec![v4(3), v4(1), v4(2), v6(9)]);
    }

    #[test]
    fn default_is_any() {
        assert_eq!(AddressFamily::default(), AddressFamily::Any);
    }

    // End-to-end ordering: `apply` then the Happy Eyeballs `interleave_addrs`.
    // This guards the real connect ordering, not just the resolver output —
    // `interleave_addrs` leads with the first address's family, so a `Prefer*`
    // preference survives interleaving.
    // End-to-end ordering: `apply` then the Happy Eyeballs `interleave_addrs`.
    // This guards the real connect ordering, not just the resolver output —
    // `interleave_addrs` leads with the first address's family, so a `Prefer*`
    // preference survives interleaving. Gated to non-wasm targets because the
    // `happy_eyeballs` module is native-only.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn prefer_ipv4_survives_interleave() {
        let resolved = vec![v6(1), v4(1), v6(2), v4(2)];
        let ordered = AddressFamily::PreferIpv4.apply(resolved);
        let interleaved = crate::happy_eyeballs::interleave_addrs(&ordered);
        // Assert the full attempted sequence, not just the first slot, so a
        // future interleave change can't preserve only the lead address.
        assert_eq!(interleaved, vec![v4(1), v6(1), v4(2), v6(2)]);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn prefer_ipv6_survives_interleave() {
        let resolved = vec![v4(1), v6(1), v4(2), v6(2)];
        let ordered = AddressFamily::PreferIpv6.apply(resolved);
        let interleaved = crate::happy_eyeballs::interleave_addrs(&ordered);
        assert_eq!(interleaved, vec![v6(1), v4(1), v6(2), v4(2)]);
    }
}
