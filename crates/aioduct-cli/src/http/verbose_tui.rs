use std::collections::VecDeque;
use std::io::{self, IsTerminal, stdout};
use std::time::{Duration, Instant};

use aioduct::observer::{RequestEvent, RequestPhase, TransferDirection};

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
use ratatui::style::{Color, Style};
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
    pub async fn wait(self) {
        if let Some(handle) = self.handle {
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
    pub async fn stop(self) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(TuiMessage::Quit);
        }
        if let Some(handle) = self.handle {
            let _ = handle.await;
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum RightPanel {
    RequestHeaders,
    ResponseHeaders,
    Metrics,
}

impl RightPanel {
    fn next(self) -> Self {
        match self {
            RightPanel::RequestHeaders => RightPanel::ResponseHeaders,
            RightPanel::ResponseHeaders => RightPanel::Metrics,
            RightPanel::Metrics => RightPanel::RequestHeaders,
        }
    }
}

struct TuiState {
    phases: Vec<PhaseEntry>,
    request_headers: Vec<(String, String)>,
    response_headers: Vec<(String, String)>,
    status_line: String,
    body_lines: Vec<String>,
    body_cap_bytes: usize,
    body_scroll: usize,
    body_is_sse: bool,
    body_done: bool,
    body_auto_scroll: bool,
    body_partial: String,
    body_col_width: usize,
    body_visible_rows: usize,
    transfer_idx: Option<usize>,
    right_panel: RightPanel,
    show_help: bool,
    done: bool,
    phase_cumulative_ms: f64,
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

impl TuiState {
    fn new() -> Self {
        Self {
            phases: Vec::new(),
            request_headers: Vec::new(),
            response_headers: Vec::new(),
            status_line: String::new(),
            body_lines: Vec::new(),
            body_cap_bytes: 0,
            body_scroll: 0,
            body_is_sse: false,
            body_done: false,
            body_auto_scroll: true,
            body_partial: String::new(),
            body_col_width: 40,
            body_visible_rows: 20,
            transfer_idx: None,
            right_panel: RightPanel::RequestHeaders,
            show_help: false,
            done: false,
            phase_cumulative_ms: 0.0,
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
        match &event.phase {
            RequestPhase::Started => {
                self.phases.push(PhaseEntry {
                    label: "START".into(),
                    duration_ms: 0.0,
                    cumulative_ms: 0.0,
                    color: Color::DarkGray,
                });
            }
            RequestPhase::DnsResolved { duration, addrs } => {
                let d = ms(duration);
                self.phase_cumulative_ms += d;
                let addr = addrs
                    .first()
                    .map(|a| a.ip().to_string())
                    .unwrap_or_default();
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
                let d = ms(duration);
                self.phase_cumulative_ms += d;
                self.status_line = format!("{remote_addr} | {protocol:?}");
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
                let d = ms(duration);
                self.phase_cumulative_ms += d;
                let alpn = alpn_protocol.as_deref().unwrap_or("");
                if !alpn.is_empty() {
                    self.status_line.push_str(&format!(" | ALPN={alpn}"));
                }
                self.phases.push(PhaseEntry {
                    label: "TLS".into(),
                    duration_ms: d,
                    cumulative_ms: self.phase_cumulative_ms,
                    color: Color::Cyan,
                });
            }
            RequestPhase::RequestSent { duration, headers } => {
                self.request_headers = redact_headers(headers);
                let cumulative = ms(duration);
                let per_phase = (cumulative - self.phase_cumulative_ms).max(0.0);
                self.phase_cumulative_ms = cumulative;
                self.phases.push(PhaseEntry {
                    label: "REQ".into(),
                    duration_ms: per_phase,
                    cumulative_ms: self.phase_cumulative_ms,
                    color: Color::Yellow,
                });
            }
            RequestPhase::ResponseStarted { waiting_duration } => {
                let d = ms(waiting_duration);
                self.phase_cumulative_ms += d;
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
                let total = ms(total_duration);
                let per_phase = (total - self.phase_cumulative_ms).max(0.0);
                self.phase_cumulative_ms = total;
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
                self.done = true;
            }
            RequestPhase::Failed { error, elapsed, .. } => {
                let total = ms(elapsed);
                let per_phase = (total - self.phase_cumulative_ms).max(0.0);
                self.phases.push(PhaseEntry {
                    label: format!("FAIL {error}"),
                    duration_ms: per_phase,
                    cumulative_ms: total,
                    color: Color::Red,
                });
                self.status_line = format!("FAILED: {error}");
                self.done = true;
            }
            RequestPhase::PoolCheckoutComplete {
                outcome,
                blocked_duration,
            } => {
                let d = ms(blocked_duration);
                self.phase_cumulative_ms += d;
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
                let label = format!("{dir}  {}", format_bytes(*cumulative_bytes));
                let color = Color::Blue;
                if let Some(idx) = self.transfer_idx {
                    self.phases[idx].label = label;
                    self.phases[idx].duration_ms = ms(elapsed);
                } else {
                    self.transfer_start_at = Some(Instant::now());
                    self.transfer_idx = Some(self.phases.len());
                    self.phases.push(PhaseEntry {
                        label,
                        duration_ms: ms(elapsed),
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
                let dir = match direction {
                    TransferDirection::Download => "DOWN",
                    TransferDirection::Upload => "UP",
                };
                let label = format!(
                    "{dir}  {}  {:.0}B/s",
                    format_bytes(*total_bytes),
                    throughput_bytes_per_sec
                );
                self.phases.push(PhaseEntry {
                    label,
                    duration_ms: ms(transfer_duration),
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
                    label: format!("{dir}  {}  ERR {error}", format_bytes(*bytes_transferred)),
                    duration_ms: ms(elapsed),
                    cumulative_ms: 0.0,
                    color: Color::Red,
                });
            }
            RequestPhase::Redirected { status, from, to } => {
                let _ = (from, to);
                self.phases.push(PhaseEntry {
                    label: format!("REDIR {status}"),
                    duration_ms: 0.0,
                    cumulative_ms: self.phase_cumulative_ms,
                    color: Color::Yellow,
                });
            }
            RequestPhase::Retrying {
                reason,
                attempt,
                backoff,
                ..
            } => {
                self.phases.push(PhaseEntry {
                    label: format!("RETRY #{attempt} {}", reason),
                    duration_ms: ms(backoff),
                    cumulative_ms: self.phase_cumulative_ms,
                    color: Color::Yellow,
                });
            }
        }
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
        self.body_scroll = self.body_scroll.saturating_add(lines);
        self.body_auto_scroll = false;
    }

    fn scroll_up(&mut self, lines: usize) {
        self.body_scroll = self.body_scroll.saturating_sub(lines);
        self.body_auto_scroll = false;
    }

    fn scroll_to_bottom(&mut self) {
        self.body_auto_scroll = true;
        self.body_scroll = usize::MAX; // prevent stale jump when user presses j after G
    }

    fn scroll_to_top(&mut self) {
        self.body_scroll = 0;
        self.body_auto_scroll = false;
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
    match key.code {
        KeyCode::Char('q') => return true,
        KeyCode::Tab => {
            state.right_panel = state.right_panel.next();
        }
        KeyCode::Char('j') | KeyCode::Down => state.scroll_down(1),
        KeyCode::Char('k') | KeyCode::Up => state.scroll_up(1),
        KeyCode::PageDown => state.scroll_down(state.body_visible_rows),
        KeyCode::PageUp => state.scroll_up(state.body_visible_rows),
        KeyCode::Char('g') | KeyCode::Home => state.scroll_to_top(),
        KeyCode::Char('G') | KeyCode::End => state.scroll_to_bottom(),
        KeyCode::Char('?') => state.show_help = !state.show_help,
        _ => {}
    }
    false
}

fn render(f: &mut Frame, state: &mut TuiState) {
    let size = f.area();
    if size.width < 40 || size.height < 10 {
        let msg = Paragraph::new("Terminal too small");
        f.render_widget(msg, size);
        return;
    }

    let main_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(5), Constraint::Length(1)])
        .split(size);

    let body_area = main_layout[0];
    let status_area = main_layout[1];

    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(46), Constraint::Min(30)])
        .split(body_area);

    let timeline_area = columns[0];
    let right_area = columns[1];

    let right_split = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(40), Constraint::Percentage(60)])
        .split(right_area);

    let headers_area = right_split[0];
    let body_pane_area = right_split[1];

    render_timeline(f, timeline_area, state);
    render_right_panel(f, headers_area, state);
    render_body(f, body_pane_area, state);
    render_status_bar(f, status_area, state);

    if state.show_help {
        render_help(f, size);
    }
}

fn render_timeline(f: &mut Frame, area: Rect, state: &TuiState) {
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

    let block = Block::default().borders(Borders::ALL).title("Timeline");
    let paragraph = Paragraph::new(lines).block(block);
    f.render_widget(paragraph, area);
}

fn render_right_panel(f: &mut Frame, area: Rect, state: &TuiState) {
    match state.right_panel {
        RightPanel::RequestHeaders => {
            render_headers_panel(f, area, "Request Headers [Tab]", &state.request_headers)
        }
        RightPanel::ResponseHeaders => {
            render_headers_panel(f, area, "Response Headers [Tab]", &state.response_headers)
        }
        RightPanel::Metrics => render_metrics(f, area, state),
    }
}

fn render_headers_panel(f: &mut Frame, area: Rect, title: &str, headers: &[(String, String)]) {
    let lines: Vec<Line> = headers
        .iter()
        .map(|(k, v)| {
            Line::from(vec![
                Span::styled(format!("{k}: "), Style::default().fg(Color::Cyan)),
                Span::raw(v.as_str()),
            ])
        })
        .collect();

    let block = Block::default().borders(Borders::ALL).title(title);
    let paragraph = Paragraph::new(lines).block(block);
    f.render_widget(paragraph, area);
}

fn render_metrics(f: &mut Frame, area: Rect, state: &TuiState) {
    let mut lines: Vec<Line> = Vec::new();

    // ── Transfer stats ──
    lines.push(Line::styled(
        "── Transfer ──",
        Style::default().fg(Color::Cyan),
    ));
    lines.push(Line::from(vec![
        Span::raw("  Body: "),
        Span::styled(
            format_bytes(state.body_bytes_received as u64),
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
                Span::styled(format_speed(bps), Style::default().fg(Color::Yellow)),
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

    let block = Block::default()
        .borders(Borders::ALL)
        .title("Metrics [Tab]");
    let paragraph = Paragraph::new(lines).block(block);
    f.render_widget(paragraph, area);
}

fn format_speed(bps: f64) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = KIB * 1024.0;

    if bps >= MIB {
        format!("{:.1} MiB/s", bps / MIB)
    } else if bps >= KIB {
        format!("{:.1} KiB/s", bps / KIB)
    } else {
        format!("{:.0} B/s", bps)
    }
}

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

fn render_status_bar(f: &mut Frame, area: Rect, state: &TuiState) {
    let style = if state.status_line.contains("FAIL") {
        Style::default().fg(Color::Red)
    } else if state.done {
        Style::default().fg(Color::Green)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let line = if state.status_line.is_empty() {
        "Connecting..."
    } else {
        &state.status_line
    };
    let paragraph = Paragraph::new(Line::styled(line, style));
    f.render_widget(paragraph, area);
}

fn render_help(f: &mut Frame, area: Rect) {
    let help_text = vec![
        Line::from(Span::styled(
            "Keyboard Shortcuts",
            Style::default().fg(Color::Yellow),
        )),
        Line::raw(""),
        Line::raw("  q/Ctrl+C Quit"),
        Line::raw("  Tab      Cycle: Headers / Metrics"),
        Line::raw("  ?        Toggle this help"),
        Line::raw("  j/↓      Scroll body down one line"),
        Line::raw("  k/↑      Scroll body up one line"),
        Line::raw("  PgDn     Scroll body down one page"),
        Line::raw("  PgUp     Scroll body up one page"),
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

fn format_bytes(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes}B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1}KB", bytes as f64 / 1024.0)
    } else if bytes < 1024 * 1024 * 1024 {
        format!("{:.1}MB", bytes as f64 / (1024.0 * 1024.0))
    } else {
        format!("{:.2}GB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
    }
}

fn ms(d: &Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

fn find_split_point(s: &str, target: usize) -> usize {
    if s.is_char_boundary(target) {
        return target;
    }
    // Walk back to nearest char boundary
    (0..target)
        .rev()
        .find(|&i| s.is_char_boundary(i))
        .unwrap_or(target)
}

fn redact_headers(headers: &[(String, String)]) -> Vec<(String, String)> {
    headers
        .iter()
        .map(|(k, v)| {
            let lower = k.to_lowercase();
            let value = if lower == "authorization"
                || lower == "proxy-authorization"
                || lower == "cookie"
                || lower == "set-cookie"
            {
                "***".to_string()
            } else {
                v.clone()
            };
            (k.clone(), value)
        })
        .collect()
}

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
    fn multiple_body_chunks_accumulate() {
        let mut state = TuiState::new();
        state.apply_body_chunk("a\nb\nc\n", StdInstant::now());
        state.apply_body_chunk("d\ne\n", StdInstant::now());
        state.apply_body_done();
        assert_eq!(state.body_lines, vec!["a", "b", "c", "d", "e"]);
    }
}
