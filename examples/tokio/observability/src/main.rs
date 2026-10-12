//! A backend-neutral reference for every native Tokio observability callback.
//!
//! The server is local and intentionally deterministic, so this example does
//! not depend on an external service.  The observer renders the public event
//! values as compact semantic lines; a real application can replace those
//! `println!` calls with tracing, OpenTelemetry, metrics, or more than one
//! backend.

use std::convert::Infallible;
use std::io::IsTerminal;
use std::net::SocketAddr;
use std::sync::{
    Arc, OnceLock,
    atomic::{AtomicU64, AtomicUsize, Ordering},
};
use std::time::Duration;

use aioduct::{
    AttemptEvent, BackoffState, ChannelDetachReason, ChannelHandoffInfo, ChannelHandoffMode,
    ChannelIoEvent, ChannelIoOperation, CleanupState, ConnectionAcquisitionEvent,
    ConnectionAcquisitionPhase, ConnectionAcquisitionResource, ConnectionEstablishmentPhase,
    ConnectionObservation, ConnectionObservationEvent, Interest, PhaseSkipReason, PoolLookupResult,
    RateLimitState, RequestBodyEvent, RequestFinish, RequestFinishReason, RequestInfo,
    RequestObservation, RequestStart, ResponseBodyEvent, ResponseChannelHandoffEvent,
    ResponseDisposition, ResponseHeaders, ResponseInfo, ResponseSource, ResponseSseEvent,
    RetryConfig, RetryReason, SseStatisticsSnapshot, TokioClient, TransportKind,
};
use bytes::Bytes;
use futures_util::stream;
use http::{Method, StatusCode, header};
use http_body_util::{BodyExt as _, Full, StreamBody};
use hyper::body::{Frame, Incoming};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response};
use nu_ansi_term::{Color, Style};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

type ServerBody = http_body_util::combinators::BoxBody<Bytes, std::io::Error>;

/// One observer can feed any number of application-owned backends.  This
/// example deliberately has no dependency on tracing or OTLP.
struct CompleteObserver {
    next_request_id: AtomicU64,
    next_connection_id: AtomicU64,
    theme: Theme,
}

struct RequestLog {
    id: u64,
    theme: Theme,
}

struct ConnectionLog {
    id: u64,
    theme: Theme,
}

#[derive(Clone, Copy)]
struct Theme {
    color: bool,
    trace: bool,
}

#[derive(Clone, Copy)]
enum Tone {
    Accent,
    Good,
    Warn,
    Bad,
    Muted,
}

static OUTPUT_THEME: OnceLock<Theme> = OnceLock::new();

impl CompleteObserver {
    fn new(trace: bool) -> Self {
        let theme = Theme {
            color: std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none(),
            trace,
        };
        let _ = OUTPUT_THEME.set(theme);
        Self {
            next_request_id: AtomicU64::new(1_000),
            next_connection_id: AtomicU64::new(1),
            theme,
        }
    }
}

impl Theme {
    fn paint(self, tone: Tone, text: impl std::fmt::Display) -> String {
        let text = text.to_string();
        if !self.color {
            return text;
        }
        let style = match tone {
            Tone::Accent => Style::new().fg(Color::Cyan).bold(),
            Tone::Good => Style::new().fg(Color::Green),
            Tone::Warn => Style::new().fg(Color::Yellow),
            Tone::Bad => Style::new().fg(Color::Red),
            Tone::Muted => Style::new().dimmed(),
        };
        style.paint(text).to_string()
    }

    fn symbol(self, tone: Tone) -> String {
        let symbol = match tone {
            Tone::Accent => "•",
            Tone::Good => "✓",
            Tone::Warn => "!",
            Tone::Bad => "✗",
            Tone::Muted => "·",
        };
        self.paint(tone, symbol)
    }
}

fn all_interest() -> Interest {
    Interest::REQUEST_START
        | Interest::RESPONSE_HEADERS
        | Interest::REQUEST_FINISH
        | Interest::ATTEMPT
        | Interest::CONNECTION_ACQUISITION
        | Interest::REQUEST_BODY
        | Interest::RESPONSE_BODY
        | Interest::RESPONSE_BODY_BYTES
        | Interest::SSE_LIFECYCLE
        | Interest::RESPONSE_CHANNEL_HANDOFF
        | Interest::CHANNEL_LIFECYCLE
        | Interest::SSE_STATISTICS
        | Interest::CHANNEL_BYTES
        | Interest::CONNECTION_LIFECYCLE
        | Interest::CONNECTION_ESTABLISHMENT
}

impl aioduct::Observer for CompleteObserver {
    type Context = u64;
    type Observation = RequestLog;
    type ConnectionObservation = ConnectionLog;

    fn begin(
        &self,
        _request: RequestInfo<'_>,
        context: Option<Self::Context>,
    ) -> (Interest, Self::Observation) {
        let id = context.unwrap_or_else(|| self.next_request_id.fetch_add(1, Ordering::Relaxed));
        (
            all_interest(),
            RequestLog {
                id,
                theme: self.theme,
            },
        )
    }

    fn begin_connection(&self) -> (Interest, Self::ConnectionObservation) {
        let id = self.next_connection_id.fetch_add(1, Ordering::Relaxed);
        (
            Interest::CONNECTION_LIFECYCLE | Interest::CONNECTION_ESTABLISHMENT,
            ConnectionLog {
                id,
                theme: self.theme,
            },
        )
    }
}

impl RequestObservation for RequestLog {
    fn on_request_start(&self, event: &RequestStart<'_>) {
        let detail = if self.theme.trace {
            format!("start {} {}", event.method, compact_uri(event.initial_uri))
        } else {
            format!("{} {}", event.method, compact_uri(event.initial_uri))
        };
        request_line(self.theme, self.id, Tone::Accent, detail);
    }

    fn on_response_headers(&self, event: &ResponseHeaders) {
        request_line(
            self.theme,
            self.id,
            if event.response.status.is_success() {
                Tone::Good
            } else {
                Tone::Warn
            },
            if self.theme.trace {
                format!("headers  {}", response_text(event.response))
            } else {
                format!("response  {}", response_text(event.response))
            },
        );
    }

