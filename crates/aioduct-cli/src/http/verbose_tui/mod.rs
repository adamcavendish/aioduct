use std::collections::VecDeque;
use std::io::{self, IsTerminal, stdout};
use std::time::{Duration, Instant};

use aioduct::PoolStats;
use aioduct::observer::{RequestEvent, RequestPhase, RetryKind, TransferDirection};

use crate::common::copy_to_clipboard;
use crate::util::{
    duration_ms, find_split_point, human_bytes, human_speed, redact_headers, truncate_chars,
};

pub enum TuiMessage {
    ResponseHeaders(Vec<(String, String)>),
    BodyChunk(String),
    BodyDone,
    FatalError(String),
    PoolStats(PoolStats),
    Quit,
}
use crossterm::ExecutableCommand;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Frame;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use time::{OffsetDateTime, Time};
use tokio::sync::mpsc;

mod input;
mod render;

use input::handle_key_event;
use render::{declared_trailer_names, event_timestamp, render, trailers_event_text};
#[cfg(test)]
use render::{format_event_timestamp, http_focus_label, trailer_summary, trailer_text_lines};

pub struct VerboseTui {
    handle: Option<tokio::task::JoinHandle<()>>,
    tx: Option<mpsc::UnboundedSender<TuiMessage>>,
}

impl VerboseTui {
    pub fn start(
        rx: mpsc::UnboundedReceiver<RequestEvent>,
        cancel_tx: tokio::sync::watch::Sender<bool>,
    ) -> Self {
        if !stdout().is_terminal() {
            return Self {
                handle: None,
                tx: None,
            };
        }
        let (tx, tui_rx) = mpsc::unbounded_channel();
        let handle = tokio::spawn(run_tui(rx, tui_rx, cancel_tx));
        Self {
            handle: Some(handle),
            tx: Some(tx),
        }
    }

    pub fn send_response_headers(&self, headers: Vec<(String, String)>) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(TuiMessage::ResponseHeaders(headers));
        }
    }

    /// Wait for the user to press 'q'. Used on the success path.
    pub async fn wait(&mut self) {
        if let Some(handle) = self.handle.take() {
            let _ = handle.await;
        }
    }

    pub fn send_body_chunk(&self, text: String) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(TuiMessage::BodyChunk(text));
        }
    }

    pub fn send_body_done(&self) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(TuiMessage::BodyDone);
        }
    }

    pub fn send_fatal_error(&self, error: String) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(TuiMessage::FatalError(error));
        }
    }

    pub fn send_pool_stats(&self, stats: PoolStats) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(TuiMessage::PoolStats(stats));
        }
    }

    /// Force-quit the TUI immediately. Used on error paths.
    pub async fn stop(&mut self) {
        if let Some(tx) = self.tx.take() {
            let _ = tx.send(TuiMessage::Quit);
        }
        if let Some(handle) = self.handle.take() {
            let _ = handle.await;
        }
    }
}

const HTTP_TABS: [&str; 6] = ["Overview", "Trace", "Headers", "Body", "Events", "Summary"];
const MAX_EVENT_LINES: usize = 240;

/// Sanitize text for single-line display: collapse newlines, strip ANSI escapes,
/// and replace control characters so they can't corrupt the TUI layout.
fn sanitize_event_text(mut text: String) -> String {
    // Strip ANSI escape sequences (ESC [ ... m, etc.)
    while let Some(start) = text.find('\x1b') {
        let slice = &text[start..];
        let end = slice
            .find(|c: char| c.is_ascii_alphabetic() || c == '~')
            .map(|i| start + i + 1)
            .unwrap_or(start + 1);
        text.replace_range(start..end, "");
    }
    // Collapse newlines and control characters to a single space
    let mut result = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '\n' | '\r' => {
                if !result.ends_with(' ') {
                    result.push(' ');
                }
            }
            c if c.is_control() && c != '\t' => {
                if !result.ends_with(' ') {
                    result.push(' ');
                }
            }
            _ => result.push(c),
        }
    }
    result
}

