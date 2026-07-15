use std::future::Future;
use std::io;
use std::net::SocketAddr;
#[cfg(feature = "rustls")]
use std::sync::Arc;
use std::time::Duration;

use http::{Method, Uri};

use crate::clock::Instant;
use crate::error::Error;
#[cfg(feature = "rustls")]
use crate::observer::RequestObserver;
use crate::observer::{NegotiatedProtocol, RequestEvent, RequestPhase};
use crate::pool::ProtocolHint;
use crate::proxy::{ProxyEndpoint, ProxyEstablishmentPlan, ProxyScheme};
use crate::runtime::SocketConfig;

use super::HttpEngineCore;

mod policy;

use policy::ProxyTargetTraversal;
pub(super) use policy::{
    ProxyAttemptError, ProxyConnectionTransitions, ProxyEndpointFailureOwner, ProxyHopTransition,
    ProxyHopTransport, ProxyNegotiation, ProxyOriginProtocol, ProxyTargetAttempt,
    classify_first_proxy_endpoint_error, classify_pre_request_endpoint_error,
    classify_socks4_error, classify_socks5_error,
};

pub(super) struct ProxyConnectCandidates {
    pub(super) first_proxy_addrs: Vec<SocketAddr>,
    pub(super) first_target_addrs: Vec<Option<SocketAddr>>,
    pub(super) second_target_addrs: Vec<Option<SocketAddr>>,
}

impl ProxyConnectCandidates {
    pub(super) async fn resolve<B: 'static>(
        core: &HttpEngineCore<B>,
        plan: &ProxyEstablishmentPlan,
        method: &Method,
        uri: &Uri,
        force_addr: Option<SocketAddr>,
    ) -> Result<Self, Error> {
        plan.validate_force_addr(force_addr)?;
        let first_proxy_addrs =
            resolve_endpoint(core, plan.first().endpoint(), method, uri).await?;
        let (first_target, first_target_force_addr) = match plan.second() {
            Some(hop) => (hop.endpoint(), None),
            None => (plan.origin(), force_addr),
        };
        let first_target_addrs = resolve_target(
            core,
            plan.first().scheme(),
            first_target,
            first_target_force_addr,
            method,
            uri,
        )
        .await?;
        let second_target_addrs = if let Some(second) = plan.second() {
            resolve_target(
                core,
                second.scheme(),
                plan.origin(),
                force_addr,
                method,
                uri,
            )
            .await?
        } else {
            vec![None]
        };

        Ok(Self {
            first_proxy_addrs,
            first_target_addrs,
            second_target_addrs,
        })
    }

    pub(super) async fn try_each_target<T, F, Fut>(&self, mut attempt: F) -> Result<T, Error>
    where
        F: FnMut(Vec<SocketAddr>, ProxyTargetAttempt) -> Fut,
        Fut: Future<Output = Result<T, ProxyAttemptError>>,
    {
        let mut traversal =
            ProxyTargetTraversal::new(&self.first_target_addrs, &self.second_target_addrs);
        let mut remaining_first_proxy_addrs = self.first_proxy_addrs.clone();
        while let Some(targets) = traversal.next() {
            loop {
                match attempt(remaining_first_proxy_addrs.clone(), targets).await {
                    Ok(connection) => return Ok(connection),
                    Err(ProxyAttemptError::FirstProxyEndpoint {
                        remote_addr,
                        source,
                    }) => {
                        let previous_len = remaining_first_proxy_addrs.len();
                        remaining_first_proxy_addrs.retain(|candidate| *candidate != remote_addr);
                        if remaining_first_proxy_addrs.is_empty()
                            || remaining_first_proxy_addrs.len() == previous_len
                        {
                            return Err(source);
                        }
                    }
                    Err(error) => {
                        traversal.record_failure(error)?;
                        break;
                    }
                }
            }
        }

        Err(traversal.into_exhausted_error())
    }
}

async fn resolve_endpoint<B: 'static>(
    core: &HttpEngineCore<B>,
    endpoint: &ProxyEndpoint,
    method: &Method,
    uri: &Uri,
) -> Result<Vec<SocketAddr>, Error> {
    let started = Instant::now();
    let addrs = core
        .resolve_all_authority_raw(endpoint.host(), endpoint.port())
        .await?;
    core.notify(
        method,
        uri,
        RequestPhase::DnsResolved {
            addrs: addrs.clone(),
            duration: started.elapsed(),
        },
    );
    Ok(addrs)
}

async fn resolve_target<B: 'static>(
    core: &HttpEngineCore<B>,
    scheme: &ProxyScheme,
    target: &ProxyEndpoint,
    force_addr: Option<SocketAddr>,
    method: &Method,
    uri: &Uri,
) -> Result<Vec<Option<SocketAddr>>, Error> {
    if let Some(addr) = force_addr {
        if matches!(scheme, ProxyScheme::Socks4 | ProxyScheme::Socks4a) && !addr.is_ipv4() {
            return Err(Error::Unsupported(
                "SOCKS4 cannot connect to an IPv6 force_addr".to_owned(),
            ));
        }
        return Ok(vec![Some(addr)]);
    }

    if !matches!(scheme, ProxyScheme::Socks4 | ProxyScheme::Socks5) {
        return Ok(vec![None]);
    }

    let addrs = resolve_endpoint(core, target, method, uri).await?;
    let candidates = addrs
        .into_iter()
        .filter_map(|addr| match scheme {
            ProxyScheme::Socks4 => addr.is_ipv4().then_some(Some(addr)),
            ProxyScheme::Socks5 => Some(Some(addr)),
            _ => None,
        })
        .collect::<Vec<_>>();

    if candidates.is_empty() {
        let message = match scheme {
            ProxyScheme::Socks4 => format!(
                "SOCKS4 requires a locally resolved IPv4 destination for {}",
                target.connect_target()
            ),
            ProxyScheme::Socks5 => format!(
                "SOCKS5 resolution returned no usable destination addresses for {}",
                target.connect_target()
            ),
            _ => "proxy target resolution returned no usable addresses".to_owned(),
        };
        return Err(Error::Io(io::Error::new(
            io::ErrorKind::AddrNotAvailable,
            message,
        )));
    }

    Ok(candidates)
}