    fn on_request_finish(&self, event: &RequestFinish<'_>) {
        let mut detail = format!(
            "{} · attempts={}",
            finish_reason(event.reason),
            event.attempt_count
        );
        if let Some(response) = event.response {
            detail.push_str(&format!(" · response={}", response_summary_text(response)));
        }
        if let Some(bytes) = event.original_data_bytes {
            detail.push_str(&format!(" · data={bytes} B"));
        }
        request_line(
            self.theme,
            self.id,
            match event.reason {
                RequestFinishReason::Ended | RequestFinishReason::HandedOff => Tone::Good,
                RequestFinishReason::ConsumerAbandoned
                | RequestFinishReason::ExecutionAbandoned => Tone::Warn,
                RequestFinishReason::Failed { .. } => Tone::Bad,
                _ => Tone::Muted,
            },
            if self.theme.trace {
                format!("finish   {detail}")
            } else {
                format!("finished {detail}")
            },
        );
    }

    fn on_attempt(&self, event: &AttemptEvent<'_>) {
        if !self.theme.trace
            && matches!(
                event,
                AttemptEvent::RateLimit { .. }
                    | AttemptEvent::Backoff {
                        state: BackoffState::Completed,
                        ..
                    }
                    | AttemptEvent::CleanupStarted { .. }
            )
        {
            return;
        }
        let detail = match event {
            AttemptEvent::Started {
                attempt,
                method,
                uri,
                ..
            } => format!("#{attempt} {} {}", method, compact_uri(uri)),
            AttemptEvent::RateLimit { attempt, state, .. } => {
                format!("#{attempt} rate {}", rate_limit_text(*state))
            }
            AttemptEvent::ResponseDisposition {
                attempt,
                disposition,
                response,
                ..
            } => format!(
                "#{attempt} {} · {}",
                disposition_text(*disposition),
                response_text(*response)
            ),
            AttemptEvent::Failed { attempt, error, .. } => {
                format!("#{attempt} failed · {}", error_text(error))
            }
            AttemptEvent::RetryScheduled {
                attempt,
                next_attempt,
                reason,
                ..
            } => format!(
                "#{attempt} retry→#{next_attempt} · {}",
                retry_reason_text(*reason)
            ),
            AttemptEvent::Backoff {
                attempt,
                delay,
                state,
                ..
            } => format!(
                "#{attempt} backoff {} · {}",
                backoff_state_text(*state),
                duration_text(*delay)
            ),
            AttemptEvent::CleanupStarted { attempt, .. } => format!("#{attempt} cleanup start"),
            AttemptEvent::Cleanup {
                attempt,
                state,
                bytes,
                ..
            } => {
                let bytes =
                    bytes.map_or_else(|| "bytes=?".to_owned(), |bytes| format!("{bytes} B"));
                format!(
                    "#{attempt} cleanup {} · {bytes}",
                    cleanup_state_text(*state)
                )
            }
            _ => "other attempt event".to_owned(),
        };
        let detail = if !self.theme.trace {
            match event {
                AttemptEvent::Started {
                    attempt,
                    method,
                    uri,
                    ..
                } => {
                    format!("attempt {attempt} · {method} {}", compact_uri(uri))
                }
                AttemptEvent::ResponseDisposition {
                    attempt,
                    disposition,
                    response,
                    ..
                } => format!(
                    "attempt {attempt} · {} · {}",
                    disposition_text(*disposition),
                    response_text(*response)
                ),
                AttemptEvent::Failed { attempt, error, .. } => {
                    format!("attempt {attempt} failed · {}", error_text(error))
                }
                AttemptEvent::RetryScheduled {
                    attempt,
                    next_attempt,
                    reason,
                    ..
                } => format!(
                    "retry {attempt} → {next_attempt} · {}",
                    retry_reason_text(*reason)
                ),
                AttemptEvent::Backoff { delay, state, .. } => {
                    format!(
                        "backoff {} · {}",
                        backoff_state_text(*state),
                        duration_text(*delay)
                    )
                }
                AttemptEvent::Cleanup { state, bytes, .. } => {
                    let bytes =
                        bytes.map_or_else(|| "bytes=?".to_owned(), |bytes| format!("{bytes} B"));
                    format!("cleanup {} · {bytes}", cleanup_state_text(*state))
                }
                _ => detail,
            }
        } else {
            detail
        };
        request_line(
            self.theme,
            self.id,
            match event {
                AttemptEvent::Failed { .. } => Tone::Bad,
                AttemptEvent::ResponseDisposition { .. }
                | AttemptEvent::RetryScheduled { .. }
                | AttemptEvent::Backoff { .. }
                | AttemptEvent::Cleanup { .. } => Tone::Warn,
                _ => Tone::Muted,
            },
            if self.theme.trace {
                format!("attempt  {detail}")
            } else {
                detail
            },
        );
    }

