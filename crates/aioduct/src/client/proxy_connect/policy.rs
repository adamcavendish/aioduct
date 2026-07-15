use std::borrow::Cow;
use std::io;
use std::net::{IpAddr, SocketAddr, SocketAddrV4};

use crate::error::Error;
use crate::pool::ProtocolHint;
use crate::proxy::{
    ProxyAuth, ProxyConfig, ProxyEndpoint, ProxyEstablishmentPlan, ProxyHopPlan, ProxyScheme,
};
use crate::socks4::Socks4HandshakeError;
use crate::socks5::{Socks5Dns, Socks5HandshakeError};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::client) struct ProxyTargetAttempt {
    pub(in crate::client) first_target_addr: Option<SocketAddr>,
    pub(in crate::client) second_target_addr: Option<SocketAddr>,
}

pub(super) struct ProxyTargetTraversal<'a> {
    first_target_addrs: &'a [Option<SocketAddr>],
    second_target_addrs: &'a [Option<SocketAddr>],
    first_target_index: usize,
    second_target_index: usize,
    last_error: Option<Error>,
}

impl<'a> ProxyTargetTraversal<'a> {
    pub(super) fn new(
        first_target_addrs: &'a [Option<SocketAddr>],
        second_target_addrs: &'a [Option<SocketAddr>],
    ) -> Self {
        Self {
            first_target_addrs,
            second_target_addrs,
            first_target_index: 0,
            second_target_index: 0,
            last_error: None,
        }
    }

    pub(super) fn record_failure(&mut self, error: ProxyAttemptError) -> Result<(), Error> {
        match error {
            ProxyAttemptError::Fatal(error) | ProxyAttemptError::AdaptiveH2cRejected(error) => {
                Err(error)
            }
            ProxyAttemptError::FirstProxyEndpoint { source, .. } => Err(source),
            ProxyAttemptError::LocalTarget { hop, source } => {
                self.last_error = Some(source);
                if hop == 0 {
                    self.second_target_index = self.second_target_addrs.len();
                }
                Ok(())
            }
        }
    }

    pub(super) fn into_exhausted_error(self) -> Error {
        self.last_error
            .unwrap_or_else(|| Error::Other("proxy target fallback exhausted".into()))
    }
}

impl Iterator for ProxyTargetTraversal<'_> {
    type Item = ProxyTargetAttempt;

    fn next(&mut self) -> Option<Self::Item> {
        while self.first_target_index < self.first_target_addrs.len() {
            if self.second_target_index < self.second_target_addrs.len() {
                let attempt = ProxyTargetAttempt {
                    first_target_addr: self.first_target_addrs[self.first_target_index],
                    second_target_addr: self.second_target_addrs[self.second_target_index],
                };
                self.second_target_index += 1;
                return Some(attempt);
            }
            self.first_target_index += 1;
            self.second_target_index = 0;
        }
        None
    }
}

#[derive(Debug)]
pub(in crate::client) enum ProxyAttemptError {
    Fatal(Error),
    AdaptiveH2cRejected(Error),
    FirstProxyEndpoint {
        remote_addr: SocketAddr,
        source: Error,
    },
    LocalTarget {
        hop: usize,
        source: Error,
    },
}

impl ProxyAttemptError {
    pub(in crate::client) fn local_target(hop: usize, source: io::Error) -> Self {
        Self::local_target_error(hop, Error::Io(source))
    }

    fn local_target_error(hop: usize, source: Error) -> Self {
        Self::LocalTarget { hop, source }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::client) enum ProxyEndpointFailureOwner {
    FirstProxy(SocketAddr),
    LocalTarget(usize),
}

impl ProxyEndpointFailureOwner {
    fn attempt_error(self, source: Error) -> ProxyAttemptError {
        match self {
            Self::FirstProxy(remote_addr) => ProxyAttemptError::FirstProxyEndpoint {
                remote_addr,
                source,
            },
            Self::LocalTarget(hop) => ProxyAttemptError::local_target_error(hop, source),
        }
    }
}

impl From<Error> for ProxyAttemptError {
    fn from(error: Error) -> Self {
        Self::Fatal(error)
    }
}

#[derive(Clone, Copy)]
pub(in crate::client) struct ProxyHopTransition<'a> {
    hop: &'a ProxyHopPlan,
    target: &'a ProxyEndpoint,
    hop_index: usize,
    resolved_addr: Option<SocketAddr>,
}