struct TuiState {
    active_page: usize,
    focus_index: usize,
    phases: Vec<PhaseEntry>,
    event_lines: VecDeque<EventLine>,
    redirects: Vec<(String, String, String)>,
    retries: Vec<String>,
    request_headers: Vec<(String, String)>,
    response_headers: Vec<(String, String)>,
    trailers: Vec<(String, String)>,
    trailers_observable: bool,
    content_type_label: String,
    status_line: String,
    method_label: String,
    target_label: String,
    status_label: String,
    protocol_label: String,
    remote_label: String,
    final_status_is_error: bool,
    fatal_error: Option<String>,
    body_lines: Vec<String>,
    body_cap_bytes: usize,
    body_scroll: usize,
    request_header_scroll: usize,
    response_header_scroll: usize,
    trailer_scroll: usize,
    event_scroll: usize,
    body_is_sse: bool,
    body_done: bool,
    body_auto_scroll: bool,
    body_partial: String,
    body_col_width: usize,
    body_visible_rows: usize,
    transfer_idx: Option<usize>,
    show_help: bool,
    editing_filter: bool,
    filter_query: String,
    done: bool,
    phase_cumulative_ms: f64,
    total_duration_ms: Option<f64>,
    request_start_at: Instant,
    sse_event_count: usize,
    sse_first_event_at: Option<Instant>,
    sse_last_event_at: Option<Instant>,
    sse_gaps: VecDeque<f64>,
    body_bytes_received: usize,
    transfer_start_at: Option<Instant>,
    transfer_end_at: Option<Instant>,
    pool_stats: Option<PoolStats>,
}

struct PhaseEntry {
    label: String,
    duration_ms: f64,
    cumulative_ms: f64,
    color: Color,
}

#[derive(Clone)]
struct EventLine {
    timestamp: String,
    text: String,
    color: Color,
}

impl TuiState {
    fn new() -> Self {
        Self {
            active_page: 0,
            focus_index: 0,
            phases: Vec::new(),
            event_lines: VecDeque::with_capacity(MAX_EVENT_LINES),
            redirects: Vec::new(),
            retries: Vec::new(),
            request_headers: Vec::new(),
            response_headers: Vec::new(),
            trailers: Vec::new(),
            trailers_observable: false,
            content_type_label: String::new(),
            status_line: String::new(),
            method_label: "HTTP".into(),
            target_label: String::new(),
            status_label: "connecting".into(),
            protocol_label: String::new(),
            remote_label: String::new(),
            final_status_is_error: false,
            fatal_error: None,
            body_lines: Vec::new(),
            body_cap_bytes: 0,
            body_scroll: 0,
            request_header_scroll: 0,
            response_header_scroll: 0,
            trailer_scroll: 0,
            event_scroll: 0,
            body_is_sse: false,
            body_done: false,
            body_auto_scroll: true,
            body_partial: String::new(),
            body_col_width: 40,
            body_visible_rows: 20,
            transfer_idx: None,
            show_help: false,
            editing_filter: false,
            filter_query: String::new(),
            done: false,
            phase_cumulative_ms: 0.0,
            total_duration_ms: None,
            request_start_at: Instant::now(),
            sse_event_count: 0,
            sse_first_event_at: None,
            sse_last_event_at: None,
            sse_gaps: VecDeque::with_capacity(128),
            body_bytes_received: 0,
            transfer_start_at: None,
            transfer_end_at: None,
            pool_stats: None,
        }
    }