    fn on_connection_acquisition(&self, event: &ConnectionAcquisitionEvent<'_>) {
        if !self.theme.trace
            && matches!(
                event,
                ConnectionAcquisitionEvent::PhaseStarted { .. }
                    | ConnectionAcquisitionEvent::PhaseCompleted { .. }
                    | ConnectionAcquisitionEvent::PhaseSkipped { .. }
            )
        {
            return;
        }
        let detail = match event {
            ConnectionAcquisitionEvent::PoolLookup {
                attempt, result, ..
            } => format!("#{attempt} pool {}", pool_lookup_text(*result)),
            ConnectionAcquisitionEvent::PhaseStarted { attempt, phase, .. } => {
                format!("#{attempt} {} start", acquisition_phase_text(*phase))
            }
            ConnectionAcquisitionEvent::PhaseCompleted { attempt, phase, .. } => {
                format!("#{attempt} {} done", acquisition_phase_text(*phase))
            }
            ConnectionAcquisitionEvent::Failed {
                attempt,
                phase,
                error,
                ..
            } => format!(
                "#{attempt} {} failed · {}",
                acquisition_phase_text(*phase),
                error_text(error)
            ),
            ConnectionAcquisitionEvent::Cancelled { attempt, phase, .. } => {
                format!("#{attempt} {} cancelled", acquisition_phase_text(*phase))
            }
            ConnectionAcquisitionEvent::PhaseSkipped {
                attempt,
                phase,
                reason,
            } => format!(
                "#{attempt} {} skipped ({})",
                acquisition_phase_text(*phase),
                skip_reason_text(*reason)
            ),
            ConnectionAcquisitionEvent::ConnectionAcquired {
                attempt,
                resource,
                version,
                ..
            } => format!(
                "#{attempt} acquired {} · {}",
                resource_text(*resource),
                version_text(*version)
            ),
            _ => "other acquisition event".to_owned(),
        };
        let detail = if !self.theme.trace {
            match event {
                ConnectionAcquisitionEvent::PoolLookup { result, .. } => {
                    format!("pool {}", pool_lookup_text(*result))
                }
                ConnectionAcquisitionEvent::ConnectionAcquired {
                    resource, version, ..
                } => format!(
                    "acquired {} · {}",
                    resource_text(*resource),
                    version_text(*version)
                ),
                ConnectionAcquisitionEvent::Failed { phase, error, .. } => format!(
                    "connection {} failed · {}",
                    acquisition_phase_text(*phase),
                    error_text(error)
                ),
                ConnectionAcquisitionEvent::Cancelled { phase, .. } => {
                    format!("connection {} cancelled", acquisition_phase_text(*phase))
                }
                _ => detail,
            }
        } else {
            detail
        };
        request_line(
            self.theme,
            self.id,
            match event {
                ConnectionAcquisitionEvent::Failed { .. }
                | ConnectionAcquisitionEvent::Cancelled { .. } => Tone::Bad,
                ConnectionAcquisitionEvent::PoolLookup { result, .. }
                    if *result == PoolLookupResult::Miss =>
                {
                    Tone::Warn
                }
                _ => Tone::Muted,
            },
            if self.theme.trace {
                format!("acquire  {detail}")
            } else {
                detail
            },
        );
    }

    fn on_request_body(&self, event: &RequestBodyEvent<'_>) {
        let detail = match event {
            RequestBodyEvent::Ended { attempt, bytes, .. } => {
                format!("#{attempt} ended · {}", optional_bytes(*bytes))
            }
            RequestBodyEvent::Failed { attempt, error, .. } => {
                format!("#{attempt} failed · {}", error_text(error))
            }
            _ => "other request-body event".to_owned(),
        };
        let detail = if !self.theme.trace {
            match event {
                RequestBodyEvent::Ended { bytes, .. } => {
                    format!("upload ended · {}", optional_bytes(*bytes))
                }
                RequestBodyEvent::Failed { error, .. } => {
                    format!("upload failed · {}", error_text(error))
                }
                _ => detail,
            }
        } else {
            detail
        };
        request_line(
            self.theme,
            self.id,
            if matches!(event, RequestBodyEvent::Failed { .. }) {
                Tone::Bad
            } else {
                Tone::Good
            },
            if self.theme.trace {
                format!("upload   {detail}")
            } else {
                detail
            },
        );
    }

    fn on_response_body(&self, event: &ResponseBodyEvent<'_>) {
        let detail = match event {
            ResponseBodyEvent::Ended { .. } => "ended".to_owned(),
            ResponseBodyEvent::Failed { error, .. } => format!("failed · {}", error_text(error)),
            ResponseBodyEvent::Abandoned { .. } => "abandoned".to_owned(),
            ResponseBodyEvent::HandedOff { .. } => "handed off".to_owned(),
            _ => "other response-body event".to_owned(),
        };
        request_line(
            self.theme,
            self.id,
            match event {
                ResponseBodyEvent::Failed { .. } => Tone::Bad,
                ResponseBodyEvent::Abandoned { .. } => Tone::Warn,
                _ => Tone::Good,
            },
            format!("body     {detail}"),
        );
    }

    fn on_response_sse(&self, event: ResponseSseEvent<'_>) {
        let detail = match event {
            ResponseSseEvent::Started => "started".to_owned(),
            ResponseSseEvent::Error { error, statistics } => {
                format!(
                    "error · {} · {}",
                    error_text(error),
                    sse_statistics_text(statistics)
                )
            }
            ResponseSseEvent::Ended { statistics } => {
                format!("ended · {}", sse_statistics_text(statistics))
            }
            ResponseSseEvent::Abandoned { statistics } => {
                format!("abandoned · {}", sse_statistics_text(statistics))
            }
            _ => "other SSE event".to_owned(),
        };
        request_line(
            self.theme,
            self.id,
            match event {
                ResponseSseEvent::Error { .. } => Tone::Bad,
                ResponseSseEvent::Abandoned { .. } => Tone::Warn,
                _ => Tone::Good,
            },
            if self.theme.trace {
                format!("sse      {detail}")
            } else {
                format!("SSE      {detail}")
            },
        );
    }

    fn on_response_channel_handoff(&self, event: &ResponseChannelHandoffEvent<'_>) {
        let detail = match event {
            ResponseChannelHandoffEvent::ChannelHandoffStarted { info, .. } => {
                format!("start · {}", handoff_text(info))
            }
            ResponseChannelHandoffEvent::ResponseChannelHandoff { info, .. } => {
                format!("ready · {}", handoff_text(info))
            }
            ResponseChannelHandoffEvent::ChannelHandoffFailed { info, error, .. } => {
                format!("failed · {} · {}", handoff_text(info), error_text(error))
            }
            ResponseChannelHandoffEvent::ChannelHandoffCancelled { info, .. } => {
                format!("cancelled · {}", handoff_text(info))
            }
            _ => "other handoff event".to_owned(),
        };
        request_line(
            self.theme,
            self.id,
            match event {
                ResponseChannelHandoffEvent::ChannelHandoffFailed { .. }
                | ResponseChannelHandoffEvent::ChannelHandoffCancelled { .. } => Tone::Bad,
                _ => Tone::Good,
            },
            format!("handoff  {detail}"),
        );
    }