impl<'a> ProxyHopTransition<'a> {
    pub(in crate::client) fn transport(self) -> ProxyHopTransport<'a> {
        ProxyHopTransport::for_hop(self.hop)
    }

    pub(in crate::client) fn negotiation(self) -> Result<ProxyNegotiation<'a>, ProxyAttemptError> {
        ProxyNegotiation::for_transition(self)
    }

    pub(in crate::client) fn local_target_hop(self) -> Option<usize> {
        self.resolved_addr.map(|_| self.hop_index)
    }

    pub(in crate::client) fn classify_target_setup_error(self, error: Error) -> ProxyAttemptError {
        classify_pre_request_target_error(self.local_target_hop(), error)
    }

    pub(in crate::client) fn classify_adaptive_h2c_setup_error(
        self,
        error: Error,
    ) -> ProxyAttemptError {
        match self.classify_target_setup_error(error) {
            error @ (ProxyAttemptError::FirstProxyEndpoint { .. }
            | ProxyAttemptError::LocalTarget { .. }) => error,
            ProxyAttemptError::Fatal(error) => ProxyAttemptError::AdaptiveH2cRejected(error),
            ProxyAttemptError::AdaptiveH2cRejected(_) => unreachable!(),
        }
    }
}

pub(in crate::client) struct ProxyConnectionTransitions<'a> {
    first: ProxyHopTransition<'a>,
    second: Option<ProxyHopTransition<'a>>,
}

impl<'a> ProxyConnectionTransitions<'a> {
    pub(in crate::client) fn new(
        plan: &'a ProxyEstablishmentPlan,
        first_target_addr: Option<SocketAddr>,
        second_target_addr: Option<SocketAddr>,
    ) -> Self {
        let first_target = plan
            .second()
            .map(ProxyHopPlan::endpoint)
            .unwrap_or_else(|| plan.origin());
        let first = ProxyHopTransition {
            hop: plan.first(),
            target: first_target,
            hop_index: 0,
            resolved_addr: first_target_addr,
        };
        let second = plan.second().map(|hop| ProxyHopTransition {
            hop,
            target: plan.origin(),
            hop_index: 1,
            resolved_addr: second_target_addr,
        });
        Self { first, second }
    }

    pub(in crate::client) fn first(&self) -> ProxyHopTransition<'a> {
        self.first
    }

    pub(in crate::client) fn second(&self) -> Option<ProxyHopTransition<'a>> {
        self.second
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::client) enum ProxyHopTransport<'a> {
    Plain,
    Tls { server_name: &'a str },
}

impl<'a> ProxyHopTransport<'a> {
    pub(in crate::client) fn for_hop(hop: &'a ProxyHopPlan) -> Self {
        if hop.scheme() == &ProxyScheme::Https {
            Self::Tls {
                server_name: hop.endpoint().host(),
            }
        } else {
            Self::Plain
        }
    }
}

pub(in crate::client) enum ProxyNegotiation<'a> {
    HttpConnect {
        proxy: &'a ProxyConfig,
        connect_target: Cow<'a, str>,
    },
    Socks4Local {
        address: SocketAddrV4,
        auth: Option<&'a ProxyAuth>,
        fallback_hop: usize,
    },
    Socks4aRemote {
        host: &'a str,
        port: u16,
        auth: Option<&'a ProxyAuth>,
    },
    Socks5 {
        host: &'a str,
        port: u16,
        auth: Option<&'a ProxyAuth>,
        dns: Socks5Dns,
        resolved_addr: Option<IpAddr>,
        fallback_hop: Option<usize>,
    },
}

