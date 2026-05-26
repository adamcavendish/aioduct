use std::io::IsTerminal;
use std::time::Duration;

use aioduct::observer::{RequestEvent, RequestPhase, RetryKind};
use crossterm::style::{Color, Stylize};
use tokio::sync::mpsc;

pub async fn run(mut rx: mpsc::UnboundedReceiver<RequestEvent>) {
    let use_color = std::io::stderr().is_terminal();
    while let Some(event) = rx.recv().await {
        let line = format_phase(&event.phase);
        if use_color {
            let colored = colorize(&event.phase, &line);
            eprintln!("{colored}");
        } else {
            eprintln!("{line}");
        }
    }
}

fn format_phase(phase: &RequestPhase) -> String {
    match phase {
        RequestPhase::Started => "* Starting request".into(),
        RequestPhase::PoolCheckoutComplete {
            outcome,
            blocked_duration,
        } => {
            format!("* Pool: {outcome:?} ({:.1}ms)", ms(blocked_duration))
        }
        RequestPhase::DnsResolved { addrs, duration } => {
            let first = addrs.first().map(|a| a.to_string()).unwrap_or_default();
            format!("* DNS: {first} ({:.1}ms)", ms(duration))
        }
        RequestPhase::TcpConnected {
            remote_addr,
            duration,
            protocol,
        } => {
            format!("* TCP: {remote_addr} {protocol:?} ({:.1}ms)", ms(duration))
        }
        RequestPhase::TlsHandshakeComplete {
            duration,
            alpn_protocol,
            ..
        } => {
            let alpn = alpn_protocol.as_deref().unwrap_or("none");
            format!("* TLS: ALPN={alpn} ({:.1}ms)", ms(duration))
        }
        RequestPhase::RequestSent { duration, headers } => {
            let mut s = format!("* Sent: {} headers ({:.1}ms)", headers.len(), ms(duration));
            for (name, value) in headers {
                s.push_str(&format!("\n> {name}: {value}"));
            }
            s
        }
        RequestPhase::ResponseStarted { waiting_duration } => {
            format!("* TTFB: {:.1}ms", ms(waiting_duration))
        }
        RequestPhase::ResponseComplete {
            status,
            protocol,
            total_duration,
        } => {
            format!(
                "* Done: {status} {protocol:?} ({:.1}ms total)",
                ms(total_duration)
            )
        }
        RequestPhase::Failed {
            error,
            retry,
            elapsed,
        } => {
            let retry_str = match retry {
                RetryKind::None => "",
                RetryKind::StaleConnection => " (stale retry)",
                RetryKind::Explicit => " (will retry)",
            };
            format!("* FAIL: {error}{retry_str} ({:.1}ms)", ms(elapsed))
        }
        RequestPhase::BytesTransferred {
            direction,
            chunk_bytes,
            cumulative_bytes,
            ..
        } => {
            format!("* {direction:?}: +{chunk_bytes}B ({cumulative_bytes}B total)")
        }
        RequestPhase::TransferComplete {
            direction,
            total_bytes,
            throughput_bytes_per_sec,
            ..
        } => {
            format!("* {direction:?} complete: {total_bytes}B ({throughput_bytes_per_sec:.0} B/s)")
        }
        RequestPhase::TransferAborted {
            direction,
            bytes_transferred,
            error,
            ..
        } => {
            format!("* {direction:?} aborted after {bytes_transferred}B: {error}")
        }
        RequestPhase::Redirected { status, from, to } => {
            format!("* Redirect: {status} {from} → {to}")
        }
        RequestPhase::Retrying {
            reason,
            attempt,
            max_retries,
            backoff,
        } => {
            format!(
                "* Retry #{attempt}/{max_retries} after {:.0}ms: {reason}",
                ms(backoff)
            )
        }
    }
}

fn colorize(phase: &RequestPhase, text: &str) -> String {
    let color = match phase {
        RequestPhase::Failed { .. } | RequestPhase::TransferAborted { .. } => Color::Red,
        RequestPhase::ResponseComplete { status, .. } => {
            if status.is_success() {
                Color::Green
            } else if status.is_redirection() {
                Color::Yellow
            } else {
                Color::Red
            }
        }
        RequestPhase::TlsHandshakeComplete { .. } => Color::Cyan,
        RequestPhase::ResponseStarted { .. } => Color::Green,
        _ => Color::DarkGrey,
    };
    format!("{}", text.with(color))
}

fn ms(d: &Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};

    use aioduct::observer::NegotiatedProtocol;
    use http::StatusCode;

    #[test]
    fn format_dns_resolved() {
        let phase = RequestPhase::DnsResolved {
            addrs: vec![SocketAddr::new(
                IpAddr::V4(Ipv4Addr::new(93, 184, 216, 34)),
                443,
            )],
            duration: Duration::from_millis(12),
        };
        let s = format_phase(&phase);
        assert!(s.contains("93.184.216.34:443"));
        assert!(s.contains("12.0ms"));
    }

    #[test]
    fn format_response_complete_success() {
        let phase = RequestPhase::ResponseComplete {
            status: StatusCode::OK,
            protocol: NegotiatedProtocol::Http2,
            total_duration: Duration::from_millis(287),
        };
        let s = format_phase(&phase);
        assert!(s.contains("200"));
        assert!(s.contains("287.0ms"));
    }

    #[test]
    fn format_failed() {
        let phase = RequestPhase::Failed {
            error: "connection reset".into(),
            retry: RetryKind::StaleConnection,
            elapsed: Duration::from_millis(50),
        };
        let s = format_phase(&phase);
        assert!(s.contains("connection reset"));
        assert!(s.contains("stale retry"));
    }

    #[test]
    fn colorize_success_is_green() {
        let phase = RequestPhase::ResponseComplete {
            status: StatusCode::OK,
            protocol: NegotiatedProtocol::Http2,
            total_duration: Duration::from_millis(100),
        };
        let colored = colorize(&phase, "test");
        assert!(colored.contains("test"));
    }
}