    fn apply(&mut self, event: &RequestEvent) {
        self.method_label = event.method.to_string();
        self.target_label = event.uri.to_string();
        match &event.phase {
            RequestPhase::Started => {
                self.log_event("start request", Color::DarkGray);
                self.phases.push(PhaseEntry {
                    label: "START".into(),
                    duration_ms: 0.0,
                    cumulative_ms: 0.0,
                    color: Color::DarkGray,
                });
            }
            RequestPhase::DnsResolved { duration, addrs } => {
                let d = duration_ms(duration);
                self.phase_cumulative_ms += d;
                let addr = addrs
                    .first()
                    .map(|a| a.ip().to_string())
                    .unwrap_or_default();
                self.log_event(format!("dns resolved {addr} in {d:.1}ms"), Color::Blue);
                self.phases.push(PhaseEntry {
                    label: format!("DNS  {addr}"),
                    duration_ms: d,
                    cumulative_ms: self.phase_cumulative_ms,
                    color: Color::Blue,
                });
            }
            RequestPhase::TcpConnected {
                duration,
                remote_addr,
                protocol,
            } => {
                let d = duration_ms(duration);
                self.phase_cumulative_ms += d;
                self.remote_label = remote_addr.to_string();
                self.protocol_label = format!("{protocol:?}");
                self.status_line = format!("{remote_addr} | {protocol:?}");
                self.log_event(
                    format!("tcp connected {remote_addr} via {protocol:?} in {d:.1}ms"),
                    Color::Blue,
                );
                self.phases.push(PhaseEntry {
                    label: "TCP".into(),
                    duration_ms: d,
                    cumulative_ms: self.phase_cumulative_ms,
                    color: Color::Blue,
                });
            }
            RequestPhase::TlsHandshakeComplete {
                duration,
                alpn_protocol,
                ..
            } => {
                let d = duration_ms(duration);
                self.phase_cumulative_ms += d;
                let alpn = alpn_protocol.as_deref().unwrap_or("");
                if !alpn.is_empty() {
                    self.status_line.push_str(&format!(" | ALPN={alpn}"));
                }
                self.log_event(format!("tls handshake {alpn} in {d:.1}ms"), Color::Cyan);
                self.phases.push(PhaseEntry {
                    label: "TLS".into(),
                    duration_ms: d,
                    cumulative_ms: self.phase_cumulative_ms,
                    color: Color::Cyan,
                });
            }
            RequestPhase::RequestSent { duration, headers } => {
                self.request_headers = redact_headers(headers);
                let cumulative = duration_ms(duration);
                let per_phase = (cumulative - self.phase_cumulative_ms).max(0.0);
                self.phase_cumulative_ms = cumulative;
                self.log_event(
                    format!("request sent, {} headers, {per_phase:.1}ms", headers.len()),
                    Color::Yellow,
                );
                self.phases.push(PhaseEntry {
                    label: "REQ".into(),
                    duration_ms: per_phase,
                    cumulative_ms: self.phase_cumulative_ms,
                    color: Color::Yellow,
                });
            }
            RequestPhase::ResponseStarted { waiting_duration } => {
                let d = duration_ms(waiting_duration);
                self.phase_cumulative_ms += d;
                self.log_event(
                    format!("first response byte after {d:.1}ms"),
                    Color::Magenta,
                );
                self.phases.push(PhaseEntry {
                    label: "WAIT".into(),
                    duration_ms: d,
                    cumulative_ms: self.phase_cumulative_ms,
                    color: Color::Magenta,
                });
            }
            RequestPhase::ResponseComplete {
                status,
                protocol,
                total_duration,
            } => {
                let total = duration_ms(total_duration);
                let per_phase = (total - self.phase_cumulative_ms).max(0.0);
                self.phase_cumulative_ms = total;
                self.total_duration_ms = Some(total);
                self.status_label = status.to_string();
                self.protocol_label = format!("{protocol:?}");
                self.final_status_is_error = status.is_client_error() || status.is_server_error();
                let color = if status.is_success() {
                    Color::Green
                } else if status.is_redirection() {
                    Color::Yellow
                } else {
                    Color::Red
                };
                self.phases.push(PhaseEntry {
                    label: format!("RESP {status}"),
                    duration_ms: per_phase,
                    cumulative_ms: self.phase_cumulative_ms,
                    color,
                });
                self.status_line = format!(
                    "{status} | {protocol:?} | {:.0}ms | {}",
                    total, self.status_line
                );
                self.log_event(
                    format!("response {status} {protocol:?} in {total:.1}ms"),
                    color,
                );
                self.done = true;
            }
            RequestPhase::Failed {
                error,
                elapsed,
                retry,
            } => {
                let total = duration_ms(elapsed);
                let per_phase = (total - self.phase_cumulative_ms).max(0.0);
                let retry_label = match retry {
                    RetryKind::None => "final",
                    RetryKind::StaleConnection => "stale retry",
                    RetryKind::Explicit => "will retry",
                };
                self.log_event(format!("failure ({retry_label}): {error}"), Color::Red);
                self.phases.push(PhaseEntry {
                    label: format!("FAIL {error}"),
                    duration_ms: per_phase,
                    cumulative_ms: total,
                    color: Color::Red,
                });
                self.status_line = format!("FAILED: {error}");
                self.status_label = "failed".into();
                self.final_status_is_error = true;
                self.done = matches!(retry, RetryKind::None);
            }
            RequestPhase::PoolCheckoutComplete {
                outcome,
                blocked_duration,
            } => {
                let d = duration_ms(blocked_duration);
                self.phase_cumulative_ms += d;
                self.log_event(
                    format!("pool checkout {outcome:?} in {d:.1}ms"),
                    Color::DarkGray,
                );
                self.phases.push(PhaseEntry {
                    label: format!("POOL {outcome:?}"),
                    duration_ms: d,
                    cumulative_ms: self.phase_cumulative_ms,
                    color: Color::DarkGray,
                });
            }
            RequestPhase::BytesTransferred {
                direction,
                cumulative_bytes,
                elapsed,
                ..
            } => {
                let dir = match direction {
                    TransferDirection::Download => "DOWN",
                    TransferDirection::Upload => "UP",
                };
                let label = format!("{dir}  {}", human_bytes(*cumulative_bytes));
                let color = Color::Blue;
                if let Some(idx) = self.transfer_idx {
                    self.phases[idx].label = label;
                    self.phases[idx].duration_ms = duration_ms(elapsed);
                } else {
                    self.transfer_start_at = Some(Instant::now());
                    self.transfer_idx = Some(self.phases.len());
                    self.phases.push(PhaseEntry {
                        label,
                        duration_ms: duration_ms(elapsed),
                        cumulative_ms: 0.0,
                        color,
                    });
                }
            }
            RequestPhase::TransferComplete {
                direction,
                total_bytes,
                transfer_duration,
                throughput_bytes_per_sec,
            } => {
                self.transfer_idx = None;
                self.transfer_end_at = Some(Instant::now());
                let dir = match direction {
                    TransferDirection::Download => "DOWN",
                    TransferDirection::Upload => "UP",
                };
                let label = format!(
                    "{dir}  {}  {:.0}B/s",
                    human_bytes(*total_bytes),
                    throughput_bytes_per_sec
                );
                self.log_event(
                    format!(
                        "{dir} complete: {} in {:.1}ms at {}",
                        human_bytes(*total_bytes),
                        duration_ms(transfer_duration),
                        human_speed(*throughput_bytes_per_sec as f64)
                    ),
                    Color::Green,
                );
                self.phases.push(PhaseEntry {
                    label,
                    duration_ms: duration_ms(transfer_duration),
                    cumulative_ms: 0.0,
                    color: Color::Green,
                });
            }
            RequestPhase::TransferAborted {
                direction,
                bytes_transferred,
                elapsed,
                error,
            } => {
                self.transfer_idx = None;
                let dir = match direction {
                    TransferDirection::Download => "DOWN",
                    TransferDirection::Upload => "UP",
                };
                self.phases.push(PhaseEntry {
                    label: format!("{dir}  {}  ERR {error}", human_bytes(*bytes_transferred)),
                    duration_ms: duration_ms(elapsed),
                    cumulative_ms: 0.0,
                    color: Color::Red,
                });
                self.log_event(
                    format!(
                        "{dir} aborted after {}: {error}",
                        human_bytes(*bytes_transferred)
                    ),
                    Color::Red,
                );
            }
            RequestPhase::Redirected { status, from, to } => {
                self.redirects
                    .push((status.to_string(), from.clone(), to.clone()));
                self.log_event(format!("redirect {status}: {from} -> {to}"), Color::Yellow);
                self.phases.push(PhaseEntry {
                    label: format!("REDIR {status}"),
                    duration_ms: 0.0,
                    cumulative_ms: self.phase_cumulative_ms,
                    color: Color::Yellow,
                });
                self.done = false;
            }
            RequestPhase::Retrying {
                reason,
                attempt,
                max_retries,
                backoff,
            } => {
                let retry = format!(
                    "retry #{attempt}/{max_retries} after {:.0}ms: {reason}",
                    duration_ms(backoff)
                );
                self.retries.push(retry.clone());
                self.log_event(retry, Color::Yellow);
                self.phases.push(PhaseEntry {
                    label: format!("RETRY #{attempt} {}", reason),
                    duration_ms: duration_ms(backoff),
                    cumulative_ms: self.phase_cumulative_ms,
                    color: Color::Yellow,
                });
                self.done = false;
            }

            RequestPhase::TrailersReceived { headers } => {
                let trailers = redact_headers(headers);
                let trailer_count = trailers.len();
                let event_text = trailers_event_text(&trailers);
                self.trailers_observable = true;
                self.trailers = trailers;
                self.log_event(event_text, Color::Cyan);
                self.phases.push(PhaseEntry {
                    label: format!("TRAILERS {trailer_count}"),
                    duration_ms: 0.0,
                    cumulative_ms: self.phase_cumulative_ms,
                    color: Color::Cyan,
                });
            }
        }
    }