impl<'a> ProxyNegotiation<'a> {
    fn for_transition(transition: ProxyHopTransition<'a>) -> Result<Self, ProxyAttemptError> {
        let ProxyHopTransition {
            hop,
            target,
            hop_index,
            resolved_addr,
        } = transition;

        match hop.scheme() {
            ProxyScheme::Http | ProxyScheme::Https => Ok(Self::HttpConnect {
                proxy: hop.proxy(),
                connect_target: resolved_addr.map_or_else(
                    || Cow::Borrowed(target.connect_target()),
                    |addr| Cow::Owned(addr.to_string()),
                ),
            }),
            ProxyScheme::Socks4 => {
                let Some(SocketAddr::V4(address)) = resolved_addr else {
                    return Err(ProxyAttemptError::Fatal(Error::Other(
                        "SOCKS4 target resolution did not produce an IPv4 address".into(),
                    )));
                };
                Ok(Self::Socks4Local {
                    address,
                    auth: hop.proxy().auth.as_ref(),
                    fallback_hop: hop_index,
                })
            }
            ProxyScheme::Socks4a => match resolved_addr {
                Some(SocketAddr::V4(address)) => Ok(Self::Socks4Local {
                    address,
                    auth: hop.proxy().auth.as_ref(),
                    fallback_hop: hop_index,
                }),
                Some(SocketAddr::V6(_)) => Err(ProxyAttemptError::Fatal(Error::Unsupported(
                    "SOCKS4a cannot connect to an IPv6 force_addr".to_owned(),
                ))),
                None => Ok(Self::Socks4aRemote {
                    host: target.host(),
                    port: target.port(),
                    auth: hop.proxy().auth.as_ref(),
                }),
            },
            ProxyScheme::Socks5 | ProxyScheme::Socks5h => {
                let dns = if resolved_addr.is_some() {
                    Socks5Dns::Local
                } else if hop.scheme() == &ProxyScheme::Socks5h {
                    Socks5Dns::Remote
                } else {
                    Socks5Dns::Local
                };
                Ok(Self::Socks5 {
                    host: target.host(),
                    port: resolved_addr.map_or(target.port(), |addr| addr.port()),
                    auth: hop.proxy().auth.as_ref(),
                    dns,
                    resolved_addr: resolved_addr.map(|addr| addr.ip()),
                    fallback_hop: (dns == Socks5Dns::Local).then_some(hop_index),
                })
            }
        }
    }
}

pub(in crate::client) fn classify_socks4_error(
    target_fallback_hop: Option<usize>,
    endpoint_failure_owner: Option<ProxyEndpointFailureOwner>,
    error: Socks4HandshakeError,
) -> ProxyAttemptError {
    if error.is_target_connect_failure() {
        return match target_fallback_hop {
            Some(hop) => ProxyAttemptError::local_target(hop, error.into_io()),
            None => ProxyAttemptError::Fatal(Error::Io(error.into_io())),
        };
    }
    if matches!(&error, Socks4HandshakeError::Io(source) if is_retryable_endpoint_io_kind(source.kind()))
        && let Some(owner) = endpoint_failure_owner
    {
        return owner.attempt_error(Error::Io(error.into_io()));
    }
    match error {
        Socks4HandshakeError::Io(source) => ProxyAttemptError::Fatal(Error::Io(source)),
        fatal => ProxyAttemptError::Fatal(Error::Other(fatal.into_io().into())),
    }
}

