use std::collections::VecDeque;
use std::io::{self, IsTerminal, stdout};
use std::time::{Duration, Instant};

use aioduct::observer::{RequestEvent, RequestPhase, RetryKind, TransferDirection};

use crate::common::copy_to_clipboard;
use crate::util::{
    duration_ms, find_split_point, human_bytes, human_speed, redact_headers, truncate_chars,
};

pub enum TuiMessage {
    ResponseHeaders(Vec<(String, String)>),
    BodyChunk(String),
    BodyDone,
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
use tokio::sync::mpsc;

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
    body_lines: Vec<String>,
    body_cap_bytes: usize,
    body_scroll: usize,
    request_header_scroll: usize,
    response_header_scroll: usize,
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
}

struct PhaseEntry {
    label: String,
    duration_ms: f64,
    cumulative_ms: f64,
    color: Color,
}

#[derive(Clone)]
struct EventLine {
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
            body_lines: Vec::new(),
            body_cap_bytes: 0,
            body_scroll: 0,
            request_header_scroll: 0,
            response_header_scroll: 0,
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
                self.trailers_observable = true;
                self.trailers = redact_headers(headers);
            }
        }
    }

    fn log_event(&mut self, text: impl Into<String>, color: Color) {
        if self.event_lines.len() >= MAX_EVENT_LINES {
            self.event_lines.pop_front();
        }
        self.event_lines.push_back(EventLine {
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
            2 if self.headers_show_trailers() => 3,
            2 => 2,
            _ => 1,
        }
    }

    fn headers_show_trailers(&self) -> bool {
        self.trailers_observable
            || !self.trailers.is_empty()
            || self
                .response_headers
                .iter()
                .any(|(name, _)| name.eq_ignore_ascii_case("trailer"))
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
                }
                TuiMessage::BodyChunk(text) => state.apply_body_chunk(&text, tick_time),
                TuiMessage::BodyDone => state.apply_body_done(),
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

fn handle_key_event(key: crossterm::event::KeyEvent, state: &mut TuiState) -> bool {
    if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
        return true;
    }
    if state.editing_filter {
        match key.code {
            KeyCode::Esc | KeyCode::Enter => state.editing_filter = false,
            KeyCode::Backspace => {
                state.filter_query.pop();
            }
            KeyCode::Char(ch) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                state.filter_query.push(ch);
            }
            _ => {}
        }
        return false;
    }
    match key.code {
        KeyCode::Char('q') => return true,
        KeyCode::Esc if state.show_help => state.show_help = false,
        KeyCode::Tab => state.next_focus(),
        KeyCode::BackTab => state.prev_focus(),
        KeyCode::Right => state.next_focus(),
        KeyCode::Left => state.prev_focus(),
        KeyCode::Char('l') => state.next_page(),
        KeyCode::Char('h') => state.prev_page(),
        KeyCode::Char('1') => state.set_page(0),
        KeyCode::Char('2') => state.set_page(1),
        KeyCode::Char('3') => state.set_page(2),
        KeyCode::Char('4') => state.set_page(3),
        KeyCode::Char('5') => state.set_page(4),
        KeyCode::Char('6') => state.set_page(5),
        KeyCode::Char('j') | KeyCode::Down => state.scroll_down(1),
        KeyCode::Char('k') | KeyCode::Up => state.scroll_up(1),
        KeyCode::PageDown => state.scroll_down(state.body_visible_rows),
        KeyCode::PageUp => state.scroll_up(state.body_visible_rows),
        KeyCode::Char('g') | KeyCode::Home => state.scroll_to_top(),
        KeyCode::Char('G') | KeyCode::End => state.scroll_to_bottom(),
        KeyCode::Char(' ') => state.toggle_body_autoscroll(),
        KeyCode::Char('/') => state.editing_filter = true,
        KeyCode::Char('y') => copy_visible_text(state),
        KeyCode::Char('?') => state.show_help = !state.show_help,
        _ => {}
    }
    false
}

fn render(f: &mut Frame, state: &mut TuiState) {
    let size = f.area();
    if size.width < 60 || size.height < 16 {
        let msg = Paragraph::new(vec![
            Line::styled(
                "aioduct http is still running",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Line::raw(""),
            fact_line("Status", &state.status_label, Color::Yellow),
            fact_line(
                "Body",
                human_bytes(state.body_bytes_received as u64),
                Color::Cyan,
            ),
            fact_line("Need", "terminal >= 60x16", Color::DarkGray),
        ]);
        f.render_widget(msg, size);
        return;
    }

    let main_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),
            Constraint::Min(6),
            Constraint::Length(1),
        ])
        .split(size);

    render_http_header(f, main_layout[0], state);

    match state.active_page {
        0 => render_overview_page(f, main_layout[1], state),
        1 => render_trace_page(f, main_layout[1], state),
        2 => render_headers_page(f, main_layout[1], state),
        3 => render_body_page(f, main_layout[1], state),
        4 => render_events_page(f, main_layout[1], state),
        5 => render_summary_page(f, main_layout[1], state),
        _ => {}
    }

    render_footer(f, main_layout[2], state);

    if state.show_help {
        render_help(f, size);
    }
}

fn render_http_header(f: &mut Frame, area: Rect, state: &TuiState) {
    let status_color = if state.final_status_is_error {
        Color::Red
    } else if state.done {
        Color::Green
    } else {
        Color::Yellow
    };
    let timing = state
        .total_duration_ms
        .map(|ms| format!(" {ms:.1}ms"))
        .unwrap_or_default();
    let protocol = if state.protocol_label.is_empty() {
        String::new()
    } else {
        format!("  {}", state.protocol_label)
    };
    let remote = if state.remote_label.is_empty() {
        String::new()
    } else {
        format!("  {}", state.remote_label)
    };
    let reserve = 28usize + protocol.len() + remote.len();
    let target = truncate_chars(
        &state.target_label,
        area.width.saturating_sub(reserve as u16) as usize,
    );
    let line1 = Line::from(vec![
        Span::styled(" aioduct http ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            format!("{} ", state.method_label),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(target, Style::default().fg(Color::Cyan)),
        Span::raw("  "),
        Span::styled(&state.status_label, Style::default().fg(status_color)),
        Span::styled(protocol, Style::default().fg(Color::Blue)),
        Span::styled(remote, Style::default().fg(Color::DarkGray)),
        Span::styled(timing, Style::default().fg(Color::Yellow)),
    ]);

    let mut tab_spans = Vec::new();
    for (idx, name) in HTTP_TABS.iter().enumerate() {
        if idx > 0 {
            tab_spans.push(Span::raw("  "));
        }
        let style = if idx == state.active_page {
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        tab_spans.push(Span::styled(format!("{} {}", idx + 1, name), style));
    }

    f.render_widget(Paragraph::new(vec![line1, Line::from(tab_spans)]), area);
}

fn render_overview_page(f: &mut Frame, area: Rect, state: &TuiState) {
    if area.width >= 92 {
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Percentage(68), Constraint::Length(4)])
            .split(area);
        let columns = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(52), Constraint::Percentage(48)])
            .split(rows[0]);
        render_timeline_panel(f, columns[0], state, "Lifecycle");
        let right = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Percentage(52), Constraint::Percentage(48)])
            .split(columns[1]);
        render_facts_panel(f, right[0], state);
        render_body_preview_panel(f, right[1], state);
        render_metrics_strip(f, rows[1], state);
    } else {
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Percentage(42),
                Constraint::Percentage(32),
                Constraint::Percentage(26),
            ])
            .split(area);
        render_timeline_panel(f, rows[0], state, "Lifecycle");
        render_facts_panel(f, rows[1], state);
        render_body_preview_panel(f, rows[2], state);
    }
}