    fn on_channel_io(&self, event: &ChannelIoEvent<'_>) {
        let detail = match event {
            ChannelIoEvent::ReadEof { read_bytes_so_far } => {
                format!("read eof · read={}", optional_bytes(*read_bytes_so_far))
            }
            ChannelIoEvent::WriteShutdown { write_bytes_so_far } => {
                format!(
                    "write shutdown · written={}",
                    optional_bytes(*write_bytes_so_far)
                )
            }
            ChannelIoEvent::IoError {
                operation,
                error,
                read_bytes_so_far,
                write_bytes_so_far,
            } => format!(
                "{} error · {} · read={} · written={}",
                channel_operation_text(*operation),
                error_text(error),
                optional_bytes(*read_bytes_so_far),
                optional_bytes(*write_bytes_so_far)
            ),
            ChannelIoEvent::Detached {
                reason,
                read_eof,
                write_shutdown,
                read_bytes_so_far,
                write_bytes_so_far,
            } => format!(
                "detached ({}) · read_eof={} · write_shutdown={} · read={} · written={}",
                detach_reason_text(*reason),
                read_eof,
                write_shutdown,
                optional_bytes(*read_bytes_so_far),
                optional_bytes(*write_bytes_so_far)
            ),
            _ => "other channel event".to_owned(),
        };
        request_line(
            self.theme,
            self.id,
            match event {
                ChannelIoEvent::IoError { .. } => Tone::Bad,
                ChannelIoEvent::Detached { .. } => Tone::Warn,
                _ => Tone::Good,
            },
            format!("channel  {detail}"),
        );
    }
}

impl ConnectionObservation for ConnectionLog {
    fn on_connection(&self, event: &ConnectionObservationEvent<'_>) {
        if !self.theme.trace
            && !matches!(
                event,
                ConnectionObservationEvent::Established { .. }
                    | ConnectionObservationEvent::PoolEligibilityChanged { .. }
                    | ConnectionObservationEvent::Closed { .. }
                    | ConnectionObservationEvent::Detached { .. }
            )
        {
            return;
        }
        let detail = match event {
            ConnectionObservationEvent::ConnectAttemptStarted { attempt, .. } => format!(
                "candidate #{} {} {} start",
                attempt.sequence,
                transport_text(attempt.kind),
                attempt.remote_addr
            ),
            ConnectionObservationEvent::ConnectAttemptSucceeded { attempt, .. } => format!(
                "candidate #{} {} {} connected",
                attempt.sequence,
                transport_text(attempt.kind),
                attempt.remote_addr
            ),
            ConnectionObservationEvent::ConnectAttemptFailed { attempt, error, .. } => format!(
                "candidate #{} {} failed · {}",
                attempt.sequence,
                attempt.remote_addr,
                error_text(error)
            ),
            ConnectionObservationEvent::ConnectAttemptCancelled { attempt, .. } => {
                format!(
                    "candidate #{} {} cancelled",
                    attempt.sequence, attempt.remote_addr
                )
            }
            ConnectionObservationEvent::PhaseStarted { phase, .. } => {
                format!("{} start", establishment_phase_text(*phase))
            }
            ConnectionObservationEvent::PhaseCompleted { phase, .. } => {
                format!("{} done", establishment_phase_text(*phase))
            }
            ConnectionObservationEvent::EstablishmentFailed { error, .. } => {
                format!("establish failed · {}", error_text(error))
            }
            ConnectionObservationEvent::EstablishmentCancelled { .. } => {
                "establish cancelled".to_owned()
            }
            ConnectionObservationEvent::Established { version, .. } => {
                format!("ready · {}", version_text(*version))
            }
            ConnectionObservationEvent::PoolEligibilityChanged {
                reusable, revision, ..
            } => format!(
                "pool {} · revision={revision}",
                if *reusable {
                    "reusable"
                } else {
                    "not reusable"
                }
            ),
            ConnectionObservationEvent::Closed { error, .. } => match error {
                Some(error) => format!("closed · {}", error_text(error)),
                None => "closed".to_owned(),
            },
            ConnectionObservationEvent::Detached { .. } => "detached".to_owned(),
            _ => "other connection event".to_owned(),
        };
        connection_line(
            self.theme,
            self.id,
            match event {
                ConnectionObservationEvent::ConnectAttemptStarted { .. }
                | ConnectionObservationEvent::PhaseStarted { .. } => Tone::Muted,
                ConnectionObservationEvent::ConnectAttemptFailed { .. }
                | ConnectionObservationEvent::EstablishmentFailed { .. }
                | ConnectionObservationEvent::Closed { error: Some(_), .. } => Tone::Bad,
                ConnectionObservationEvent::ConnectAttemptCancelled { .. }
                | ConnectionObservationEvent::EstablishmentCancelled { .. }
                | ConnectionObservationEvent::PoolEligibilityChanged {
                    reusable: false, ..
                }
                | ConnectionObservationEvent::Detached { .. } => Tone::Warn,
                _ => Tone::Good,
            },
            detail,
        );
    }
}

fn request_line(theme: Theme, id: u64, tone: Tone, detail: impl std::fmt::Display) {
    let detail = detail.to_string();
    if theme.trace {
        println!("  req {id:04} │ {}", theme.paint(tone, detail));
    } else {
        println!("    {} {}", theme.symbol(tone), theme.paint(tone, detail));
    }
}

fn connection_line(theme: Theme, id: u64, tone: Tone, detail: impl std::fmt::Display) {
    let detail = detail.to_string();
    if theme.trace {
        // Keep the separator and payload in the same columns as `req 0007`.
        println!("  conn {id:02}  │ {}", theme.paint(tone, detail));
    } else {
        println!(
            "    {} {} {}",
            theme.symbol(tone),
            theme.paint(Tone::Muted, format!("connection {id:02}")),
            theme.paint(tone, detail)
        );
    }
}