    fn log_event(&mut self, text: impl Into<String>, color: Color) {
        if self.event_lines.len() >= MAX_EVENT_LINES {
            self.event_lines.pop_front();
        }
        self.event_lines.push_back(EventLine {
            timestamp: event_timestamp(),
            text: sanitize_event_text(text.into()),
            color,
        });
    }

    fn apply_body_chunk(&mut self, text: &str, now: Instant) {
        self.body_bytes_received += text.len();
        self.body_partial.push_str(text);
        loop {
            if let Some(pos) = self.body_partial.find('\n') {
                let line = self.body_partial[..pos].to_string();
                self.body_partial = self.body_partial[pos + 1..].to_string();
                self.track_sse_line(&line, now);
                self.add_body_line(line);
            } else if self.body_partial.len() > 1024 {
                let split_at = find_split_point(&self.body_partial, 1024);
                let line = self.body_partial[..split_at].to_string();
                self.body_partial = self.body_partial[split_at..].to_string();
                self.add_body_line(line);
            } else {
                break;
            }
        }
    }

    fn track_sse_line(&mut self, line: &str, now: Instant) {
        if !self.body_is_sse {
            return;
        }
        let trimmed = line.trim();
        if !trimmed.starts_with("data:") {
            return;
        }
        if self.sse_first_event_at.is_none() {
            self.sse_first_event_at = Some(now);
        }
        if let Some(last) = self.sse_last_event_at {
            let gap_ms = now.duration_since(last).as_secs_f64() * 1000.0;
            // Only record inter-chunk gaps (skip zero-gaps from same chunk)
            if gap_ms > 0.0 {
                if self.sse_gaps.len() >= 128 {
                    self.sse_gaps.pop_front();
                }
                self.sse_gaps.push_back(gap_ms);
            }
        }
        self.sse_last_event_at = Some(now);
        self.sse_event_count += 1;
    }