fn render_timeline_panel(f: &mut Frame, area: Rect, state: &TuiState, title: &str) {
    let lines: Vec<Line> = state
        .phases
        .iter()
        .map(|p| {
            let phase_time = if p.duration_ms > 0.0 {
                format!("{:>7.1}ms", p.duration_ms)
            } else {
                "       ".to_string()
            };
            let cumulative = if p.cumulative_ms > 0.0 {
                format!(" (+{:>7.1}ms)", p.cumulative_ms)
            } else {
                String::new()
            };
            Line::from(vec![
                Span::styled("● ", Style::default().fg(p.color)),
                Span::styled(
                    format!("{:<22}", p.label.chars().take(22).collect::<String>()),
                    Style::default().fg(p.color),
                ),
                Span::raw(phase_time),
                Span::styled(cumulative, Style::default().fg(Color::DarkGray)),
            ])
        })
        .collect();

    let title = if state.filter_query.trim().is_empty() {
        title.to_string()
    } else {
        format!("{title} [filter: {}]", state.filter_query)
    };
    let block = Block::default().borders(Borders::ALL).title(title);
    let paragraph = Paragraph::new(lines).block(block);
    f.render_widget(paragraph, area);
}

fn render_facts_panel(f: &mut Frame, area: Rect, state: &TuiState) {
    let mut lines = vec![
        fact_line(
            "Status",
            &state.status_label,
            if state.final_status_is_error {
                Color::Red
            } else {
                Color::Green
            },
        ),
        fact_line(
            "Protocol",
            value_or_dash(&state.protocol_label),
            Color::Blue,
        ),
        fact_line(
            "Remote",
            value_or_dash(&state.remote_label),
            Color::DarkGray,
        ),
        fact_line(
            "Duration",
            state
                .total_duration_ms
                .map(|ms| format!("{ms:.1}ms"))
                .unwrap_or_else(|| "running".into()),
            Color::Yellow,
        ),
        fact_line(
            "Body",
            human_bytes(state.body_bytes_received as u64),
            Color::Cyan,
        ),
        fact_line(
            "Redirects",
            state.redirects.len().to_string(),
            Color::Yellow,
        ),
        fact_line("Retries", state.retries.len().to_string(), Color::Yellow),
    ];

    if state.body_is_sse {
        lines.push(Line::raw(""));
        lines.push(fact_line(
            "SSE events",
            state.sse_event_count.to_string(),
            Color::Cyan,
        ));
    }

    if let Some(last) = state.event_lines.back() {
        lines.push(Line::raw(""));
        lines.push(Line::styled("Latest", Style::default().fg(Color::DarkGray)));
        lines.push(Line::styled(
            last.text.clone(),
            Style::default().fg(last.color),
        ));
    }

    let block = Block::default().borders(Borders::ALL).title("Overview");
    f.render_widget(Paragraph::new(lines).block(block), area);
}

fn render_body_preview_panel(f: &mut Frame, area: Rect, state: &TuiState) {
    let title = if state.body_is_sse {
        "SSE tail"
    } else {
        "Body preview"
    };
    let visible = area.height.saturating_sub(2) as usize;
    let width = area.width.saturating_sub(2) as usize;
    let mut lines: Vec<Line> = state
        .body_lines
        .iter()
        .rev()
        .filter(|line| text_matches_filter(line, &state.filter_query))
        .take(visible.max(1))
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .map(|line| Line::raw(truncate_chars(line, width.max(1))))
        .collect();
    if lines.is_empty() {
        lines.push(Line::styled(
            if state.response_headers.is_empty() {
                "waiting for response body"
            } else if state.body_done {
                "empty body"
            } else {
                "body stream open"
            },
            Style::default().fg(Color::DarkGray),
        ));
    }
    let block = Block::default().borders(Borders::ALL).title(title);
    f.render_widget(Paragraph::new(lines).block(block), area);
}

fn render_metrics_strip(f: &mut Frame, area: Rect, state: &TuiState) {
    let mut spans = vec![
        Span::styled(" body ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            human_bytes(state.body_bytes_received as u64),
            Style::default().fg(Color::Cyan),
        ),
        Span::styled("  duration ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            state
                .total_duration_ms
                .map(|ms| format!("{ms:.1}ms"))
                .unwrap_or_else(|| "running".to_string()),
            Style::default().fg(Color::Yellow),
        ),
    ];
    if let Some(transfer_start) = state.transfer_start_at {
        let elapsed = state
            .transfer_end_at
            .map(|end| end.duration_since(transfer_start).as_secs_f64())
            .unwrap_or_else(|| transfer_start.elapsed().as_secs_f64());
        if elapsed > 0.0 && state.body_bytes_received > 0 {
            spans.extend([
                Span::styled("  speed ", Style::default().fg(Color::DarkGray)),
                Span::styled(
                    human_speed(state.body_bytes_received as f64 / elapsed),
                    Style::default().fg(Color::Cyan),
                ),
            ]);
        }
    }
    if state.body_is_sse {
        spans.extend([
            Span::styled("  SSE ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                format!("{} events", state.sse_event_count),
                Style::default().fg(Color::Green),
            ),
        ]);
    }
    let block = Block::default().borders(Borders::ALL).title("Metrics");
    f.render_widget(Paragraph::new(Line::from(spans)).block(block), area);
}

fn render_trace_page(f: &mut Frame, area: Rect, state: &TuiState) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(6),
            Constraint::Length(6),
            Constraint::Length(3),
        ])
        .split(area);
    let columns = if rows[1].width >= 92 {
        Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(38), Constraint::Percentage(62)])
            .split(rows[1])
    } else {
        Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
            .split(rows[1])
    };

    render_trace_waterfall(f, rows[0], state);
    render_selected_span_panel(f, columns[0], state);
    render_redirect_chain_panel(f, columns[1], state);
    render_trace_totals_panel(f, rows[2], state);
}

