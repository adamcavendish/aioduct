use std::time::Duration;

/// Per-request timing breakdown.
///
/// Captures the duration of each connection phase (DNS, TCP, TLS, transfer).
/// Phases that were skipped (e.g. no DNS for literal IPs, no TLS for HTTP,
/// pool hit that skips all connection phases) are `None`.
#[derive(Debug, Clone)]
pub struct RequestTimings {
    pub(crate) dns: Option<Duration>,
    pub(crate) tcp_connect: Option<Duration>,
    pub(crate) tls_handshake: Option<Duration>,
    pub(crate) transfer: Option<Duration>,
    pub(crate) total: Duration,
}

impl RequestTimings {
    /// Time spent resolving the hostname. `None` if the address was a literal
    /// IP, the connection came from the pool, or DNS was handled by a proxy.
    pub fn dns(&self) -> Option<Duration> {
        self.dns
    }

    /// Time spent establishing the TCP connection. `None` for pool hits.
    pub fn tcp_connect(&self) -> Option<Duration> {
        self.tcp_connect
    }

    /// Time spent on the TLS handshake. `None` for plain HTTP or pool hits.
    pub fn tls_handshake(&self) -> Option<Duration> {
        self.tls_handshake
    }

    /// Time from sending the request to receiving the first byte of the
    /// response headers (TTFB). `None` should not normally occur.
    pub fn transfer(&self) -> Option<Duration> {
        self.transfer
    }

    /// Wall-clock time from the start of the request to receiving response
    /// headers.
    pub fn total(&self) -> Duration {
        self.total
    }
}

#[derive(Debug, Clone, Default)]
pub(crate) struct TimingCollector {
    pub(crate) dns: Option<Duration>,
    pub(crate) tcp_connect: Option<Duration>,
    pub(crate) tls_handshake: Option<Duration>,
}

impl TimingCollector {
    pub(crate) fn into_timings(
        self,
        transfer: Option<Duration>,
        total: Duration,
    ) -> RequestTimings {
        RequestTimings {
            dns: self.dns,
            tcp_connect: self.tcp_connect,
            tls_handshake: self.tls_handshake,
            transfer,
            total,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_timings() {
        let t = RequestTimings {
            dns: Some(Duration::from_millis(10)),
            tcp_connect: Some(Duration::from_millis(20)),
            tls_handshake: Some(Duration::from_millis(30)),
            transfer: Some(Duration::from_millis(40)),
            total: Duration::from_millis(100),
        };
        assert_eq!(t.dns(), Some(Duration::from_millis(10)));
        assert_eq!(t.tcp_connect(), Some(Duration::from_millis(20)));
        assert_eq!(t.tls_handshake(), Some(Duration::from_millis(30)));
        assert_eq!(t.transfer(), Some(Duration::from_millis(40)));
        assert_eq!(t.total(), Duration::from_millis(100));
    }

    #[test]
    fn pool_hit_timings() {
        let t = RequestTimings {
            dns: None,
            tcp_connect: None,
            tls_handshake: None,
            transfer: Some(Duration::from_millis(15)),
            total: Duration::from_millis(15),
        };
        assert!(t.dns().is_none());
        assert!(t.tcp_connect().is_none());
        assert!(t.tls_handshake().is_none());
        assert_eq!(t.transfer(), Some(Duration::from_millis(15)));
    }

    #[test]
    fn http_no_tls_timings() {
        let t = RequestTimings {
            dns: Some(Duration::from_millis(5)),
            tcp_connect: Some(Duration::from_millis(10)),
            tls_handshake: None,
            transfer: Some(Duration::from_millis(20)),
            total: Duration::from_millis(35),
        };
        assert!(t.tls_handshake().is_none());
        assert!(t.dns().is_some());
    }

    #[test]
    fn debug_format() {
        let t = RequestTimings {
            dns: Some(Duration::from_millis(1)),
            tcp_connect: None,
            tls_handshake: None,
            transfer: None,
            total: Duration::from_millis(1),
        };
        let dbg = format!("{t:?}");
        assert!(dbg.contains("RequestTimings"));
    }

    #[test]
    fn collector_into_timings() {
        let c = TimingCollector {
            dns: Some(Duration::from_millis(5)),
            tcp_connect: Some(Duration::from_millis(10)),
            tls_handshake: Some(Duration::from_millis(15)),
        };
        let t = c.into_timings(Some(Duration::from_millis(20)), Duration::from_millis(50));
        assert_eq!(t.dns(), Some(Duration::from_millis(5)));
        assert_eq!(t.total(), Duration::from_millis(50));
    }

    #[test]
    fn collector_default() {
        let c = TimingCollector::default();
        assert!(c.dns.is_none());
        assert!(c.tcp_connect.is_none());
        assert!(c.tls_handshake.is_none());
    }
}
