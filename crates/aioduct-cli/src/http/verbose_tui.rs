use std::io::{self, IsTerminal, stdout};
use std::time::Duration;

use aioduct::observer::{RequestEvent, RequestPhase};

pub enum TuiMessage {
    ResponseHeaders(Vec<(String, String)>),
    Quit,
}
use crossterm::ExecutableCommand;
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Frame;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use tokio::sync::mpsc;

pub struct VerboseTui {
    handle: Option<tokio::task::JoinHandle<()>>,
    tx: Option<mpsc::UnboundedSender<TuiMessage>>,
}

impl VerboseTui {
    pub fn start(rx: mpsc::UnboundedReceiver<RequestEvent>) -> Self {
        if !stdout().is_terminal() {
            return Self {
                handle: None,
                tx: None,
            };
        }
        let (tx, tui_rx) = mpsc::unbounded_channel();
        let handle = tokio::spawn(run_tui(rx, tui_rx));
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

struct TuiState {
    phases: Vec<PhaseEntry>,
    request_headers: Vec<(String, String)>,
    response_headers: Vec<(String, String)>,
    status_line: String,
    body_lines: Vec<String>,
    show_response_headers: bool,
    done: bool,
}

struct PhaseEntry {
    label: String,
    duration_ms: f64,
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
            show_response_headers: false,
            done: false,
        }
    }

    fn apply(&mut self, event: &RequestEvent) {
        match &event.phase {
            RequestPhase::Started => {
                self.phases.push(PhaseEntry {
                    label: "START".into(),
                    duration_ms: 0.0,
                    color: Color::DarkGray,
                });
            }
            RequestPhase::DnsResolved { duration, addrs } => {
                let addr = addrs
                    .first()
                    .map(|a| a.ip().to_string())
                    .unwrap_or_default();
                self.phases.push(PhaseEntry {
                    label: format!("DNS  {addr}"),
                    duration_ms: ms(duration),
                    color: Color::Blue,
                });
            }
            RequestPhase::TcpConnected {
                duration,
                remote_addr,
                protocol,
            } => {
                self.status_line = format!("{remote_addr} | {protocol:?}");
                self.phases.push(PhaseEntry {
                    label: "TCP".into(),
                    duration_ms: ms(duration),
                    color: Color::Blue,
                });
            }
            RequestPhase::TlsHandshakeComplete {
                duration,
                alpn_protocol,
                ..
            } => {
                let alpn = alpn_protocol.as_deref().unwrap_or("");
                if !alpn.is_empty() {
                    self.status_line.push_str(&format!(" | ALPN={alpn}"));
                }
                self.phases.push(PhaseEntry {
                    label: "TLS".into(),
                    duration_ms: ms(duration),
                    color: Color::Cyan,
                });
            }
            RequestPhase::RequestSent { duration, headers } => {
                self.request_headers = headers.clone();
                self.phases.push(PhaseEntry {
                    label: "REQ".into(),
                    duration_ms: ms(duration),
                    color: Color::Yellow,
                });
            }
            RequestPhase::ResponseStarted { waiting_duration } => {
                self.phases.push(PhaseEntry {
                    label: "WAIT".into(),
                    duration_ms: ms(waiting_duration),
                    color: Color::Magenta,
                });
            }
            RequestPhase::ResponseComplete {
                status,
                protocol,
                total_duration,
            } => {
                let color = if status.is_success() {
                    Color::Green
                } else if status.is_redirection() {
                    Color::Yellow
                } else {
                    Color::Red
                };
                self.phases.push(PhaseEntry {
                    label: format!("RESP {status}"),
                    duration_ms: ms(total_duration),
                    color,
                });
                self.status_line = format!(
                    "{status} | {protocol:?} | {:.0}ms | {}",
                    ms(total_duration),
                    self.status_line
                );
                self.done = true;
            }
            RequestPhase::Failed { error, elapsed, .. } => {
                self.phases.push(PhaseEntry {
                    label: format!("FAIL {error}"),
                    duration_ms: ms(elapsed),
                    color: Color::Red,
                });
                self.status_line = format!("FAILED: {error}");
                self.done = true;
            }
            _ => {}
        }
    }
}