fn render_trace_waterfall(f: &mut Frame, area: Rect, state: &TuiState) {
    let max_ms = state
        .phases
        .iter()
        .map(|p| p.duration_ms)
        .fold(1.0, f64::max);
    let bar_width = area.width.saturating_sub(38).max(8) as usize;
    let lines: Vec<Line> = state
        .phases
        .iter()
        .enumerate()
        .map(|(idx, p)| {
            let filled = ((p.duration_ms / max_ms) * bar_width as f64).round() as usize;
            let filled = filled
                .min(bar_width)
                .max(if p.duration_ms > 0.0 { 1 } else { 0 });
            let selected = idx == state.focus_index.min(state.phases.len().saturating_sub(1));
            Line::from(vec![
                Span::styled(
                    if selected { "› " } else { "  " },
                    Style::default().fg(Color::Cyan),
                ),
                Span::styled(
                    format!("{:<18}", truncate_chars(&p.label, 18)),
                    Style::default().fg(if selected { Color::Cyan } else { p.color }),
                ),
                Span::styled(
                    " ".to_string() + &"━".repeat(filled),
                    Style::default().fg(p.color),
                ),
                Span::styled(
                    format!(" {:>8.1}ms", p.duration_ms),
                    Style::default().fg(Color::Yellow),
                ),
                Span::styled(
                    format!(" +{:>8.1}ms", p.cumulative_ms),
                    Style::default().fg(Color::DarkGray),
                ),
            ])
        })
        .collect();
    let block = Block::default().borders(Borders::ALL).title("Trace");
    f.render_widget(Paragraph::new(lines).block(block), area);
}

fn render_selected_span_panel(f: &mut Frame, area: Rect, state: &TuiState) {
    let selected = state
        .phases
        .get(state.focus_index.min(state.phases.len().saturating_sub(1)));
    let lines = if let Some(phase) = selected {
        vec![
            fact_line("Selected", truncate_chars(&phase.label, 32), phase.color),
            fact_line(
                "Duration",
                format!("{:.1}ms", phase.duration_ms),
                Color::Yellow,
            ),
            fact_line(
                "Cumulative",
                format!("{:.1}ms", phase.cumulative_ms),
                Color::DarkGray,
            ),
            fact_line("Note", phase_note(&phase.label), Color::DarkGray),
        ]
    } else {
        vec![Line::styled(
            "waiting for spans",
            Style::default().fg(Color::DarkGray),
        )]
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title("Selected span");
    f.render_widget(Paragraph::new(lines).block(block), area);
}

fn render_redirect_chain_panel(f: &mut Frame, area: Rect, state: &TuiState) {
    let mut lines: Vec<Line> = state
        .redirects
        .iter()
        .map(|(status, from, to)| {
            Line::from(vec![
                Span::styled(format!("{status} "), Style::default().fg(Color::Yellow)),
                Span::styled(
                    truncate_chars(from, 22),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::raw(" -> "),
                Span::styled(
                    truncate_chars(to, area.width.saturating_sub(30) as usize),
                    Style::default().fg(Color::Cyan),
                ),
            ])
        })
        .collect();
    if lines.is_empty() {
        lines.push(Line::styled(
            "no redirects observed",
            Style::default().fg(Color::DarkGray),
        ));
    }
    let block = Block::default()
        .borders(Borders::ALL)
        .title("Redirect chain");
    f.render_widget(Paragraph::new(lines).block(block), area);
}

fn render_trace_totals_panel(f: &mut Frame, area: Rect, state: &TuiState) {
    let slowest = state
        .phases
        .iter()
        .max_by(|a, b| {
            a.duration_ms
                .partial_cmp(&b.duration_ms)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .map(|phase| {
            format!(
                "{} {:.1}ms",
                truncate_chars(&phase.label, 18),
                phase.duration_ms
            )
        })
        .unwrap_or_else(|| "-".into());
    let setup_ms: f64 = state
        .phases
        .iter()
        .filter(|phase| {
            phase.label.starts_with("DNS")
                || phase.label.starts_with("TCP")
                || phase.label.starts_with("TLS")
                || phase.label.starts_with("POOL")
        })
        .map(|phase| phase.duration_ms)
        .sum();
    let transfer_ms: f64 = state
        .phases
        .iter()
        .filter(|phase| phase.label.starts_with("DOWN") || phase.label.starts_with("UP"))
        .map(|phase| phase.duration_ms)
        .sum();
    let line = Line::from(vec![
        Span::styled("slowest ", Style::default().fg(Color::DarkGray)),
        Span::styled(slowest, Style::default().fg(Color::Magenta)),
        Span::styled("   setup ", Style::default().fg(Color::DarkGray)),
        Span::styled(format!("{setup_ms:.1}ms"), Style::default().fg(Color::Blue)),
        Span::styled("   transfer ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            format!("{transfer_ms:.1}ms"),
            Style::default().fg(Color::Green),
        ),
    ]);
    let block = Block::default().borders(Borders::ALL).title("Totals");
    f.render_widget(Paragraph::new(line).block(block), area);
}

fn render_headers_page(f: &mut Frame, area: Rect, state: &mut TuiState) {
    let show_trailers = state.trailers_observable
        || !state.trailers.is_empty()
        || state
            .response_headers
            .iter()
            .any(|(name, _)| name.eq_ignore_ascii_case("trailer"));
    if area.width >= 120 && show_trailers {
        let columns = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Percentage(34),
                Constraint::Percentage(34),
                Constraint::Percentage(32),
            ])
            .split(area);
        render_headers_panel(
            f,
            columns[0],
            focused_title(
                "Request Headers",
                state.active_page == 2 && state.focus_index == 0,
            ),
            &state.request_headers,
            state.request_header_scroll,
            &state.filter_query,
        );
        render_headers_panel(
            f,
            columns[1],
            focused_title(
                "Response Headers",
                state.active_page == 2 && state.focus_index == 1,
            ),
            &state.response_headers,
            state.response_header_scroll,
            &state.filter_query,
        );
        render_trailers_panel(
            f,
            columns[2],
            focused_title("Trailers", state.active_page == 2 && state.focus_index == 2),
            state,
        );
    } else if show_trailers {
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Percentage(34),
                Constraint::Percentage(34),
                Constraint::Percentage(32),
            ])
            .split(area);
        render_headers_panel(
            f,
            rows[0],
            focused_title(
                "Request Headers",
                state.active_page == 2 && state.focus_index == 0,
            ),
            &state.request_headers,
            state.request_header_scroll,
            &state.filter_query,
        );
        render_headers_panel(
            f,
            rows[1],
            focused_title(
                "Response Headers",
                state.active_page == 2 && state.focus_index == 1,
            ),
            &state.response_headers,
            state.response_header_scroll,
            &state.filter_query,
        );
        render_trailers_panel(
            f,
            rows[2],
            focused_title("Trailers", state.active_page == 2 && state.focus_index == 2),
            state,
        );
    } else if area.width >= 96 {
        let columns = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
            .split(area);
        render_headers_panel(
            f,
            columns[0],
            focused_title(
                "Request Headers",
                state.active_page == 2 && state.focus_index == 0,
            ),
            &state.request_headers,
            state.request_header_scroll,
            &state.filter_query,
        );
        render_headers_panel(
            f,
            columns[1],
            focused_title(
                "Response Headers",
                state.active_page == 2 && state.focus_index == 1,
            ),
            &state.response_headers,
            state.response_header_scroll,
            &state.filter_query,
        );
    } else {
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
            .split(area);
        render_headers_panel(
            f,
            rows[0],
            focused_title(
                "Request Headers",
                state.active_page == 2 && state.focus_index == 0,
            ),
            &state.request_headers,
            state.request_header_scroll,
            &state.filter_query,
        );
        render_headers_panel(
            f,
            rows[1],
            focused_title(
                "Response Headers",
                state.active_page == 2 && state.focus_index == 1,
            ),
            &state.response_headers,
            state.response_header_scroll,
            &state.filter_query,
        );
    }
}

