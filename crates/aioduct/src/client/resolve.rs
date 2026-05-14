use crate::error::Error;

use super::HttpEngineCore;

impl HttpEngineCore {
    pub(super) async fn resolve_authority(
        &self,
        authority: &http::uri::Authority,
        default_port: u16,
    ) -> Result<std::net::SocketAddr, Error> {
        let host = authority.host();
        let port = authority.port_u16().unwrap_or(default_port);
        self.resolve_authority_raw(host, port).await
    }

    async fn resolve_authority_raw(
        &self,
        host: &str,
        port: u16,
    ) -> Result<std::net::SocketAddr, Error> {
        self.resolve_all_authority_raw(host, port)
            .await
            .map(|addrs| addrs[0])
    }

    pub(super) async fn resolve_all_authority_raw(
        &self,
        host: &str,
        port: u16,
    ) -> Result<Vec<std::net::SocketAddr>, Error> {
        if let Ok(addr) = format!("{host}:{port}").parse::<std::net::SocketAddr>() {
            return Ok(vec![addr]);
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

        #[cfg(feature = "tracing")]
        match &result {
            Ok(addrs) => tracing::trace!(host = host, count = addrs.len(), "dns.resolve.done"),
            Err(e) => tracing::trace!(host = host, error = %e, "dns.resolve.error"),
        }

        result
    }

    #[cfg(all(feature = "http3", feature = "rustls"))]
    pub(super) fn cache_alt_svc(&self, uri: &http::Uri, headers: &http::HeaderMap) {
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