    fn apply_body_done(&mut self) {
        if !self.body_partial.is_empty() {
            let remaining = std::mem::take(&mut self.body_partial);
            self.add_body_line(remaining);
        }
        self.body_done = true;
        self.transfer_end_at = Some(Instant::now());
    }

    fn apply_fatal_error(&mut self, error: String) {
        self.fatal_error = Some(error.clone());
        self.final_status_is_error = true;
        self.status_label = "failed".into();
        self.status_line = format!("FAILED: {error}");
        self.done = true;
        self.log_event(format!("request failed: {error}"), Color::Red);
    }

    fn add_body_line(&mut self, line: String) {
        let line_bytes = line.len();
        self.body_lines.push(line);
        self.body_cap_bytes += line_bytes;
        // Rolling buffer: 64KB cap
        while self.body_cap_bytes > 64 * 1024 && !self.body_lines.is_empty() {
            let removed = self.body_lines.remove(0);
            self.body_cap_bytes = self.body_cap_bytes.saturating_sub(removed.len());
            let visual_rows = removed.len().max(1).div_ceil(self.body_col_width);
            self.body_scroll = self.body_scroll.saturating_sub(visual_rows);
        }
    }

    fn scroll_down(&mut self, lines: usize) {
        match self.active_page {
            2 if self.focus_index == 1 => {
                self.response_header_scroll = self.response_header_scroll.saturating_add(lines)
            }
            2 if self.focus_index == 2 && self.headers_show_trailers() => {
                self.trailer_scroll = self.trailer_scroll.saturating_add(lines)
            }
            2 => self.request_header_scroll = self.request_header_scroll.saturating_add(lines),
            4 => self.event_scroll = self.event_scroll.saturating_add(lines),
            1 => {
                self.focus_index =
                    (self.focus_index + lines).min(self.focus_count().saturating_sub(1))
            }
            _ => {
                self.body_scroll = self.body_scroll.saturating_add(lines);
                self.body_auto_scroll = false;
            }
        }
    }