fn render_headers_panel(
    f: &mut Frame,
    area: Rect,
    title: String,
    headers: &[(String, String)],
    scroll: usize,
    filter_query: &str,
) {
    let lines: Vec<Line> = headers
        .iter()
        .filter(|(k, v)| header_matches_filter(k, v, filter_query))
        .map(|(k, v)| {
            Line::from(vec![
                Span::styled(format!("{k}: "), Style::default().fg(Color::Cyan)),
                Span::raw(v.as_str()),
            ])
        })
        .collect();

    let block = Block::default().borders(Borders::ALL).title(title);
    let max_scroll = lines
        .len()
        .saturating_sub(area.height.saturating_sub(2) as usize);
    let scroll = scroll.min(max_scroll).min(u16::MAX as usize) as u16;
    let paragraph = Paragraph::new(lines).block(block).scroll((scroll, 0));
    f.render_widget(paragraph, area);
}

fn render_trailers_panel(f: &mut Frame, area: Rect, title: String, state: &TuiState) {
    let mut lines: Vec<Line> = state
        .trailers
        .iter()
        .filter(|(k, v)| header_matches_filter(k, v, &state.filter_query))
        .map(|(k, v)| {
            Line::from(vec![
                Span::styled(format!("{k}: "), Style::default().fg(Color::Cyan)),
                Span::raw(v.as_str()),
            ])
        })
        .collect();
    if lines.is_empty() {
        lines.push(Line::styled(
            if state.trailers_observable {
                "none received"
            } else {
                "not exposed by client yet"
            },
            Style::default().fg(Color::DarkGray),
        ));
    }
    let block = Block::default().borders(Borders::ALL).title(title);
    f.render_widget(Paragraph::new(lines).block(block), area);
}

fn render_body_page(f: &mut Frame, area: Rect, state: &mut TuiState) {
    if area.width >= 100 && area.height >= 18 {
        let columns = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(68), Constraint::Percentage(32)])
            .split(area);
        render_body(f, columns[0], state);
        render_metrics_panel(f, columns[1], state);
    } else {
        render_body(f, area, state);
    }
}

fn render_events_page(f: &mut Frame, area: Rect, state: &mut TuiState) {
    let title = if state.filter_query.trim().is_empty() {
        "Events".to_string()
    } else {
        format!("Events [filter: {}]", state.filter_query)
    };
    let block = Block::default().borders(Borders::ALL).title(title);
    let inner = block.inner(area);
    f.render_widget(block, area);

    let visible = inner.height as usize;
    let filtered_events: Vec<&EventLine> = state
        .event_lines
        .iter()
        .filter(|line| text_matches_filter(&line.text, &state.filter_query))
        .collect();
    let max_scroll = filtered_events.len().saturating_sub(visible);
    let scroll = state.event_scroll.min(max_scroll);
    state.event_scroll = scroll;
    let lines: Vec<Line> = filtered_events
        .into_iter()
        .skip(scroll)
        .take(visible)
        .map(|line| Line::styled(line.text.clone(), Style::default().fg(line.color)))
        .collect();
    f.render_widget(Paragraph::new(lines), inner);
}

fn render_summary_page(f: &mut Frame, area: Rect, state: &TuiState) {
    let mut lines = vec![
        Line::styled(
            if state.final_status_is_error {
                "failure"
            } else if state.done {
                "success"
            } else {
                "running"
            },
            Style::default()
                .fg(if state.final_status_is_error {
                    Color::Red
                } else {
                    Color::Green
                })
                .add_modifier(Modifier::BOLD),
        ),
        Line::raw(""),
        fact_line(
            "Request",
            format!("{} {}", state.method_label, state.target_label),
            Color::Cyan,
        ),
        fact_line("Status", &state.status_label, Color::White),
        fact_line(
            "Protocol",
            value_or_dash(&state.protocol_label),
            Color::Blue,
        ),
        fact_line(
            "Remote",
            value_or_dash(&state.remote_label),
            Color::DarkGray,
        ),
        fact_line(
            "Total",
            state
                .total_duration_ms
                .map(|ms| format!("{ms:.1}ms"))
                .unwrap_or_else(|| "-".into()),
            Color::Yellow,
        ),
        fact_line(
            "Body",
            human_bytes(state.body_bytes_received as u64),
            Color::Cyan,
        ),
        fact_line(
            "Request headers",
            state.request_headers.len().to_string(),
            Color::White,
        ),
        fact_line(
            "Response headers",
            state.response_headers.len().to_string(),
            Color::White,
        ),
        fact_line(
            "Trailers",
            trailer_summary(state),
            if state.trailers.is_empty() {
                Color::DarkGray
            } else {
                Color::Cyan
            },
        ),
        fact_line("Body handling", body_mode_label(state), Color::DarkGray),
    ];

    if state.final_status_is_error {
        lines.push(Line::raw(""));
        lines.push(Line::styled("Failure", Style::default().fg(Color::Red)));
        lines.push(fact_line("Phase/error", &state.status_line, Color::Red));
        lines.push(fact_line(
            "Retries",
            if state.retries.is_empty() {
                "none".to_string()
            } else {
                format!("{} attempts", state.retries.len())
            },
            Color::Yellow,
        ));
        lines.push(fact_line(
            "Output",
            if state.body_bytes_received == 0 {
                "no body received"
            } else {
                "partial body received"
            },
            Color::DarkGray,
        ));
    }

    if !state.redirects.is_empty() {
        lines.push(Line::raw(""));
        lines.push(Line::styled(
            "Redirect Chain",
            Style::default().fg(Color::Yellow),
        ));
        for (idx, (status, from, to)) in state.redirects.iter().enumerate() {
            lines.push(Line::raw(format!(
                "  {}. {status} {} -> {}",
                idx + 1,
                truncate_chars(from, 34),
                truncate_chars(to, 44)
            )));
        }
    }

    if !state.retries.is_empty() {
        lines.push(Line::raw(""));
        lines.push(Line::styled("Retries", Style::default().fg(Color::Yellow)));
        for retry in &state.retries {
            lines.push(Line::raw(format!("  {retry}")));
        }
    }

    lines.push(Line::raw(""));
    lines.push(Line::styled("Trailers", Style::default().fg(Color::Cyan)));
    if state.trailers.is_empty() {
        lines.push(Line::styled(
            if state.trailers_observable {
                "  none received"
            } else {
                "  not exposed by client yet"
            },
            Style::default().fg(Color::DarkGray),
        ));
    } else {
        for (name, value) in &state.trailers {
            lines.push(Line::raw(format!("  {name}: {value}")));
        }
    }

    let block = Block::default().borders(Borders::ALL).title("Summary");
    f.render_widget(Paragraph::new(lines).block(block), area);
}

