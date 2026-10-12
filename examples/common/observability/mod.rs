//! Shared terminal renderer used by the runtime observability examples.
//!
//! This is deliberately an example-only module.  It renders the public
//! callback contract semantically so the Tokio, smol, and compio examples
//! present the same API, while a real application can replace the renderer
//! with tracing, OpenTelemetry, metrics, or several backends.

pub mod fixture;

use std::fmt::Display;
use std::future::poll_fn;
use std::io::IsTerminal;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use aioduct::{
    AttemptEvent, BackoffState, ChannelDetachReason, ChannelHandoffInfo, ChannelHandoffMode,
    ChannelIoEvent, ChannelIoOperation, CleanupState, ConnectionAcquisitionEvent,
    ConnectionAcquisitionPhase, ConnectionAcquisitionResource, ConnectionEstablishmentPhase,
    ConnectionObservation as ConnectionObservationTrait, ConnectionObservationEvent, Interest,
    PhaseSkipReason, PoolLookupResult, RateLimitState, RequestBodyEvent, RequestFinish,
    RequestFinishReason, RequestInfo, RequestObservation as RequestObservationTrait, RequestStart,
    ResponseBodyEvent, ResponseChannelHandoffEvent, ResponseDisposition, ResponseHeaders,
    ResponseInfo, ResponseSseEvent, RetryReason, SseStatisticsSnapshot, TransportKind,
};
use nu_ansi_term::{Color, Style};

static NEXT_REQUEST: AtomicU64 = AtomicU64::new(1_000);
static NEXT_CONNECTION: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy)]
struct Theme {
    runtime: &'static str,
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

#[derive(Clone, Copy)]
pub struct ReferenceObserver {
    theme: Theme,
}

pub struct RequestObservation {
    theme: Theme,
    id: u64,
}

pub struct ConnectionObservation {
    theme: Theme,
    id: u64,
}

impl ReferenceObserver {
    pub fn new(runtime: &'static str, trace: bool) -> Self {
        Self {
            theme: Theme {
                runtime,
                color: std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none(),
                trace,
            },
        }
    }

    pub fn heading(&self, title: impl Display) {
        println!(
            "{}",
            self.theme.paint(
                Tone::Accent,
                format!("aioduct observability · {} · {title}", self.theme.runtime),
            )
        );
        println!(
            "{}",
            self.theme.paint(
                Tone::Muted,
                if self.theme.trace {
                    "trace mode · every subscribed callback is rendered"
                } else {
                    "summary mode · use --trace for callback-level detail"
                },
            )
        );
        println!(
            "{}",
            self.theme.paint(
                Tone::Muted,
                "────────────────────────────────────────────────────────",
            )
        );
    }

    pub fn section(&self, number: u8, title: &str) {
        println!(
            "\n{}  {}",
            self.theme.paint(Tone::Accent, format!("{number:02}")),
            self.theme.paint(Tone::Accent, title)
        );
        println!(
            "{}",
            self.theme
                .paint(Tone::Muted, "────────────────────────────────────────")
        );
    }

    pub fn application_line(&self, kind: &str, detail: impl Display) {
        let detail = detail.to_string();
        if self.theme.trace {
            println!(
                "  app {kind:<4} │ {}",
                self.theme.paint(Tone::Accent, detail)
            );
        } else {
            println!(
                "    {} {}",
                self.theme.paint(Tone::Accent, "→"),
                self.theme.paint(Tone::Accent, detail)
            );
        }
    }

    pub fn print_sse_application_event(&self, event: &Result<aioduct::SseEvent, aioduct::Error>) {
        match event {
            Ok(aioduct::SseEvent::Message(message)) => self.application_line(
                "SSE",
                format!(
                    "message · event={} · data={}",
                    message.event,
                    text_preview(message.data.as_bytes())
                ),
            ),
            Ok(aioduct::SseEvent::Retry(milliseconds)) => {
                self.application_line("SSE", format!("retry · {milliseconds}ms"))
            }
            Err(error) => self.application_line("SSE", format!("error · {}", error_text(error))),
        }
    }