fn output_theme() -> Theme {
    *OUTPUT_THEME
        .get()
        .expect("complete observer must initialize output theme")
}

fn application_line(kind: &str, detail: impl std::fmt::Display) {
    let theme = output_theme();
    let detail = detail.to_string();
    if theme.trace {
        println!("  app {kind:<4} │ {}", theme.paint(Tone::Accent, detail));
    } else {
        println!(
            "    {} {}",
            theme.paint(Tone::Accent, "→"),
            theme.paint(Tone::Accent, detail)
        );
    }
}

fn section(number: u8, title: &str) {
    let theme = output_theme();
    println!(
        "\n{}  {}",
        theme.paint(Tone::Accent, format!("{number:02}")),
        theme.paint(Tone::Accent, title)
    );
    println!(
        "{}",
        theme.paint(Tone::Muted, "────────────────────────────────────────")
    );
}

fn compact_uri(uri: &http::Uri) -> String {
    uri.path_and_query()
        .map_or_else(|| uri.to_string(), ToString::to_string)
}

fn version_text(version: http::Version) -> &'static str {
    match version {
        http::Version::HTTP_09 => "HTTP/0.9",
        http::Version::HTTP_10 => "HTTP/1.0",
        http::Version::HTTP_11 => "HTTP/1.1",
        http::Version::HTTP_2 => "HTTP/2",
        http::Version::HTTP_3 => "HTTP/3",
        _ => "HTTP/?",
    }
}

fn response_text(response: ResponseInfo) -> String {
    let source = match response.source {
        ResponseSource::Transport => "transport",
        ResponseSource::CacheHit => "cache",
        ResponseSource::CacheRevalidated => "revalidated",
        ResponseSource::CacheStaleIfError => "stale-if-error",
        _ => "unknown-source",
    };
    format!(
        "{} {} · {source}",
        response.status,
        version_text(response.version.unwrap_or(http::Version::HTTP_11))
    )
}

fn response_summary_text(response: ResponseInfo) -> String {
    response.status.as_u16().to_string()
}

fn error_text(error: &aioduct::Error) -> String {
    compact_display(error)
}

fn compact_display(value: impl std::fmt::Display) -> String {
    let mut text = value.to_string().replace(['\n', '\r'], " ");
    if text.len() > 52 {
        text.truncate(49);
        text.push_str("...");
    }
    text
}

fn optional_bytes(bytes: Option<u64>) -> String {
    bytes.map_or_else(|| "?".to_owned(), |bytes| format!("{bytes} B"))
}

fn duration_text(duration: Duration) -> String {
    if duration.as_secs() > 0 {
        format!("{}.{:03}s", duration.as_secs(), duration.subsec_millis())
    } else if duration.as_millis() > 0 {
        format!("{}ms", duration.as_millis())
    } else if duration.as_micros() > 0 {
        format!("{}µs", duration.as_micros())
    } else {
        format!("{}ns", duration.as_nanos())
    }
}

fn finish_reason(reason: RequestFinishReason<'_>) -> String {
    match reason {
        RequestFinishReason::Ended => "ended".to_owned(),
        // The attempt/body callback already carries the original error. Keep
        // the terminal line compact instead of printing the same detail twice.
        RequestFinishReason::Failed { .. } => "failed".to_owned(),
        RequestFinishReason::ConsumerAbandoned => "consumer abandoned".to_owned(),
        RequestFinishReason::ExecutionAbandoned => "execution abandoned".to_owned(),
        RequestFinishReason::HandedOff => "handed off".to_owned(),
        _ => "other finish reason".to_owned(),
    }
}

fn disposition_text(disposition: ResponseDisposition) -> &'static str {
    match disposition {
        ResponseDisposition::Retry => "retry",
        ResponseDisposition::Redirect => "redirect",
        ResponseDisposition::AuthenticationReplay => "auth replay",
        ResponseDisposition::CacheRevalidation => "cache revalidation",
        ResponseDisposition::StaleIfError => "stale-if-error",
        _ => "other disposition",
    }
}

fn retry_reason_text(reason: RetryReason<'_>) -> String {
    match reason {
        RetryReason::ResponseStatus(status) => format!("status {status}"),
        RetryReason::Error { error } => format!("error {}", error_text(error)),
        RetryReason::StaleConnection => "stale connection".to_owned(),
        RetryReason::Policy => "policy".to_owned(),
        _ => "other reason".to_owned(),
    }
}

fn rate_limit_text(state: RateLimitState) -> &'static str {
    match state {
        RateLimitState::Started => "started",
        RateLimitState::Admitted => "admitted",
        RateLimitState::Cancelled => "cancelled",
        _ => "other",
    }
}

fn backoff_state_text(state: BackoffState) -> &'static str {
    match state {
        BackoffState::Started => "started",
        BackoffState::Completed => "completed",
        BackoffState::Cancelled => "cancelled",
        _ => "other",
    }
}

fn cleanup_state_text(state: CleanupState<'_>) -> String {
    match state {
        CleanupState::Drained => "drained".to_owned(),
        CleanupState::Stopped => "stopped".to_owned(),
        CleanupState::Failed { error } => format!("failed · {}", error_text(error)),
        _ => "other".to_owned(),
    }
}

fn pool_lookup_text(result: PoolLookupResult) -> &'static str {
    match result {
        PoolLookupResult::Hit => "hit",
        PoolLookupResult::Coalesced => "coalesced",
        PoolLookupResult::Miss => "miss",
        _ => "other",
    }
}

fn acquisition_phase_text(phase: ConnectionAcquisitionPhase) -> &'static str {
    match phase {
        ConnectionAcquisitionPhase::PoolLookup => "pool lookup",
        ConnectionAcquisitionPhase::PoolWait => "pool wait",
        ConnectionAcquisitionPhase::Dns => "dns",
        ConnectionAcquisitionPhase::TcpConnect => "tcp",
        ConnectionAcquisitionPhase::QuicConnect => "quic",
        ConnectionAcquisitionPhase::TlsHandshake => "tls",
        ConnectionAcquisitionPhase::ProxyConnect => "proxy",
        ConnectionAcquisitionPhase::StreamAcquire => "stream",
        _ => "other phase",
    }
}