fn render_metrics_panel(f: &mut Frame, area: Rect, state: &TuiState) {
    let mut lines: Vec<Line> = Vec::new();

    // ── Transfer stats ──
    lines.push(Line::styled(
        "── Transfer ──",
        Style::default().fg(Color::Cyan),
    ));
    lines.push(Line::from(vec![
        Span::raw("  Body: "),
        Span::styled(
            human_bytes(state.body_bytes_received as u64),
            Style::default().fg(Color::Yellow),
        ),
    ]));

    // Transfer duration & throughput
    if let Some(transfer_start) = state.transfer_start_at {
        let elapsed = state
            .transfer_end_at
            .map(|end| end.duration_since(transfer_start).as_secs_f64())
            .unwrap_or_else(|| transfer_start.elapsed().as_secs_f64());
        if elapsed > 0.0 && state.body_bytes_received > 0 {
            let bps = state.body_bytes_received as f64 / elapsed;
            lines.push(Line::from(vec![
                Span::raw("  Speed: "),
                Span::styled(human_speed(bps), Style::default().fg(Color::Yellow)),
            ]));
            lines.push(Line::from(vec![
                Span::raw("  Duration: "),
                Span::styled(
                    format!("{:.1}s", elapsed),
                    Style::default().fg(Color::Yellow),
                ),
            ]));
        }
    }

    // ── SSE metrics ──
    if state.body_is_sse && state.sse_event_count > 0 {
        lines.push(Line::raw(""));
        lines.push(Line::styled("── SSE ──", Style::default().fg(Color::Cyan)));
        lines.push(Line::from(vec![
            Span::raw("  Events: "),
            Span::styled(
                format!("{}", state.sse_event_count),
                Style::default().fg(Color::Yellow),
            ),
        ]));

        if let Some(first_at) = state.sse_first_event_at {
            let ttfe = first_at
                .duration_since(state.request_start_at)
                .as_secs_f64()
                * 1000.0;
            lines.push(Line::from(vec![
                Span::raw("  TTFE: "),
                Span::styled(format!("{ttfe:.1}ms"), Style::default().fg(Color::Yellow)),
            ]));
        }

        // Compute quantiles from gaps
        let gaps: Vec<f64> = {
            let mut v: Vec<f64> = state.sse_gaps.iter().copied().collect();
            v.sort_unstable_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            v
        };
        if !gaps.is_empty() {
            let p50 = gaps[gaps.len() * 50 / 100];
            let p95 = gaps[(gaps.len() * 95 / 100).min(gaps.len() - 1)];
            let p99 = gaps[(gaps.len() * 99 / 100).min(gaps.len() - 1)];
            lines.push(Line::from(vec![
                Span::raw("  Gap p50: "),
                Span::styled(format!("{p50:.1}ms"), Style::default().fg(Color::Yellow)),
                Span::raw("  p95: "),
                Span::styled(format!("{p95:.1}ms"), Style::default().fg(Color::Yellow)),
                Span::raw("  p99: "),
                Span::styled(format!("{p99:.1}ms"), Style::default().fg(Color::Yellow)),
            ]));
        }
    }

    let block = Block::default().borders(Borders::ALL).title("Body Metrics");
    let paragraph = Paragraph::new(lines).block(block);
    f.render_widget(paragraph, area);
}

fn phase_note(label: &str) -> &'static str {
    if label.starts_with("WAIT") {
        "time-to-first-byte"
    } else if label.starts_with("DNS") || label.starts_with("TCP") || label.starts_with("TLS") {
        "transport setup"
    } else if label.starts_with("DOWN") || label.starts_with("UP") {
        "body transfer"
    } else if label.starts_with("FAIL") {
        "terminal error"
    } else {
        "-"
    }
}

fn trailer_summary(state: &TuiState) -> String {
    if !state.trailers.is_empty() {
        format!("{} received", state.trailers.len())
    } else if state.trailers_observable {
        "none received".to_string()
    } else {
        "not exposed".to_string()
    }
}

fn body_mode_label(state: &TuiState) -> String {
    let mode = if state.body_is_sse {
        "SSE"
    } else if state
        .body_lines
        .iter()
        .any(|line| line.trim_start().starts_with('{'))
    {
        "JSON/text"
    } else if state.content_type_label.is_empty()
        || state.content_type_label.starts_with("text/")
        || state.content_type_label.contains("json")
        || state.content_type_label.contains("xml")
    {
        "text"
    } else {
        "binary or mixed"
    };
    let state_label = if state.body_done { "done" } else { "streaming" };
    let cap = if state.body_cap_bytes > 64 * 1024 {
        ", truncated"
    } else {
        ""
    };
    format!("{mode}, {state_label}{cap}")
}

fn fact_line(label: &str, value: impl Into<String>, color: Color) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{label:<16}"), Style::default().fg(Color::DarkGray)),
        Span::styled(value.into(), Style::default().fg(color)),
    ])
}

fn value_or_dash(value: &str) -> &str {
    if value.is_empty() { "-" } else { value }
}

fn focused_title(title: &str, focused: bool) -> String {
    if focused {
        format!("{title} [focus]")
    } else {
        title.to_string()
    }
}

fn http_focus_label(state: &TuiState) -> &'static str {
    match state.active_page {
        0 => {
            if state.focus_index == 1 {
                "facts"
            } else {
                "timeline"
            }
        }
        2 => {
            if state.focus_index == 1 {
                "response"
            } else {
                "request"
            }
        }
        3 => {
            if state.focus_index == 1 {
                "metrics"
            } else {
                "body"
            }
        }
        _ => "main",
    }
}

fn text_matches_filter(text: &str, filter_query: &str) -> bool {
    let query = filter_query.trim().to_ascii_lowercase();
    query.is_empty() || text.to_ascii_lowercase().contains(&query)
}

fn header_matches_filter(name: &str, value: &str, filter_query: &str) -> bool {
    let query = filter_query.trim().to_ascii_lowercase();
    query.is_empty()
        || name.to_ascii_lowercase().contains(&query)
        || value.to_ascii_lowercase().contains(&query)
}