pub(in crate::client) fn classify_socks5_error(
    target_fallback_hop: Option<usize>,
    endpoint_failure_owner: Option<ProxyEndpointFailureOwner>,
    error: Socks5HandshakeError,
) -> ProxyAttemptError {
    if error.is_target_connect_failure() {
        return match target_fallback_hop {
            Some(hop) => ProxyAttemptError::local_target(hop, error.into_io()),
            None => ProxyAttemptError::Fatal(Error::Io(error.into_io())),
        };
    }
    if matches!(&error, Socks5HandshakeError::Io(source) if is_retryable_endpoint_io_kind(source.kind()))
        && let Some(owner) = endpoint_failure_owner
    {
        return owner.attempt_error(Error::Io(error.into_io()));
    }
    match error {
        Socks5HandshakeError::Io(source) => ProxyAttemptError::Fatal(Error::Io(source)),
        fatal => ProxyAttemptError::Fatal(Error::Other(fatal.into_io().into())),
    }
}

pub(in crate::client) fn classify_pre_request_target_error(
    fallback_hop: Option<usize>,
    error: Error,
) -> ProxyAttemptError {
    classify_pre_request_endpoint_error(
        fallback_hop.map(ProxyEndpointFailureOwner::LocalTarget),
        error,
    )
}

pub(in crate::client) fn classify_first_proxy_endpoint_error(
    remote_addr: SocketAddr,
    error: Error,
) -> ProxyAttemptError {
    classify_pre_request_endpoint_error(
        Some(ProxyEndpointFailureOwner::FirstProxy(remote_addr)),
        error,
    )
}

pub(in crate::client) fn classify_pre_request_endpoint_error(
    owner: Option<ProxyEndpointFailureOwner>,
    error: Error,
) -> ProxyAttemptError {
    if error_chain_has_retryable_endpoint_io(&error)
        && let Some(owner) = owner
    {
        return owner.attempt_error(error);
    }
    ProxyAttemptError::Fatal(error)
}