fn resource_text(resource: ConnectionAcquisitionResource) -> &'static str {
    match resource {
        ConnectionAcquisitionResource::Connection => "connection",
        ConnectionAcquisitionResource::Stream => "stream",
        _ => "other resource",
    }
}

fn skip_reason_text(reason: PhaseSkipReason) -> &'static str {
    match reason {
        PhaseSkipReason::NotApplicable => "not applicable",
        PhaseSkipReason::BypassedByConfiguration => "configuration",
        PhaseSkipReason::Unsupported => "unsupported",
        _ => "other",
    }
}

fn establishment_phase_text(phase: ConnectionEstablishmentPhase) -> &'static str {
    match phase {
        ConnectionEstablishmentPhase::Dns => "dns",
        ConnectionEstablishmentPhase::TcpConnect => "tcp",
        ConnectionEstablishmentPhase::QuicConnect => "quic",
        ConnectionEstablishmentPhase::TlsHandshake => "tls",
        ConnectionEstablishmentPhase::ProxyConnect => "proxy",
        _ => "other phase",
    }
}

fn transport_text(kind: TransportKind) -> &'static str {
    match kind {
        TransportKind::Tcp => "tcp",
        TransportKind::Quic => "quic",
        _ => "other",
    }
}

fn handoff_text(info: &ChannelHandoffInfo<'_>) -> String {
    let mode = match info.mode {
        ChannelHandoffMode::Http1Upgrade => "HTTP/1.1 upgrade",
        ChannelHandoffMode::Http1Connect => "HTTP/1.1 CONNECT",
        ChannelHandoffMode::Http2Connect => "HTTP/2 CONNECT",
        ChannelHandoffMode::Http2ExtendedConnect => "HTTP/2 extended CONNECT",
        _ => "other handoff",
    };
    match info.protocol {
        Some(protocol) => format!("{mode} · protocol={protocol} · status={}", info.status),
        None => format!("{mode} · status={}", info.status),
    }
}

fn channel_operation_text(operation: ChannelIoOperation) -> &'static str {
    match operation {
        ChannelIoOperation::Read => "read",
        ChannelIoOperation::Write => "write",
        ChannelIoOperation::Flush => "flush",
        ChannelIoOperation::Shutdown => "shutdown",
        _ => "other",
    }
}

fn detach_reason_text(reason: ChannelDetachReason) -> &'static str {
    match reason {
        ChannelDetachReason::Dropped => "dropped",
        ChannelDetachReason::IntoInner => "into_inner",
        _ => "other",
    }
}

fn sse_statistics_text(statistics: Option<&SseStatisticsSnapshot>) -> String {
    let Some(statistics) = statistics else {
        return "stats=off".to_owned();
    };
    format!(
        "msgs={} comments={} retry={} buf={}B",
        statistics.messages_delivered,
        statistics.comment_lines_parsed,
        statistics.retry_fields_accepted,
        statistics.buffered_input_bytes
    )
}

fn text_preview(bytes: &[u8]) -> String {
    let mut text = String::from_utf8_lossy(bytes)
        .replace(['\n', '\r', '\t'], " ")
        .trim()
        .to_owned();
    if text.chars().count() > 48 {
        text = text.chars().take(45).collect();
        text.push_str("...");
    }
    format!("{text:?}")
}

fn print_sse_application_event(event: &Result<aioduct::SseEvent, aioduct::Error>) {
    match event {
        Ok(aioduct::SseEvent::Message(message)) => application_line(
            "SSE",
            format!(
                "message · event={} · data={}",
                message.event,
                text_preview(message.data.as_bytes())
            ),
        ),
        Ok(aioduct::SseEvent::Retry(milliseconds)) => {
            application_line("SSE", format!("retry · {milliseconds}ms"))
        }
        Err(error) => application_line("SSE", format!("error · {}", error_text(error))),
    }
}

fn full_body(body: impl Into<Bytes>) -> ServerBody {
    Full::new(body.into())
        .map_err(|never| match never {})
        .boxed()
}

fn response(status: StatusCode, body: impl Into<Bytes>) -> Response<ServerBody> {
    Response::builder()
        .status(status)
        .body(full_body(body))
        .expect("static response builder arguments are valid")
}

fn truncated_body() -> ServerBody {
    // The headers and first DATA frame are sent successfully.  The body then
    // returns an I/O error, which lets the client observe a selected response
    // whose body fails without relying on a malformed Content-Length header.
    StreamBody::new(stream::iter(vec![
        Ok::<_, std::io::Error>(Frame::data(Bytes::from_static(b"short body"))),
        Err(std::io::Error::new(
            std::io::ErrorKind::ConnectionReset,
            "intentional body fixture failure",
        )),
    ]))
    .boxed()
}

async fn echo_upgraded(upgraded: hyper::upgrade::Upgraded) {
    let mut io = aioduct::UpgradedSend::from(upgraded);
    let mut buf = [0_u8; 1024];

    loop {
        let read = match io.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(read) => read,
        };
        if io.write_all(b"echo: ").await.is_err()
            || io.write_all(&buf[..read]).await.is_err()
            || io.flush().await.is_err()
        {
            break;
        }
    }

    let _ = io.shutdown().await;
}