fn copy_visible_text(state: &mut TuiState) {
    let text = visible_page_text(state);
    match copy_to_clipboard(&text) {
        Ok(()) => state.log_event("copied visible page to clipboard", Color::Green),
        Err(err) => state.log_event(format!("copy failed: {err}"), Color::Red),
    }
}

fn visible_page_text(state: &TuiState) -> String {
    match state.active_page {
        0 => [
            format!("status: {}", state.status_label),
            format!("protocol: {}", value_or_dash(&state.protocol_label)),
            format!("remote: {}", value_or_dash(&state.remote_label)),
            format!(
                "duration: {}",
                state
                    .total_duration_ms
                    .map(|ms| format!("{ms:.1}ms"))
                    .unwrap_or_else(|| "running".into())
            ),
            format!("body: {}", human_bytes(state.body_bytes_received as u64)),
            format!("redirects: {}", state.redirects.len()),
            format!("retries: {}", state.retries.len()),
        ]
        .join("\n"),
        1 => state
            .phases
            .iter()
            .map(|p| {
                format!(
                    "{} {:.1}ms +{:.1}ms",
                    p.label, p.duration_ms, p.cumulative_ms
                )
            })
            .collect::<Vec<_>>()
            .join("\n"),
        2 => {
            let mut lines = Vec::new();
            lines.push("[request headers]".to_string());
            lines.extend(headers_as_text(&state.request_headers, &state.filter_query));
            lines.push(String::new());
            lines.push("[response headers]".to_string());
            lines.extend(headers_as_text(
                &state.response_headers,
                &state.filter_query,
            ));
            lines.push(String::new());
            lines.push("[trailers]".to_string());
            if state.trailers.is_empty() {
                lines.push(if state.trailers_observable {
                    "none received".to_string()
                } else {
                    "not exposed by client yet".to_string()
                });
            } else {
                lines.extend(headers_as_text(&state.trailers, &state.filter_query));
            }
            lines.join("\n")
        }
        3 => state
            .body_lines
            .iter()
            .filter(|line| text_matches_filter(line, &state.filter_query))
            .cloned()
            .collect::<Vec<_>>()
            .join("\n"),
        4 => state
            .event_lines
            .iter()
            .filter(|line| text_matches_filter(&line.text, &state.filter_query))
            .map(|line| line.text.clone())
            .collect::<Vec<_>>()
            .join("\n"),
        5 => {
            let mut lines = vec![
                format!(
                    "result: {}",
                    if state.final_status_is_error {
                        "failure"
                    } else if state.done {
                        "success"
                    } else {
                        "running"
                    }
                ),
                format!("request: {} {}", state.method_label, state.target_label),
                format!("status: {}", state.status_label),
                format!("protocol: {}", value_or_dash(&state.protocol_label)),
                format!("remote: {}", value_or_dash(&state.remote_label)),
                format!(
                    "total: {}",
                    state
                        .total_duration_ms
                        .map(|ms| format!("{ms:.1}ms"))
                        .unwrap_or_else(|| "-".into())
                ),
                format!("body: {}", human_bytes(state.body_bytes_received as u64)),
                format!("trailers: {}", trailer_summary(state)),
            ];
            if !state.redirects.is_empty() {
                lines.push("redirects:".to_string());
                for (status, from, to) in &state.redirects {
                    lines.push(format!("  {status} {from} -> {to}"));
                }
            }
            if !state.retries.is_empty() {
                lines.push("retries:".to_string());
                lines.extend(state.retries.iter().map(|retry| format!("  {retry}")));
            }
            lines.join("\n")
        }
        _ => String::new(),
    }
}

fn headers_as_text(headers: &[(String, String)], filter_query: &str) -> Vec<String> {
    headers
        .iter()
        .filter(|(name, value)| header_matches_filter(name, value, filter_query))
        .map(|(name, value)| format!("{name}: {value}"))
        .collect()
}

// human_speed imported from crate::util

/// Split `line` into visual rows at `col_width` char boundaries.
/// Returns borrowed `Line`s to avoid allocations.
fn wrap_line(line: &str, col_width: usize) -> Vec<Line<'_>> {
    if line.is_empty() {
        return vec![Line::raw("")];
    }
    let mut result = Vec::new();
    let mut pos = 0;
    let bytes = line.as_bytes();
    while pos < bytes.len() {
        let mut end = (pos + col_width).min(bytes.len());
        while end > pos && !line.is_char_boundary(end) {
            end -= 1;
        }
        result.push(Line::raw(&line[pos..end]));
        pos = end;
    }
    result
}

fn render_body(f: &mut Frame, area: Rect, state: &mut TuiState) {
    let title = if state.body_is_sse {
        if state.body_done {
            "Body [SSE]"
        } else {
            "Body [SSE · streaming]"
        }
    } else if !state.body_done {
        "Body [streaming]"
    } else if state.body_cap_bytes > 64 * 1024 {
        "Body [truncated]"
    } else {
        "Body"
    };

    let block = Block::default().borders(Borders::ALL).title(title);
    let visible_rows = area.height.saturating_sub(2) as usize;
    let col_width = area.width.saturating_sub(2).max(1) as usize;
    state.body_col_width = col_width;
    state.body_visible_rows = visible_rows;

    // Pre-wrap: split each logical line into visual rows at col_width
    // char boundaries, using &str slices to avoid per-frame allocations.
    let visual_lines: Vec<Line> = state
        .body_lines
        .iter()
        .filter(|logical| text_matches_filter(logical, &state.filter_query))
        .flat_map(|logical| wrap_line(logical, col_width))
        .collect();

    let max_scroll = visual_lines.len().saturating_sub(visible_rows);
    let scroll_row = if state.body_auto_scroll {
        // Keep body_scroll synced so manual scroll (k/j) starts from here
        state.body_scroll = max_scroll;
        max_scroll
    } else {
        let row = state.body_scroll.min(max_scroll);
        if row >= max_scroll {
            state.body_auto_scroll = true;
        }
        state.body_scroll = row;
        row
    };
    let scroll = (scroll_row.min(u16::MAX as usize) as u16, 0);
    let paragraph = Paragraph::new(visual_lines).block(block).scroll(scroll);
    f.render_widget(paragraph, area);
}