    fn scroll_up(&mut self, lines: usize) {
        match self.active_page {
            2 if self.focus_index == 1 => {
                self.response_header_scroll = self.response_header_scroll.saturating_sub(lines)
            }
            2 if self.focus_index == 2 && self.headers_show_trailers() => {
                self.trailer_scroll = self.trailer_scroll.saturating_sub(lines)
            }
            2 => self.request_header_scroll = self.request_header_scroll.saturating_sub(lines),
            4 => self.event_scroll = self.event_scroll.saturating_sub(lines),
            1 => self.focus_index = self.focus_index.saturating_sub(lines),
            _ => {
                self.body_scroll = self.body_scroll.saturating_sub(lines);
                self.body_auto_scroll = false;
            }
        }
    }

    fn scroll_to_bottom(&mut self) {
        match self.active_page {
            2 if self.focus_index == 1 => self.response_header_scroll = usize::MAX,
            2 if self.focus_index == 2 && self.headers_show_trailers() => {
                self.trailer_scroll = usize::MAX
            }
            2 => self.request_header_scroll = usize::MAX,
            4 => self.event_scroll = usize::MAX,
            1 => self.focus_index = self.focus_count().saturating_sub(1),
            _ => {
                self.body_auto_scroll = true;
                self.body_scroll = usize::MAX; // prevent stale jump when user presses j after G
            }
        }
    }

    fn scroll_to_top(&mut self) {
        match self.active_page {
            2 if self.focus_index == 1 => self.response_header_scroll = 0,
            2 if self.focus_index == 2 && self.headers_show_trailers() => self.trailer_scroll = 0,
            2 => self.request_header_scroll = 0,
            4 => self.event_scroll = 0,
            1 => self.focus_index = 0,
            _ => {
                self.body_scroll = 0;
                self.body_auto_scroll = false;
            }
        }
    }

    fn next_page(&mut self) {
        self.active_page = (self.active_page + 1) % HTTP_TABS.len();
        self.focus_index = 0;
    }