async fn run_tui(
    mut rx: mpsc::UnboundedReceiver<RequestEvent>,
    mut msg_rx: mpsc::UnboundedReceiver<TuiMessage>,
) {
    if let Err(e) = run_tui_inner(&mut rx, &mut msg_rx).await {
        eprintln!("TUI error: {e}");
    }
}

async fn run_tui_inner(
    rx: &mut mpsc::UnboundedReceiver<RequestEvent>,
    msg_rx: &mut mpsc::UnboundedReceiver<TuiMessage>,
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
        while let Ok(msg) = msg_rx.try_recv() {
            match msg {
                TuiMessage::ResponseHeaders(h) => state.response_headers = h,
                TuiMessage::Quit => {
                    state.done = true;
                    force_quit = true;
                }
            }
        }

        if event::poll(Duration::ZERO)?
            && let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
        {
            match key.code {
                KeyCode::Char('q') => break,
                KeyCode::Tab => {
                    state.show_response_headers = !state.show_response_headers;
                }
                _ => {}
            }
        }

        terminal.draw(|f| render(f, &state))?;

        if state.done {
            if force_quit {
                break;
            }
            // Wait for 'q' — no timeout, let the user inspect results
            loop {
                if event::poll(Duration::from_millis(100))?
                    && let Event::Key(key) = event::read()?
                    && key.kind == KeyEventKind::Press
                    && key.code == KeyCode::Char('q')
                {
                    break;
                }
                // Also check for Quit message (stop() called from main)
                if let Ok(TuiMessage::Quit) = msg_rx.try_recv() {
                    break;
                }
            }
            break;
        }

        interval.tick().await;
    }

    disable_raw_mode()?;
    stdout().execute(LeaveAlternateScreen)?;
    Ok(())
}

fn render(f: &mut Frame, state: &TuiState) {
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
        .constraints([Constraint::Length(30), Constraint::Min(30)])
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
    render_headers(f, headers_area, state);
    render_body(f, body_pane_area, state);
    render_status_bar(f, status_area, state);
}

fn render_timeline(f: &mut Frame, area: Rect, state: &TuiState) {
    let lines: Vec<Line> = state
        .phases
        .iter()
        .map(|p| {
            let timing = if p.duration_ms > 0.0 {
                format!("{:>6.1}ms", p.duration_ms)
            } else {
                String::new()
            };
            Line::from(vec![
                Span::styled("● ", Style::default().fg(p.color)),
                Span::styled(
                    format!("{:<10}", p.label.chars().take(10).collect::<String>()),
                    Style::default().fg(p.color),
                ),
                Span::raw(format!(" {timing}")),
            ])
        })
        .collect();

    let block = Block::default().borders(Borders::ALL).title("Timeline");
    let paragraph = Paragraph::new(lines).block(block);
    f.render_widget(paragraph, area);
}

fn render_headers(f: &mut Frame, area: Rect, state: &TuiState) {
    let (title, headers) = if state.show_response_headers {
        ("Response Headers [Tab]", &state.response_headers)
    } else {
        ("Request Headers [Tab]", &state.request_headers)
    };

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

fn render_body(f: &mut Frame, area: Rect, state: &TuiState) {
    let lines: Vec<Line> = state
        .body_lines
        .iter()
        .map(|l| Line::raw(l.as_str()))
        .collect();
    let block = Block::default().borders(Borders::ALL).title("Body");
    let paragraph = Paragraph::new(lines).block(block);
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

fn ms(d: &Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use aioduct::observer::{Instant, NegotiatedProtocol};
    use http::{Method, StatusCode, Uri};
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};

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
            will_retry: false,
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
        let state = TuiState::new();
        terminal.draw(|f| render(f, &state)).unwrap();
    }
}