fn render_footer(f: &mut Frame, area: Rect, state: &TuiState) {
    let quit_hint = if state.done { "q close" } else { "q cancel" };
    let hint = if state.editing_filter {
        format!("filter: {}_   Enter apply   Esc close", state.filter_query)
    } else {
        match state.active_page {
            0 => format!(
                "1-6 pages   Tab focus:{}   j/k scroll   / filter   y copy   {quit_hint}   ? help",
                http_focus_label(state)
            ),
            1 => format!(
                "1-6 pages   ←/→ span focus   h/l pages   j/k span   / filter   y copy   {quit_hint}   ? help"
            ),
            2 => format!(
                "1-6 pages   Tab column:{}   / filter   y copy   {quit_hint}   ? help",
                http_focus_label(state)
            ),
            3 => format!(
                "1-6 pages   j/k scroll body   Space autoscroll:{}   / filter   y copy   {quit_hint}   ? help",
                if state.body_auto_scroll { "on" } else { "off" }
            ),
            4 => format!(
                "1-6 pages   j/k scroll   g/G top/bottom   / filter   y copy visible   {quit_hint}   ? help"
            ),
            5 => format!("1-6 pages   y copy summary   {quit_hint}   ? help"),
            _ => format!("1-6 pages   {quit_hint}   ? help"),
        }
    };
    let paragraph = Paragraph::new(Line::styled(hint, Style::default().fg(Color::DarkGray)));
    f.render_widget(paragraph, area);
}

fn render_help(f: &mut Frame, area: Rect) {
    let help_text = vec![
        Line::from(Span::styled(
            "Keyboard Shortcuts",
            Style::default().fg(Color::Yellow),
        )),
        Line::raw(""),
        Line::raw("  q        Quit"),
        Line::raw("  Ctrl+C   Quit"),
        Line::raw("  Tab/←→   Move focus or selection"),
        Line::raw("  h/l      Previous / next page"),
        Line::raw("  1-6      Jump to page"),
        Line::raw("  /        Filter visible text"),
        Line::raw("  y        Copy visible page"),
        Line::raw("  Space    Toggle body autoscroll"),
        Line::raw("  ?        Toggle this help"),
        Line::raw("  Esc      Close help"),
        Line::raw("  j/↓      Scroll active page down"),
        Line::raw("  k/↑      Scroll active page up"),
        Line::raw("  PgDn     Scroll active page down"),
        Line::raw("  PgUp     Scroll active page up"),
        Line::raw("  g/Home   Scroll to top"),
        Line::raw("  G/End    Scroll to bottom"),
    ];

    let popup_area = centered_rect(50, 60, area);
    f.render_widget(Clear, popup_area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan).bg(Color::Black))
        .style(Style::default().bg(Color::Black))
        .title("Help [?]");
    let paragraph = Paragraph::new(help_text).block(block);
    f.render_widget(paragraph, popup_area);
}

fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(r);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup_layout[1])[1]
}

// human_bytes imported from crate::util

// duration_ms imported from crate::util

// find_split_point imported from crate::util

// truncate_chars imported from crate::util

// redact_headers imported from crate::util

