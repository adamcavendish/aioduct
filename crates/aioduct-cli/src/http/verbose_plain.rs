use super::observer::CliEvent;
use crate::util::duration_ms;
use crossterm::style::{Color, Stylize};
use std::io::IsTerminal;
use tokio::sync::mpsc;

pub async fn run(mut rx: mpsc::UnboundedReceiver<CliEvent>) {
    let use_color = std::io::stderr().is_terminal();
    while let Some(event) = rx.recv().await {
        let (line, failed) = format_event(&event);
        if use_color {
            eprintln!(
                "{}",
                line.with(if failed { Color::Red } else { Color::DarkGrey })
            );
        } else {
            eprintln!("{line}");
        }
    }
}

fn format_event(event: &CliEvent) -> (String, bool) {
    match event {
        CliEvent::Started { method, uri } => (format!("* Starting {method} {uri}"), false),
        CliEvent::ResponseHeaders { response, elapsed } => (
            format!(
                "* Response headers: {} {} {:?} after {:.1}ms",
                response.status,
                response_version(response.version),
                response.source,
                duration_ms(elapsed)
            ),
            false,
        ),
        CliEvent::Finished {
            reason,
            failed,
            elapsed,
            original_data_bytes,
        } => (
            format!(
                "* Request {reason} after {:.1}ms; original DATA bytes: {original_data_bytes:?}",
                duration_ms(elapsed)
            ),
            *failed,
        ),
        CliEvent::Diagnostic {
            label,
            detail,
            failed,
        } => (format!("* {label}: {detail}"), *failed),
    }
}

fn response_version(version: Option<http::Version>) -> String {
    version
        .map(|version| format!("{version:?}"))
        .unwrap_or_else(|| "unknown".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn failure_retains_diagnostic() {
        let event = CliEvent::Diagnostic {
            label: "ATTEMPT",
            detail: "connection reset".into(),
            failed: true,
        };
        let (line, failed) = format_event(&event);
        assert!(line.contains("connection reset"));
        assert!(failed);
    }
    #[test]
    fn headers_are_not_labelled_complete() {
        let event = CliEvent::ResponseHeaders {
            response: aioduct::ResponseInfo {
                status: http::StatusCode::OK,
                version: Some(http::Version::HTTP_2),
                source: aioduct::ResponseSource::Transport,
            },
            elapsed: std::time::Duration::from_millis(287),
        };
        let (line, _) = format_event(&event);
        assert!(line.contains("200"));
        assert!(line.contains("287.0ms"));
        assert!(!line.contains("complete"));
    }
}