fn error_chain_has_retryable_endpoint_io(error: &Error) -> bool {
    let mut source: Option<&(dyn std::error::Error + 'static)> = Some(error);
    while let Some(current) = source {
        if let Some(error) = current.downcast_ref::<io::Error>()
            && is_retryable_endpoint_io_kind(error.kind())
        {
            return true;
        }
        source = current.source();
    }
    false
}

fn is_retryable_endpoint_io_kind(kind: io::ErrorKind) -> bool {
    matches!(
        kind,
        io::ErrorKind::ConnectionRefused
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::ConnectionAborted
            | io::ErrorKind::BrokenPipe
            | io::ErrorKind::NotConnected
            | io::ErrorKind::UnexpectedEof
            | io::ErrorKind::TimedOut
            | io::ErrorKind::WriteZero
            | io::ErrorKind::NetworkUnreachable
            | io::ErrorKind::HostUnreachable
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::client) enum ProxyOriginProtocol<'a> {
    Tls {
        server_name: &'a str,
        protocol_hint: ProtocolHint,
    },
    Http1,
    Http2,
}

impl<'a> ProxyOriginProtocol<'a> {
    pub(in crate::client) fn for_plan(plan: &'a ProxyEstablishmentPlan) -> Result<Self, Error> {
        if plan.origin_is_https() {
            return Ok(Self::Tls {
                server_name: plan.origin().host(),
                protocol_hint: plan.protocol_hint(),
            });
        }

        match plan.protocol_hint() {
            ProtocolHint::Auto | ProtocolHint::Http1 => Ok(Self::Http1),
            ProtocolHint::Http2 | ProtocolHint::H2c => Ok(Self::Http2),
            ProtocolHint::AdaptiveH2c | ProtocolHint::Http3 => Err(Error::Unsupported(format!(
                "protocol {:?} cannot be established through a byte-stream proxy tunnel",
                plan.protocol_hint()
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::proxy::{ProxyChain, ProxyDispatchRoute};

    use super::*;

    fn addr(value: &str) -> SocketAddr {
        value.parse().unwrap()
    }

    fn plan(
        origin: &str,
        proxies: Vec<ProxyConfig>,
        protocol_hint: ProtocolHint,
    ) -> ProxyEstablishmentPlan {
        let origin = origin.parse().unwrap();
        let chain = ProxyChain::new(proxies);
        ProxyDispatchRoute::resolve(&origin, Some(&chain), None, protocol_hint, None)
            .unwrap()
            .establishment_plan()
            .unwrap()
            .unwrap()
    }

    #[test]
    fn target_traversal_applies_hop_scoped_fallback() {
        let first_target_addrs = [Some(addr("192.0.2.1:8080")), Some(addr("192.0.2.2:8080"))];
        let second_target_addrs = [
            Some(addr("198.51.100.1:443")),
            Some(addr("198.51.100.2:443")),
        ];
        let mut traversal = ProxyTargetTraversal::new(&first_target_addrs, &second_target_addrs);

        assert_eq!(
            traversal.next(),
            Some(ProxyTargetAttempt {
                first_target_addr: Some(addr("192.0.2.1:8080")),
                second_target_addr: Some(addr("198.51.100.1:443")),
            })
        );
        traversal
            .record_failure(ProxyAttemptError::local_target(
                1,
                io::Error::other("second hop failed"),
            ))
            .unwrap();
        assert_eq!(
            traversal.next(),
            Some(ProxyTargetAttempt {
                first_target_addr: Some(addr("192.0.2.1:8080")),
                second_target_addr: Some(addr("198.51.100.2:443")),
            })
        );

        traversal
            .record_failure(ProxyAttemptError::local_target(
                0,
                io::Error::other("first hop failed"),
            ))
            .unwrap();
        assert_eq!(
            traversal.next(),
            Some(ProxyTargetAttempt {
                first_target_addr: Some(addr("192.0.2.2:8080")),
                second_target_addr: Some(addr("198.51.100.1:443")),
            })
        );
    }

    #[test]
    fn target_traversal_preserves_fatal_and_last_fallback_errors() {
        let first_target_addrs = [None];
        let second_target_addrs = [None];
        let mut traversal = ProxyTargetTraversal::new(&first_target_addrs, &second_target_addrs);
        assert!(traversal.next().is_some());
        traversal
            .record_failure(ProxyAttemptError::local_target(
                1,
                io::Error::new(io::ErrorKind::ConnectionRefused, "target refused"),
            ))
            .unwrap();
        assert!(traversal.next().is_none());
        let error = traversal.into_exhausted_error();
        assert!(
            matches!(error, Error::Io(error) if error.kind() == io::ErrorKind::ConnectionRefused)
        );

        let mut traversal = ProxyTargetTraversal::new(&first_target_addrs, &second_target_addrs);
        let error = traversal
            .record_failure(ProxyAttemptError::Fatal(Error::Unsupported(
                "fatal".to_owned(),
            )))
            .unwrap_err();
        assert!(matches!(error, Error::Unsupported(message) if message == "fatal"));
    }

    #[test]
    fn connection_transitions_plan_each_hop_without_runtime_io() {
        let plan = plan(
            "https://origin.test/path",
            vec![
                ProxyConfig::http("http://first.test:8080").unwrap(),
                ProxyConfig::socks5h("socks5h://second.test:1080").unwrap(),
            ],
            ProtocolHint::Http2,
        );
        let forced_origin = addr("203.0.113.8:8443");
        let transitions = ProxyConnectionTransitions::new(&plan, None, Some(forced_origin));

        match transitions.first().negotiation().unwrap() {
            ProxyNegotiation::HttpConnect { connect_target, .. } => {
                assert_eq!(connect_target, "second.test:1080");
            }
            _ => panic!("expected first-hop HTTP CONNECT"),
        }
        let second = transitions.second().unwrap();
        assert_eq!(second.transport(), ProxyHopTransport::Plain);
        match second.negotiation().unwrap() {
            ProxyNegotiation::Socks5 {
                host,
                port,
                dns,
                resolved_addr,
                fallback_hop,
                ..
            } => {
                assert_eq!(host, "origin.test");
                assert_eq!(port, forced_origin.port());
                assert_eq!(dns, Socks5Dns::Local);
                assert_eq!(resolved_addr, Some(forced_origin.ip()));
                assert_eq!(fallback_hop, Some(1));
            }
            _ => panic!("expected second-hop SOCKS5 negotiation"),
        }
        assert_eq!(
            ProxyOriginProtocol::for_plan(&plan).unwrap(),
            ProxyOriginProtocol::Tls {
                server_name: "origin.test",
                protocol_hint: ProtocolHint::Http2,
            }
        );
    }

    #[test]
    fn negotiation_policy_keeps_remote_dns_failures_fatal() {
        let local = classify_socks4_error(
            Some(0),
            None,
            Socks4HandshakeError::ConnectRejected {
                code: 0x5B,
                message: "request rejected or failed",
            },
        );
        assert!(matches!(
            local,
            ProxyAttemptError::LocalTarget { hop: 0, .. }
        ));

        let remote = classify_socks5_error(
            None,
            None,
            Socks5HandshakeError::ConnectRejected {
                code: 0x04,
                message: "host unreachable",
            },
        );
        assert!(matches!(remote, ProxyAttemptError::Fatal(Error::Io(_))));
    }

    #[test]
    fn pre_request_fallback_preserves_auth_policy_certificate_and_protocol_failures() {
        let endpoint_io = classify_pre_request_target_error(
            Some(0),
            Error::Tls(Box::new(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "peer closed during TLS",
            ))),
        );
        assert!(matches!(
            endpoint_io,
            ProxyAttemptError::LocalTarget {
                hop: 0,
                source: Error::Tls(_),
            }
        ));

        let certificate = classify_pre_request_target_error(
            Some(0),
            Error::Tls(Box::new(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid peer certificate",
            ))),
        );
        assert!(matches!(
            certificate,
            ProxyAttemptError::Fatal(Error::Tls(_))
        ));

        let policy = classify_pre_request_target_error(
            Some(0),
            Error::Other("CONNECT tunnel failed with status 403".into()),
        );
        assert!(matches!(policy, ProxyAttemptError::Fatal(Error::Other(_))));

        let authentication = classify_socks5_error(
            None,
            Some(ProxyEndpointFailureOwner::LocalTarget(0)),
            Socks5HandshakeError::Authentication(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "authentication failed",
            )),
        );
        assert!(matches!(
            authentication,
            ProxyAttemptError::Fatal(Error::Other(_))
        ));

        let protocol = classify_socks5_error(
            None,
            Some(ProxyEndpointFailureOwner::LocalTarget(0)),
            Socks5HandshakeError::Protocol(io::Error::new(
                io::ErrorKind::InvalidData,
                "unexpected version",
            )),
        );
        assert!(matches!(
            protocol,
            ProxyAttemptError::Fatal(Error::Other(_))
        ));

        let proxy_policy = classify_socks5_error(
            None,
            Some(ProxyEndpointFailureOwner::LocalTarget(0)),
            Socks5HandshakeError::ConnectRejected {
                code: 0x02,
                message: "connection not allowed by ruleset",
            },
        );
        assert!(matches!(
            proxy_policy,
            ProxyAttemptError::Fatal(Error::Other(_))
        ));

        let negotiation_io = classify_socks5_error(
            None,
            Some(ProxyEndpointFailureOwner::LocalTarget(0)),
            Socks5HandshakeError::Io(io::Error::new(
                io::ErrorKind::ConnectionReset,
                "proxy endpoint reset",
            )),
        );
        assert!(matches!(
            negotiation_io,
            ProxyAttemptError::LocalTarget { hop: 0, .. }
        ));

        let first_proxy_io = classify_socks5_error(
            None,
            Some(ProxyEndpointFailureOwner::FirstProxy(addr(
                "192.0.2.10:1080",
            ))),
            Socks5HandshakeError::Io(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "first proxy endpoint closed",
            )),
        );
        assert!(matches!(
            first_proxy_io,
            ProxyAttemptError::FirstProxyEndpoint {
                remote_addr,
                ..
            } if remote_addr == addr("192.0.2.10:1080")
        ));
    }
}