#[cfg(test)]
mod tests {
    use super::*;
    use aioduct::observer::{Instant, NegotiatedProtocol, RetryKind};
    use http::{Method, StatusCode, Uri};
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};
    use std::time::Instant as StdInstant;

    fn make_event(phase: RequestPhase) -> RequestEvent {
        RequestEvent {
            method: Method::GET,
            uri: Uri::from_static("http://example.com"),
            phase,
            at: Instant::now(),
        }
    }

    fn key(code: KeyCode) -> crossterm::event::KeyEvent {
        crossterm::event::KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl_key(code: KeyCode) -> crossterm::event::KeyEvent {
        crossterm::event::KeyEvent::new(code, KeyModifiers::CONTROL)
    }

    #[test]
    fn state_tracks_phases() {
        let mut state = TuiState::new();
        state.apply(&make_event(RequestPhase::Started));
        state.apply(&make_event(RequestPhase::DnsResolved {
            addrs: vec![SocketAddr::new(IpAddr::V4(Ipv4Addr::new(1, 2, 3, 4)), 443)],
            duration: Duration::from_millis(10),
        }));
        assert_eq!(state.phases.len(), 2);
        assert!(!state.done);
    }

    #[test]
    fn state_marks_done_on_response_complete() {
        let mut state = TuiState::new();
        state.apply(&make_event(RequestPhase::ResponseComplete {
            status: StatusCode::OK,
            protocol: NegotiatedProtocol::Http2,
            total_duration: Duration::from_millis(200),
        }));
        assert!(state.done);
        assert!(state.status_line.contains("200"));
    }

    #[test]
    fn state_marks_done_on_failure() {
        let mut state = TuiState::new();
        state.apply(&make_event(RequestPhase::Failed {
            error: "timeout".into(),
            retry: RetryKind::None,
            elapsed: Duration::from_millis(5000),
        }));
        assert!(state.done);
        assert!(state.status_line.contains("timeout"));
    }

    #[test]
    fn request_headers_stored() {
        let mut state = TuiState::new();
        state.apply(&make_event(RequestPhase::RequestSent {
            duration: Duration::from_millis(2),
            headers: vec![
                ("host".into(), "example.com".into()),
                ("content-type".into(), "application/json".into()),
            ],
        }));
        assert_eq!(state.request_headers.len(), 2);
    }

    #[test]
    fn min_terminal_size_fallback() {
        let backend = ratatui::backend::TestBackend::new(20, 5);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        let mut state = TuiState::new();
        terminal.draw(|f| render(f, &mut state)).unwrap();
    }

    #[test]
    fn body_chunk_splits_lines() {
        let mut state = TuiState::new();
        state.apply_body_chunk("line1\nline2\n", StdInstant::now());
        assert_eq!(state.body_lines, vec!["line1", "line2"]);
        assert!(!state.body_done);
    }

    #[test]
    fn body_chunk_buffers_partial_line() {
        let mut state = TuiState::new();
        state.apply_body_chunk("hel", StdInstant::now());
        assert!(state.body_lines.is_empty());
        state.apply_body_chunk("lo\nworld\n", StdInstant::now());
        assert_eq!(state.body_lines, vec!["hello", "world"]);
    }

    #[test]
    fn body_done_flushes_partial_line() {
        let mut state = TuiState::new();
        state.apply_body_chunk("no trailing newline", StdInstant::now());
        assert!(state.body_lines.is_empty());
        state.apply_body_done();
        assert_eq!(state.body_lines, vec!["no trailing newline"]);
        assert!(state.body_done);
    }

    #[test]
    fn body_rolling_buffer_evicts_old_lines() {
        let mut state = TuiState::new();
        // Each line is ~1024 bytes → 70 lines ≈ 70KB > 64KB cap
        let line = "x".repeat(1024);
        for _ in 0..70 {
            state.apply_body_chunk(&format!("{line}\n"), StdInstant::now());
        }
        assert!(state.body_cap_bytes <= 64 * 1024 + 1024); // Allow slight overshoot from last line
        assert!(state.body_lines.len() < 70);
    }

    #[test]
    fn sse_detected_from_response_headers() {
        let headers = [
            ("content-type".into(), "text/event-stream".into()),
            ("transfer-encoding".into(), "chunked".into()),
        ];
        let mut state = TuiState::new();
        assert!(!state.body_is_sse);
        // Simulate what the event loop does
        state.body_is_sse = headers.iter().any(|(k, v): &(String, String)| {
            k.eq_ignore_ascii_case("content-type") && v.starts_with("text/event-stream")
        });
        assert!(state.body_is_sse);
    }

    #[test]
    fn sse_not_detected_for_plain_response() {
        let headers = [("content-type".into(), "application/json".into())];
        let mut state = TuiState::new();
        state.body_is_sse = headers.iter().any(|(k, v): &(String, String)| {
            k.eq_ignore_ascii_case("content-type") && v.starts_with("text/event-stream")
        });
        assert!(!state.body_is_sse);
    }

    #[test]
    fn scroll_down_moves_offset() {
        let mut state = TuiState::new();
        state.body_scroll = 0;
        state.body_auto_scroll = false;
        state.scroll_down(5);
        assert_eq!(state.body_scroll, 5);
        assert!(!state.body_auto_scroll);
    }

    #[test]
    fn scroll_up_clamps_at_zero() {
        let mut state = TuiState::new();
        state.body_scroll = 3;
        state.scroll_up(10);
        assert_eq!(state.body_scroll, 0);
    }

    #[test]
    fn scroll_to_bottom_enables_auto_scroll() {
        let mut state = TuiState::new();
        state.body_auto_scroll = false;
        state.scroll_to_bottom();
        assert!(state.body_auto_scroll);
    }

    #[test]
    fn help_toggle() {
        let mut state = TuiState::new();
        assert!(!state.show_help);
        state.show_help = !state.show_help;
        assert!(state.show_help);
    }

    #[test]
    fn esc_closes_help_without_quitting() {
        let mut state = TuiState::new();
        state.show_help = true;
        assert!(!handle_key_event(key(KeyCode::Esc), &mut state));
        assert!(!state.show_help);
    }

    #[test]
    fn esc_without_overlay_does_not_quit() {
        let mut state = TuiState::new();
        assert!(!handle_key_event(key(KeyCode::Esc), &mut state));
        assert!(!state.show_help);
    }

    #[test]
    fn q_quits_even_when_help_is_open() {
        let mut state = TuiState::new();
        state.show_help = true;
        assert!(handle_key_event(key(KeyCode::Char('q')), &mut state));
    }

    #[test]
    fn ctrl_c_quits() {
        let mut state = TuiState::new();
        assert!(handle_key_event(ctrl_key(KeyCode::Char('c')), &mut state));
    }

    #[test]
    fn tab_moves_focus_without_changing_page() {
        let mut state = TuiState::new();
        assert_eq!(state.active_page, 0);
        assert_eq!(state.focus_index, 0);
        assert!(!handle_key_event(key(KeyCode::Tab), &mut state));
        assert_eq!(state.active_page, 0);
        assert_eq!(state.focus_index, 1);
    }

    #[test]
    fn number_keys_jump_pages_and_reset_focus() {
        let mut state = TuiState::new();
        state.focus_index = 1;
        assert!(!handle_key_event(key(KeyCode::Char('4')), &mut state));
        assert_eq!(state.active_page, 3);
        assert_eq!(state.focus_index, 0);
    }

    #[test]
    fn slash_filter_captures_text_until_escape() {
        let mut state = TuiState::new();
        assert!(!handle_key_event(key(KeyCode::Char('/')), &mut state));
        assert!(state.editing_filter);
        assert!(!handle_key_event(key(KeyCode::Char('x')), &mut state));
        assert_eq!(state.filter_query, "x");
        assert!(!handle_key_event(key(KeyCode::Esc), &mut state));
        assert!(!state.editing_filter);
        assert_eq!(state.filter_query, "x");
    }

    #[test]
    fn space_toggles_body_autoscroll_on_body_page() {
        let mut state = TuiState::new();
        state.active_page = 3;
        assert!(state.body_auto_scroll);
        assert!(!handle_key_event(key(KeyCode::Char(' ')), &mut state));
        assert!(!state.body_auto_scroll);
    }

    #[test]
    fn multiple_body_chunks_accumulate() {
        let mut state = TuiState::new();
        state.apply_body_chunk("a\nb\nc\n", StdInstant::now());
        state.apply_body_chunk("d\ne\n", StdInstant::now());
        state.apply_body_done();
        assert_eq!(state.body_lines, vec!["a", "b", "c", "d", "e"]);
    }

    // ── sanitize_event_text tests ──

    #[test]
    fn sanitize_passes_plain_text() {
        assert_eq!(
            super::sanitize_event_text("hello world".into()),
            "hello world"
        );
    }

    #[test]
    fn sanitize_collapses_newlines() {
        assert_eq!(
            super::sanitize_event_text("line1\nline2\r\nline3".into()),
            "line1 line2 line3"
        );
    }

    #[test]
    fn sanitize_collapses_multiple_newlines() {
        assert_eq!(super::sanitize_event_text("a\n\nb".into()), "a b");
    }

    #[test]
    fn sanitize_removes_ansi_escapes() {
        assert_eq!(
            super::sanitize_event_text("ok \x1b[31mred\x1b[0m text".into()),
            "ok red text"
        );
    }

    #[test]
    fn sanitize_removes_ansi_escapes_with_sgr() {
        assert_eq!(
            super::sanitize_event_text("prefix \x1b[1;32mbold green\x1b[m suffix".into()),
            "prefix bold green suffix"
        );
    }

    #[test]
    fn sanitize_replaces_control_chars() {
        assert_eq!(
            super::sanitize_event_text("text\x00null\x08bs".into()),
            "text null bs"
        );
    }

    #[test]
    fn sanitize_preserves_tabs() {
        assert_eq!(
            super::sanitize_event_text("col1\tcol2".into()),
            "col1\tcol2"
        );
    }

    #[test]
    fn sanitize_empty_string() {
        assert_eq!(super::sanitize_event_text("".into()), "");
    }

    #[test]
    fn sanitize_error_message_with_newlines() {
        let text = "connection error:\n  caused by timeout\n  request id: abc123";
        let result = super::sanitize_event_text(text.into());
        assert!(!result.contains('\n'));
        assert!(result.contains("connection error:"));
    }

    #[test]
    fn log_event_sanitizes_text() {
        let mut state = TuiState::new();
        state.log_event("error\nwith\nnewlines", Color::Red);
        let last = state.event_lines.back().unwrap();
        assert!(!last.text.contains('\n'));
        assert_eq!(last.text, "error with newlines");
    }

    #[test]
    fn log_event_removes_ansi_from_error() {
        let mut state = TuiState::new();
        state.log_event("failed: \x1b[31mtimeout\x1b[0m after 5s", Color::Red);
        let last = state.event_lines.back().unwrap();
        assert!(!last.text.contains('\x1b'));
        assert_eq!(last.text, "failed: timeout after 5s");
    }
}