pub(super) fn configure_proxy_socket<B, S>(
    core: &HttpEngineCore<B>,
    stream: &S,
) -> Result<(), Error>
where
    S: SocketConfig,
{
    #[cfg(target_os = "linux")]
    if let Some(ref interface) = core.interface {
        stream.bind_device(interface).map_err(Error::Io)?;
    }
    if let Some(time) = core.tcp_keepalive {
        stream
            .set_keepalive(
                time,
                core.tcp_keepalive_interval,
                core.tcp_keepalive_retries,
            )
            .map_err(Error::Io)?;
    }
    if core.tcp_fast_open {
        let _ = stream.set_fast_open();
    }
    Ok(())
}

pub(super) struct ProxyAttemptObservation {
    #[cfg(feature = "rustls")]
    observer: Option<Arc<dyn RequestObserver>>,
    #[cfg(feature = "rustls")]
    method: Method,
    #[cfg(feature = "rustls")]
    uri: Uri,
}

impl ProxyAttemptObservation {
    pub(super) fn new<B: 'static>(
        core: &HttpEngineCore<B>,
        method: &Method,
        uri: &Uri,
        first_proxy_scheme: &ProxyScheme,
        protocol_hint: ProtocolHint,
        remote_addr: SocketAddr,
        tcp_duration: Duration,
    ) -> Self {
        let observer = core.observer.clone();
        if let Some(observer) = observer.as_ref() {
            observer.on_event(&RequestEvent {
                method: method.clone(),
                uri: uri.clone(),
                phase: RequestPhase::TcpConnected {
                    remote_addr,
                    duration: tcp_duration,
                    protocol: protocol_for_proxy_tcp(first_proxy_scheme, protocol_hint),
                },
                at: crate::observer::Instant::now(),
            });
        }
        Self {
            #[cfg(feature = "rustls")]
            observer,
            #[cfg(feature = "rustls")]
            method: method.clone(),
            #[cfg(feature = "rustls")]
            uri: uri.clone(),
        }
    }

    #[cfg(feature = "rustls")]
    pub(super) fn record_proxy_tls<S>(
        &self,
        stream: &crate::tls::TlsStream<S>,
        duration: Duration,
    ) {
        let connection = stream.tls_connection();
        let Some(observer) = self.observer.as_ref() else {
            return;
        };
        observer.on_event(&RequestEvent {
            method: self.method.clone(),
            uri: self.uri.clone(),
            phase: RequestPhase::TlsHandshakeComplete {
                duration,
                alpn_protocol: connection
                    .alpn_protocol()
                    .map(|protocol| String::from_utf8_lossy(protocol).into_owned()),
                peer_certificate_der: connection
                    .peer_certificates()
                    .and_then(|certificates| certificates.first())
                    .map(|certificate| certificate.as_ref().to_vec()),
            },
            at: crate::observer::Instant::now(),
        });
    }
}

fn protocol_for_proxy_tcp(
    first_proxy_scheme: &ProxyScheme,
    origin_protocol_hint: ProtocolHint,
) -> NegotiatedProtocol {
    match first_proxy_scheme {
        ProxyScheme::Http | ProxyScheme::Https => NegotiatedProtocol::Http1,
        ProxyScheme::Socks4 | ProxyScheme::Socks4a | ProxyScheme::Socks5 | ProxyScheme::Socks5h => {
            protocol_for_hint(origin_protocol_hint)
        }
    }
}

fn protocol_for_hint(protocol_hint: ProtocolHint) -> NegotiatedProtocol {
    match protocol_hint {
        ProtocolHint::Http2 | ProtocolHint::H2c => NegotiatedProtocol::Http2,
        ProtocolHint::Http3 => NegotiatedProtocol::Http3,
        ProtocolHint::Auto | ProtocolHint::Http1 | ProtocolHint::AdaptiveH2c => {
            NegotiatedProtocol::Http1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn http_proxy_tcp_events_report_the_first_hop_protocol() {
        for scheme in [ProxyScheme::Http, ProxyScheme::Https] {
            for hint in [
                ProtocolHint::Auto,
                ProtocolHint::Http1,
                ProtocolHint::Http2,
                ProtocolHint::H2c,
                ProtocolHint::AdaptiveH2c,
            ] {
                assert_eq!(
                    protocol_for_proxy_tcp(&scheme, hint),
                    NegotiatedProtocol::Http1
                );
            }
        }
    }

    #[test]
    fn socks_tcp_events_retain_the_best_available_origin_hint() {
        for scheme in [
            ProxyScheme::Socks4,
            ProxyScheme::Socks4a,
            ProxyScheme::Socks5,
            ProxyScheme::Socks5h,
        ] {
            for (hint, expected) in [
                (ProtocolHint::Auto, NegotiatedProtocol::Http1),
                (ProtocolHint::Http1, NegotiatedProtocol::Http1),
                (ProtocolHint::Http2, NegotiatedProtocol::Http2),
                (ProtocolHint::H2c, NegotiatedProtocol::Http2),
                (ProtocolHint::AdaptiveH2c, NegotiatedProtocol::Http1),
                (ProtocolHint::Http3, NegotiatedProtocol::Http3),
            ] {
                assert_eq!(protocol_for_proxy_tcp(&scheme, hint), expected);
            }
        }
    }
}
