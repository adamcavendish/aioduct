use crate::error::Error;

use super::HttpEngineCore;

impl<B> HttpEngineCore<B> {
    #[allow(dead_code, reason = "used by later request dispatch integration")]
    pub(crate) async fn resolve_authority(
        &self,
        authority: &http::uri::Authority,
        default_port: u16,
    ) -> Result<std::net::SocketAddr, Error> {
        let host = authority.host();
        let port = authority.port_u16().unwrap_or(default_port);
        self.resolve_authority_raw(host, port).await
    }

    #[allow(dead_code, reason = "used by later request dispatch integration")]
    pub(crate) async fn resolve_authority_raw(
        &self,
        host: &str,
        port: u16,
    ) -> Result<std::net::SocketAddr, Error> {
        self.resolve_all_authority_raw(host, port)
            .await
            .map(|addrs| addrs[0])
    }

    /// Resolve a hostname and port to all available socket addresses.
    ///
    /// If `host` is an IP literal, returns it as a single-element `Vec` without
    /// consulting the resolver. Otherwise delegates to the configured
    /// [`Resolve`](crate::Resolve) implementation.
    pub async fn resolve_all_authority_raw(
        &self,
        host: &str,
        port: u16,
    ) -> Result<Vec<std::net::SocketAddr>, Error> {
        if let Some(ip) = parse_ip_literal(host) {
            // IP-literal hosts bypass the address-family filter: an explicit
            // literal is the caller's deliberate choice.
            return Ok(vec![std::net::SocketAddr::new(ip, port)]);
        }

        #[cfg(feature = "tracing")]
        tracing::trace!(host = host, port = port, "dns.resolve.start");

        let result = if let Some(resolver) = &self.resolver {
            resolver
                .resolve_all(host, port)
                .await
                .map_err(|e| Error::InvalidUrl(format!("cannot resolve {host}:{port}: {e}")))
        } else {
            Err(Error::InvalidUrl(format!(
                "no DNS resolver configured for {host}:{port} — use .resolver() on the builder"
            )))
        };

        // Apply the configured address-family preference to resolver output.
        let result = result.and_then(|addrs| {
            let filtered = self.address_family.apply(addrs);
            if filtered.is_empty() {
                Err(Error::InvalidUrl(format!(
                    "no {:?} addresses for {host}:{port}",
                    self.address_family
                )))
            } else {
                Ok(filtered)
            }
        });

        #[cfg(feature = "tracing")]
        match &result {
            Ok(addrs) => tracing::trace!(host = host, count = addrs.len(), "dns.resolve.done"),
            Err(e) => tracing::trace!(host = host, error = %e, "dns.resolve.error"),
        }

        result
    }

    #[cfg(all(feature = "http3", feature = "rustls"))]
    pub(crate) fn cache_alt_svc(&self, uri: &http::Uri, headers: &http::HeaderMap) {
        use http::header::ALT_SVC;
        if let Some(authority) = uri.authority()
            && let Some(alt_svc_value) = headers.get(ALT_SVC)
            && let Ok(value_str) = alt_svc_value.to_str()
        {
            let entries = crate::alt_svc::parse_alt_svc(value_str);
            self.alt_svc_cache.insert(authority.clone(), entries);
        }
    }
}

fn parse_ip_literal(host: &str) -> Option<std::net::IpAddr> {
    host.parse::<std::net::IpAddr>().ok().or_else(|| {
        host.strip_prefix('[')
            .and_then(|host| host.strip_suffix(']'))
            .and_then(|host| host.parse::<std::net::IpAddr>().ok())
    })
}