async fn serve_request(
    mut request: Request<Incoming>,
    retry_count: Arc<AtomicUsize>,
) -> Result<Response<ServerBody>, Infallible> {
    // Both HTTP/1 Upgrade and ordinary HTTP/1 CONNECT leave the HTTP message
    // layer and become a bidirectional channel after a successful response.
    if request.method() == Method::CONNECT {
        tokio::spawn(async move {
            if let Ok(upgraded) = hyper::upgrade::on(&mut request).await {
                echo_upgraded(upgraded).await;
            }
        });
        return Ok(response(StatusCode::OK, Bytes::new()));
    }

    if request
        .headers()
        .get(header::UPGRADE)
        .is_some_and(|value| value.as_bytes().eq_ignore_ascii_case(b"websocket"))
    {
        tokio::spawn(async move {
            if let Ok(upgraded) = hyper::upgrade::on(&mut request).await {
                echo_upgraded(upgraded).await;
            }
        });
        return Ok(Response::builder()
            .status(StatusCode::SWITCHING_PROTOCOLS)
            .header(header::CONNECTION, "Upgrade")
            .header(header::UPGRADE, "websocket")
            .body(full_body(Bytes::new()))
            .expect("static response builder arguments are valid"));
    }

    match request.uri().path() {
        "/ok" => Ok(response(StatusCode::OK, "hello from /ok")),
        "/redirect" => Ok(Response::builder()
            .status(StatusCode::FOUND)
            .header(header::LOCATION, "/ok")
            .body(full_body(Bytes::new()))
            .expect("static response builder arguments are valid")),
        "/retry" => {
            let call = retry_count.fetch_add(1, Ordering::SeqCst);
            if call == 0 {
                Ok(response(StatusCode::SERVICE_UNAVAILABLE, "retry once"))
            } else {
                Ok(response(StatusCode::OK, "retry succeeded"))
            }
        }
        "/upload" => {
            let received = request
                .into_body()
                .collect()
                .await
                .map(|body| body.to_bytes().len())
                .unwrap_or(0);
            Ok(response(
                StatusCode::OK,
                format!("server received {received} upload bytes"),
            ))
        }
        "/hang" => std::future::pending().await,
        "/body-error" => Ok(Response::builder()
            .status(StatusCode::OK)
            // The client receives headers and then discovers a body error.
            // This is a body failure after response selection, not an
            // execution retry.
            .body(truncated_body())
            .expect("static response builder arguments are valid")),
        "/sse" => {
            let frames = vec![
                Ok::<_, std::io::Error>(Frame::data(Bytes::from_static(
                    b": a comment\nretry: 250\n\n",
                ))),
                Ok(Frame::data(Bytes::from_static(
                    b"event: greeting\ndata: hello\nid: first\n\n",
                ))),
                Ok(Frame::data(Bytes::from_static(b"data: world\n\n"))),
            ];
            let body = StreamBody::new(stream::iter(frames)).boxed();
            Ok(Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, "text/event-stream")
                .header(header::CACHE_CONTROL, "no-cache")
                .body(body)
                .expect("static response builder arguments are valid"))
        }
        _ => Ok(response(StatusCode::NOT_FOUND, "not found")),
    }
}

async fn start_server() -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("bind local example server");
    let address = listener.local_addr().expect("read local server address");
    let retry_count = Arc::new(AtomicUsize::new(0));

    let task = tokio::spawn(async move {
        loop {
            let (stream, _) = match listener.accept().await {
                Ok(connection) => connection,
                Err(error) => {
                    eprintln!("example server accept failed: {error}");
                    break;
                }
            };
            let retry_count = Arc::clone(&retry_count);
            tokio::spawn(async move {
                let io = aioduct::runtime::tokio_rt::TokioIo::new(stream);
                let service =
                    service_fn(move |request| serve_request(request, Arc::clone(&retry_count)));
                // `/body-error` intentionally ends the server response with
                // a body error; the client observes that error.
                let _ = http1::Builder::new()
                    .serve_connection(io, service)
                    .with_upgrades()
                    .await;
            });
        }
    });

    (address, task)
}

async fn start_body_error_server() -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("bind body-error fixture server");
    let address = listener
        .local_addr()
        .expect("read body-error fixture address");
    let task = tokio::spawn(async move {
        let Ok((mut stream, _)) = listener.accept().await else {
            return;
        };
        let mut request = Vec::new();
        let mut buffer = [0_u8; 1024];
        while !request.windows(4).any(|window| window == b"\r\n\r\n") {
            let Ok(read) = stream.read(&mut buffer).await else {
                return;
            };
            if read == 0 {
                return;
            }
            request.extend_from_slice(&buffer[..read]);
        }
        // Raw bytes are intentional here: hyper's server refuses to create
        // an internally inconsistent body.  A real peer can still close a
        // response after headers, which is the client-side body-failure case
        // this fixture needs to demonstrate.
        let _ = stream
            .write_all(
                b"HTTP/1.1 200 OK\r\nContent-Length: 1024\r\nConnection: close\r\n\r\nshort body",
            )
            .await;
        let _ = stream.shutdown().await;
    });
    (address, task)
}

fn url(address: SocketAddr, path: &str) -> String {
    // `localhost` keeps the hostname visible to connection-acquisition
    // observers.  The server is bound to IPv4, so this remains deterministic
    // even on hosts that resolve localhost to both address families.
    format!("http://localhost:{}{path}", address.port())
}