    pub fn complete(&self) {
        println!("\n{}", self.theme.paint(Tone::Good, "✓ complete"));
    }
}

impl Theme {
    fn paint(self, tone: Tone, text: impl Display) -> String {
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

impl aioduct::Observer for ReferenceObserver {
    type Context = u64;
    type Observation = RequestObservation;
    type ConnectionObservation = ConnectionObservation;

    fn begin(
        &self,
        _request: RequestInfo<'_>,
        context: Option<Self::Context>,
    ) -> (Interest, Self::Observation) {
        let id = context.unwrap_or_else(|| NEXT_REQUEST.fetch_add(1, Ordering::Relaxed));
        (
            all_interest(),
            RequestObservation {
                theme: self.theme,
                id,
            },
        )
    }

    fn begin_connection(&self) -> (Interest, Self::ConnectionObservation) {
        (
            Interest::CONNECTION_LIFECYCLE | Interest::CONNECTION_ESTABLISHMENT,
            ConnectionObservation {
                theme: self.theme,
                id: NEXT_CONNECTION.fetch_add(1, Ordering::Relaxed),
            },
        )
    }
}

impl RequestObservationTrait for RequestObservation {
    fn on_request_start(&self, event: &RequestStart<'_>) {
        request_line(
            self.theme,
            self.id,
            Tone::Accent,
            if self.theme.trace {
                format!("start {} {}", event.method, compact_uri(event.initial_uri))
            } else {
                format!("{} {}", event.method, compact_uri(event.initial_uri))
            },
        );
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
        let detail = if self.theme.trace {
            match event {
                AttemptEvent::Started {
                    attempt,
                    method,
                    uri,
                    ..
                } => format!("#{attempt} {method} {}", compact_uri(uri)),
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
                AttemptEvent::CleanupStarted { attempt, .. } => {
                    format!("#{attempt} cleanup start")
                }
                AttemptEvent::Cleanup {
                    attempt,
                    state,
                    bytes,
                    ..
                } => format!(
                    "#{attempt} cleanup {} · {}",
                    cleanup_state_text(*state),
                    optional_bytes(*bytes)
                ),
                _ => "other attempt event".to_owned(),
            }
        } else {
            match event {
                AttemptEvent::Started {
                    attempt,
                    method,
                    uri,
                    ..
                } => format!("attempt {attempt} · {method} {}", compact_uri(uri)),
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
                AttemptEvent::Cleanup { state, bytes, .. } => format!(
                    "cleanup {} · {}",
                    cleanup_state_text(*state),
                    optional_bytes(*bytes)
                ),
                _ => "other attempt event".to_owned(),
            }
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
            },
        );
    }

    fn on_request_body(&self, event: &RequestBodyEvent<'_>) {
        let detail = match event {
            RequestBodyEvent::Ended { bytes, .. } => {
                format!("upload ended · {}", optional_bytes(*bytes))
            }
            RequestBodyEvent::Failed { error, .. } => {
                format!("upload failed · {}", error_text(error))
            }
            _ => "other request-body event".to_owned(),
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
                let detail = match event {
                    RequestBodyEvent::Ended { attempt, bytes, .. } => {
                        format!("#{attempt} ended · {}", optional_bytes(*bytes))
                    }
                    RequestBodyEvent::Failed { attempt, error, .. } => {
                        format!("#{attempt} failed · {}", error_text(error))
                    }
                    _ => detail,
                };
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

    fn on_channel_io(&self, event: &aioduct::ChannelIoEvent<'_>) {
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

impl ConnectionObservationTrait for ConnectionObservation {
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

fn request_line(theme: Theme, id: u64, tone: Tone, detail: impl Display) {
    let detail = detail.to_string();
    if theme.trace {
        println!("  req {id:04} │ {}", theme.paint(tone, detail));
    } else {
        println!("    {} {}", theme.symbol(tone), theme.paint(tone, detail));
    }
}

fn connection_line(theme: Theme, id: u64, tone: Tone, detail: impl Display) {
    let detail = detail.to_string();
    if theme.trace {
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
        aioduct::ResponseSource::Transport => "transport",
        aioduct::ResponseSource::CacheHit => "cache",
        aioduct::ResponseSource::CacheRevalidated => "revalidated",
        aioduct::ResponseSource::CacheStaleIfError => "stale-if-error",
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

fn finish_reason(reason: RequestFinishReason<'_>) -> &'static str {
    match reason {
        RequestFinishReason::Ended => "ended",
        RequestFinishReason::Failed { .. } => "failed",
        RequestFinishReason::ConsumerAbandoned => "consumer abandoned",
        RequestFinishReason::ExecutionAbandoned => "execution abandoned",
        RequestFinishReason::HandedOff => "handed off",
        _ => "other finish reason",
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

pub fn text_preview(bytes: &[u8]) -> String {
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

pub fn error_text(error: &aioduct::Error) -> String {
    compact_display(error)
}

pub fn compact_display(value: impl Display) -> String {
    let mut text = value.to_string().replace(['\n', '\r'], " ");
    if text.len() > 52 {
        text.truncate(49);
        text.push_str("...");
    }
    text
}

pub async fn channel_write_all<T: hyper::rt::Write + Unpin>(
    io: &mut T,
    mut bytes: &[u8],
) -> std::io::Result<()> {
    while !bytes.is_empty() {
        let written =
            poll_fn(|cx| hyper::rt::Write::poll_write(Pin::new(&mut *io), cx, bytes)).await?;
        if written == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::WriteZero,
                "upgraded channel accepted no bytes",
            ));
        }
        bytes = &bytes[written..];
    }
    poll_fn(|cx| hyper::rt::Write::poll_flush(Pin::new(&mut *io), cx)).await
}

pub async fn channel_shutdown<T: hyper::rt::Write + Unpin>(io: &mut T) -> std::io::Result<()> {
    poll_fn(|cx| hyper::rt::Write::poll_shutdown(Pin::new(&mut *io), cx)).await
}

pub async fn channel_read_to_end<T: hyper::rt::Read + Unpin>(
    io: &mut T,
) -> std::io::Result<Vec<u8>> {
    let mut output = Vec::new();
    loop {
        let mut buffer = [0_u8; 1024];
        let read = poll_fn(|cx| {
            let mut read_buffer = hyper::rt::ReadBuf::new(&mut buffer);
            match hyper::rt::Read::poll_read(Pin::new(&mut *io), cx, read_buffer.unfilled()) {
                std::task::Poll::Ready(Ok(())) => {
                    std::task::Poll::Ready(Ok(read_buffer.filled().len()))
                }
                std::task::Poll::Ready(Err(error)) => std::task::Poll::Ready(Err(error)),
                std::task::Poll::Pending => std::task::Poll::Pending,
            }
        })
        .await?;
        if read == 0 {
            return Ok(output);
        }
        output.extend_from_slice(&buffer[..read]);
    }
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