    fn prev_page(&mut self) {
        self.active_page = (self.active_page + HTTP_TABS.len() - 1) % HTTP_TABS.len();
        self.focus_index = 0;
    }

    fn set_page(&mut self, page: usize) {
        self.active_page = page.min(HTTP_TABS.len() - 1);
        self.focus_index = 0;
    }

    fn next_focus(&mut self) {
        self.focus_index = (self.focus_index + 1) % self.focus_count();
    }

    fn prev_focus(&mut self) {
        let count = self.focus_count();
        self.focus_index = (self.focus_index + count - 1) % count;
    }

    fn focus_count(&self) -> usize {
        match self.active_page {
            0 | 3 => 2,
            1 => self.phases.len().max(1),
            2 => 3,
            _ => 1,
        }
    }

    fn headers_show_trailers(&self) -> bool {
        true
    }

    fn toggle_body_autoscroll(&mut self) {
        if self.active_page == 3 {
            self.body_auto_scroll = !self.body_auto_scroll;
            if self.body_auto_scroll {
                self.body_scroll = usize::MAX;
            }
        }
    }
}

async fn run_tui(
    mut rx: mpsc::UnboundedReceiver<RequestEvent>,
    mut msg_rx: mpsc::UnboundedReceiver<TuiMessage>,
    cancel_tx: tokio::sync::watch::Sender<bool>,
) {
    if let Err(e) = run_tui_inner(&mut rx, &mut msg_rx, cancel_tx).await {
        eprintln!("TUI error: {e}");
    }
}

async fn run_tui_inner(
    rx: &mut mpsc::UnboundedReceiver<RequestEvent>,
    msg_rx: &mut mpsc::UnboundedReceiver<TuiMessage>,
    cancel_tx: tokio::sync::watch::Sender<bool>,
) -> io::Result<()> {
    enable_raw_mode()?;
    stdout().execute(EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout());
    let mut terminal = ratatui::Terminal::new(backend)?;
    let mut state = TuiState::new();
    let mut interval = tokio::time::interval(Duration::from_millis(50));
    let mut force_quit = false;

    loop {
        while let Ok(ev) = rx.try_recv() {
            state.apply(&ev);
        }
        let tick_time = Instant::now();
        while let Ok(msg) = msg_rx.try_recv() {
            match msg {
                TuiMessage::ResponseHeaders(h) => {
                    state.body_is_sse = h.iter().any(|(k, v)| {
                        k.eq_ignore_ascii_case("content-type") && v.starts_with("text/event-stream")
                    });
                    state.content_type_label = h
                        .iter()
                        .find(|(k, _)| k.eq_ignore_ascii_case("content-type"))
                        .map(|(_, v)| v.clone())
                        .unwrap_or_default();
                    state.response_headers = h;
                    let declared = declared_trailer_names(&state);
                    if !declared.is_empty() {
                        state.log_event(
                            format!(
                                "trailers declared: {}",
                                truncate_chars(&declared.join(", "), 96)
                            ),
                            Color::Yellow,
                        );
                    }
                }
                TuiMessage::BodyChunk(text) => state.apply_body_chunk(&text, tick_time),
                TuiMessage::BodyDone => state.apply_body_done(),
                TuiMessage::PoolStats(stats) => state.pool_stats = Some(stats),
                TuiMessage::FatalError(error) => state.apply_fatal_error(error),
                TuiMessage::Quit => {
                    force_quit = true;
                    break;
                }
            }
        }

        // Render before handling keys so the final frame (e.g. [DONE]) is painted
        // before a quit key can break the loop.
        terminal.draw(|f| render(f, &mut state))?;

        if force_quit {
            break;
        }

        if event::poll(Duration::ZERO)?
            && let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
            && handle_key_event(key, &mut state)
        {
            break;
        }

        interval.tick().await;
    }

    let _ = cancel_tx.send(true);
    disable_raw_mode()?;
    stdout().execute(LeaveAlternateScreen)?;
    Ok(())
}

#[cfg(test)]
mod tests;