async fn run_requests(
    client: &TokioClient<CompleteObserver>,
    address: SocketAddr,
    body_error_address: SocketAddr,
) -> Result<(), aioduct::Error> {
    section(1, "ordinary GET · start, headers, body, finish");
    let body = client
        .get(&url(address, "/ok"))?
        .observation_context(1)
        .send()
        .await?
        .bytes()
        .await?;
    application_line(
        "HTTP",
        format!("body {} · {}", body.len(), text_preview(&body)),
    );

    section(2, "pool hit · reuse without a new physical connection");
    let body = client
        .get(&url(address, "/ok"))?
        .observation_context(2)
        .send()
        .await?
        .text()
        .await?;
    application_line("HTTP", format!("body · {}", text_preview(body.as_bytes())));

    section(3, "redirect · one logical request, multiple attempts");
    let response = client
        .get(&url(address, "/redirect"))?
        .observation_context(3)
        .send()
        .await?;
    application_line("HTTP", format!("final status={}", response.status()));
    let _ = response.bytes().await?;

    section(4, "retry · 503, cleanup, backoff, then 200");
    let response = client
        .get(&url(address, "/retry"))?
        .observation_context(4)
        .send()
        .await?;
    application_line("HTTP", format!("final status={}", response.status()));
    let _ = response.bytes().await?;

    section(5, "upload · request-body terminal observation");
    let response = client
        .post(&url(address, "/upload"))?
        .observation_context(5)
        .body("request body observed once")
        .send()
        .await?;
    application_line("HTTP", format!("body · {}", response.text().await?));

    section(6, "failed upload · body error before response selection");
    let request_body = StreamBody::new(stream::iter(vec![Err::<Frame<Bytes>, _>(
        aioduct::Error::Io(std::io::Error::new(
            std::io::ErrorKind::BrokenPipe,
            "intentional upload fixture failure",
        )),
    )]))
    .boxed_unsync();
    let result = client
        .post(&url(address, "/upload"))?
        .observation_context(6)
        .body_stream(request_body)
        .send()
        .await;
    match result {
        Ok(_) => application_line("HTTP", "unexpected upload success"),
        Err(error) => application_line(
            "HTTP",
            format!("expected error · {}", compact_display(error)),
        ),
    }

    section(7, "body failure · selected response fails while reading");
    let response = client
        .get(&url(body_error_address, "/body-error"))?
        .observation_context(7)
        // This fixture is a one-shot raw peer.  Zero retries makes the
        // selected-response/body-failure boundary visible on the first call.
        .retry(RetryConfig::default().max_retries(0))
        .send()
        .await?;
    match response.bytes().await {
        Ok(body) => application_line(
            "HTTP",
            format!("unexpected body success · {}", text_preview(&body)),
        ),
        Err(error) => application_line(
            "HTTP",
            format!("expected body error · {}", error_text(&error)),
        ),
    }

    section(8, "SSE · lifecycle and selected statistics");
    let response = client
        .get(&url(address, "/sse"))?
        .observation_context(8)
        .header(
            header::ACCEPT,
            http::HeaderValue::from_static("text/event-stream"),
        )
        .send()
        .await?;
    let mut events = response.into_sse_stream();
    while let Some(event) = events.next().await {
        print_sse_application_event(&event);
    }

    section(
        9,
        "SSE abandonment · HTTP and SSE lifetimes stay independent",
    );
    let response = client
        .get(&url(address, "/sse"))?
        .observation_context(9)
        .send()
        .await?;
    let mut events = response.into_sse_stream();
    let _ = events.next().await;
    drop(events);

    section(10, "SSE decoder error · original error is reported");
    let response = client
        .get(&url(address, "/sse"))?
        .observation_context(10)
        .send()
        .await?;
    let mut events = response.into_sse_stream().with_max_payload_size(8);
    while let Some(event) = events.next().await {
        let failed = event.is_err();
        print_sse_application_event(&event);
        if failed {
            break;
        }
    }
    drop(events);

    section(11, "cancelled execution · no response is selected");
    let _result = tokio::time::timeout(
        Duration::from_millis(20),
        client
            .get(&url(address, "/hang"))?
            .observation_context(11)
            .send(),
    )
    .await;
    application_line("HTTP", "expected timeout");

    section(12, "per-request opt-out · begin is not called");
    let response = client
        .get(&url(address, "/ok"))?
        .disable_observation()
        .send()
        .await?;
    application_line("HTTP", format!("status={}", response.status()));
    let _ = response.bytes().await?;

    section(
        13,
        "HTTP/1 Upgrade · response handoff and channel lifecycle",
    );
    let response = client
        .get(&url(address, "/upgrade"))?
        .observation_context(13)
        .upgrade()
        .send()
        .await?;
    let mut channel = response.upgrade().await?;
    channel.write_all(b"upgrade payload").await?;
    channel.shutdown().await?;
    let mut echoed = Vec::new();
    channel.read_to_end(&mut echoed).await?;
    application_line("CHAN", format!("echo · {}", text_preview(&echoed)));

    section(14, "raw extraction · explicit channel detachment");
    let response = client
        .get(&url(address, "/upgrade"))?
        .observation_context(14)
        .upgrade()
        .send()
        .await?;
    let channel = response.upgrade().await?;
    let _raw = channel.into_inner();

    section(15, "HTTP/1 CONNECT · distinct handoff mode");
    // The client API accepts a normal absolute URL; the HTTP/1 dispatch path
    // converts this CONNECT request to the required authority-form target.
    let authority = url(address, "");
    let response = client
        .request(Method::CONNECT, &authority)?
        .observation_context(15)
        .send()
        .await?;
    application_line("HTTP", format!("status={}", response.status()));
    let mut channel = response.upgrade().await?;
    channel.write_all(b"connect payload").await?;
    channel.shutdown().await?;
    let mut echoed = Vec::new();
    channel.read_to_end(&mut echoed).await?;
    application_line("CHAN", format!("echo · {}", text_preview(&echoed)));

    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let (address, server_task) = start_server().await;
    let (body_error_address, body_error_task) = start_body_error_server().await;
    let trace = std::env::args().any(|arg| arg == "--trace");
    let observer = CompleteObserver::new(trace);
    let theme = observer.theme;
    println!(
        "{}",
        theme.paint(
            Tone::Accent,
            "aioduct observability · Tokio complete example"
        )
    );
    println!(
        "{}",
        theme.paint(Tone::Muted, format!("local fixture · http://{address}"))
    );
    println!(
        "{}",
        theme.paint(
            Tone::Muted,
            if trace {
                "trace mode · every subscribed callback is rendered"
            } else {
                "summary mode · use --trace for callback-level detail"
            },
        )
    );

    let client = TokioClient::builder()
        .observer(observer)
        .retry(
            RetryConfig::default()
                .max_retries(1)
                .initial_backoff(Duration::from_millis(25))
                .max_backoff(Duration::from_millis(25)),
        )
        .build()?;

    let result = run_requests(&client, address, body_error_address).await;
    drop(client);
    // Give the independent connection owners time to report their final
    // Closed/Detached facts before the process exits.
    tokio::time::sleep(Duration::from_millis(100)).await;
    server_task.abort();
    body_error_task.abort();
    result?;

    println!("\n{}", theme.paint(Tone::Good, "✓ complete"));
    Ok(())
}
