use std::collections::VecDeque;
use std::io::{self, IsTerminal, stdout};
use std::sync::Arc;
use std::time::{Duration, Instant};

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
use ratatui::widgets::{
    Block, Borders, Clear, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState,
};
use tokio_util::sync::CancellationToken;

use super::file_entry::FileStatus;
use super::piece_grid::{HeatMapParams, render_heat_map, render_overview_bar, wrap_line};
use super::progress::format_size;
use super::scheduler::{FileSnapshot, GlobalScheduler};
use super::tui_state::{SharedEventLog, SharedWorkerStates, WorkerStatus};

pub struct MultiFileTui {
    cancel: CancellationToken,
    handle: Option<tokio::task::JoinHandle<()>>,
}

impl MultiFileTui {
    pub fn start(
        scheduler: Arc<GlobalScheduler>,
        worker_states: SharedWorkerStates,
        events: SharedEventLog,
        cancel: CancellationToken,
        num_workers: usize,
        total_files: usize,
    ) -> Self {
        if !stdout().is_terminal() {
            return Self {
                cancel: cancel.clone(),
                handle: None,
            };
        }

        let tui_cancel = cancel.child_token();
        let handle = tokio::spawn(run_multi_tui(
            scheduler,
            num_workers,
            total_files,
            worker_states,
            events,
            tui_cancel.clone(),
            cancel,
        ));

        Self {
            cancel: tui_cancel,
            handle: Some(handle),
        }
    }

    pub async fn stop(self) {
        self.cancel.cancel();
        if let Some(handle) = self.handle {
            let _ = handle.await;
        }
    }
}

struct AppState {
    active_tab: usize, // 0=Files, 1=Pieces, 2=Workers, 3=Stats
    selected_file: usize,
    files_scroll: u16,
    event_scroll: u16,
    show_help: bool,
    frame_count: u64,
    live_speed_bps: f64,
    prev_downloaded: u64,
    prev_download_time: Instant,
    speed_history: VecDeque<u64>,
    last_speed_sample: Instant,
}

impl AppState {
    fn new() -> Self {
        let now = Instant::now();
        Self {
            active_tab: 0,
            selected_file: 0,
            files_scroll: 0,
            event_scroll: 0,
            show_help: false,
            frame_count: 0,
            live_speed_bps: 0.0,
            prev_downloaded: 0,
            prev_download_time: now,
            speed_history: VecDeque::with_capacity(60),
            last_speed_sample: now,
        }
    }

    fn update_live_speed(&mut self, current_downloaded: u64) {
        let now = Instant::now();
        let dt = now.duration_since(self.prev_download_time).as_secs_f64();
        if dt >= 0.2 {
            let delta = current_downloaded.saturating_sub(self.prev_downloaded) as f64;
            let instant_speed = delta / dt;
            self.live_speed_bps = 0.6 * self.live_speed_bps + 0.4 * instant_speed;
            self.prev_downloaded = current_downloaded;
            self.prev_download_time = now;
        }
    }

    fn sample_speed(&mut self, speed: f64) {
        let now = Instant::now();
        if now.duration_since(self.last_speed_sample).as_secs() >= 1 {
            self.last_speed_sample = now;
            if self.speed_history.len() >= 60 {
                self.speed_history.pop_front();
            }
            self.speed_history.push_back(speed as u64);
        }
    }
}

struct MultiFrameData {
    files: Vec<FileSnapshot>,
    total_downloaded: u64,
    total_size: u64,
    completed_files: usize,
    total_files: usize,
    speed_bps: f64,
    num_workers: usize,
}

fn setup_terminal() -> io::Result<ratatui::Terminal<CrosstermBackend<io::Stdout>>> {
    enable_raw_mode()?;
    stdout().execute(EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout());
    ratatui::Terminal::new(backend)
}

fn restore_terminal() -> io::Result<()> {
    disable_raw_mode()?;
    stdout().execute(LeaveAlternateScreen)?;
    Ok(())
}

async fn run_multi_tui(
    scheduler: Arc<GlobalScheduler>,
    num_workers: usize,
    total_files: usize,
    worker_states: SharedWorkerStates,
    events: SharedEventLog,
    cancel: CancellationToken,
    parent_cancel: CancellationToken,
) {
    let mut terminal = match setup_terminal() {
        Ok(t) => t,
        Err(_) => return,
    };

    let mut app = AppState::new();
    if total_files == 1 {
        app.active_tab = 1;
    }

    loop {
        if cancel.is_cancelled() {
            break;
        }

        tokio::select! {
            _ = cancel.cancelled() => break,
            _ = tokio::time::sleep(Duration::from_millis(100)) => {}
        }

        // Handle input
        while let Some(action) = poll_input() {
            match action {
                InputAction::Quit => {
                    parent_cancel.cancel();
                    break;
                }
                InputAction::PrevTab => {
                    app.active_tab = (app.active_tab + 3) % 4;
                }
                InputAction::NextTab => {
                    app.active_tab = (app.active_tab + 1) % 4;
                }
                InputAction::Tab(n) => {
                    app.active_tab = n.min(3);
                }
                InputAction::ScrollUp => match app.active_tab {
                    0 => app.selected_file = app.selected_file.saturating_sub(1),
                    1 => app.files_scroll = app.files_scroll.saturating_sub(1),
                    3 => app.event_scroll = app.event_scroll.saturating_sub(1),
                    _ => {}
                },
                InputAction::ScrollDown => match app.active_tab {
                    0 => {
                        let total = scheduler.total_files();
                        if app.selected_file + 1 < total {
                            app.selected_file += 1;
                        }
                    }
                    1 => app.files_scroll = app.files_scroll.saturating_add(1),
                    3 => app.event_scroll = app.event_scroll.saturating_add(1),
                    _ => {}
                },
                InputAction::ScrollPageUp => match app.active_tab {
                    0 => app.selected_file = app.selected_file.saturating_sub(10),
                    1 => app.files_scroll = app.files_scroll.saturating_sub(10),
                    3 => app.event_scroll = app.event_scroll.saturating_sub(10),
                    _ => {}
                },
                InputAction::ScrollPageDown => match app.active_tab {
                    0 => {
                        let total = scheduler.total_files();
                        app.selected_file = (app.selected_file + 10).min(total.saturating_sub(1));
                    }
                    1 => app.files_scroll = app.files_scroll.saturating_add(10),
                    3 => app.event_scroll = app.event_scroll.saturating_add(10),
                    _ => {}
                },
                InputAction::ScrollTop => match app.active_tab {
                    0 => app.selected_file = 0,
                    1 => app.files_scroll = 0,
                    3 => app.event_scroll = 0,
                    _ => {}
                },
                InputAction::ScrollBottom => match app.active_tab {
                    0 => app.selected_file = scheduler.total_files().saturating_sub(1),
                    1 => app.files_scroll = u16::MAX,
                    3 => app.event_scroll = u16::MAX,
                    _ => {}
                },
                InputAction::ToggleHelp => {
                    app.show_help = !app.show_help;
                }
            }
        }

        // Build frame data
        let files = scheduler.snapshot_files();
        let total_downloaded: u64 = files
            .iter()
            .map(|f| f.completed_pieces as u64 * f.piece_length as u64)
            .sum();
        let total_size: u64 = files.iter().map(|f| f.total_size).sum();
        let completed_files = files
            .iter()
            .filter(|f| f.status == FileStatus::Complete)
            .count();

        // Add in-flight bytes
        let inflight_bytes: u64 = worker_states
            .lock()
            .unwrap()
            .iter()
            .filter(|w| w.current_piece.is_some())
            .map(|w| w.downloaded_bytes())
            .sum();
        let current_downloaded = (total_downloaded + inflight_bytes).min(total_size);

        app.update_live_speed(current_downloaded);

        let data = MultiFrameData {
            total_downloaded: current_downloaded,
            total_size,
            completed_files,
            total_files: files.len(),
            speed_bps: app.live_speed_bps,
            num_workers,
            files,
        };

        app.sample_speed(data.speed_bps);
        app.frame_count += 1;

        let _ = terminal.draw(|f| {
            draw_multi_ui(f, &data, &app, &scheduler, &worker_states, &events);
        });
    }

    let _ = terminal.clear();
    let _ = restore_terminal();
}

enum InputAction {
    Quit,
    PrevTab,
    NextTab,
    Tab(usize),
    ScrollUp,
    ScrollDown,
    ScrollPageUp,
    ScrollPageDown,
    ScrollTop,
    ScrollBottom,
    ToggleHelp,
}

fn poll_input() -> Option<InputAction> {
    if !event::poll(Duration::ZERO).unwrap_or(false) {
        return None;
    }
    if let Ok(Event::Key(key)) = event::read() {
        if key.kind != KeyEventKind::Press {
            return None;
        }
        return match key.code {
            KeyCode::Char('q') | KeyCode::Esc => Some(InputAction::Quit),
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                Some(InputAction::Quit)
            }
            KeyCode::Tab => Some(InputAction::NextTab),
            KeyCode::Left | KeyCode::Char('h') => Some(InputAction::PrevTab),
            KeyCode::Right | KeyCode::Char('l') => Some(InputAction::NextTab),
            KeyCode::Char('1') => Some(InputAction::Tab(0)),
            KeyCode::Char('2') => Some(InputAction::Tab(1)),
            KeyCode::Char('3') => Some(InputAction::Tab(2)),
            KeyCode::Char('4') => Some(InputAction::Tab(3)),
            KeyCode::Up | KeyCode::Char('k') => Some(InputAction::ScrollUp),
            KeyCode::Down | KeyCode::Char('j') => Some(InputAction::ScrollDown),
            KeyCode::PageUp => Some(InputAction::ScrollPageUp),
            KeyCode::PageDown => Some(InputAction::ScrollPageDown),
            KeyCode::Home | KeyCode::Char('g') => Some(InputAction::ScrollTop),
            KeyCode::End | KeyCode::Char('G') => Some(InputAction::ScrollBottom),
            KeyCode::Char('?') => Some(InputAction::ToggleHelp),
            _ => None,
        };
    }
    None
}

// ─── Drawing ────────────────────────────────────────────────────────────────

fn draw_multi_ui(
    f: &mut Frame,
    data: &MultiFrameData,
    app: &AppState,
    scheduler: &GlobalScheduler,
    worker_states: &SharedWorkerStates,
    events: &SharedEventLog,
) {
    let area = f.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2), // header
            Constraint::Min(4),    // main content
            Constraint::Length(1), // footer
        ])
        .split(area);

    draw_header(f, chunks[0], data, app);

    match app.active_tab {
        0 => draw_files_tab(f, chunks[1], data, app),
        1 => draw_pieces_tab(f, chunks[1], data, app, scheduler, worker_states),
        2 => draw_workers_tab(f, chunks[1], data, worker_states, app),
        3 => draw_stats_tab(f, chunks[1], app, events),
        _ => {}
    }

    draw_footer(f, chunks[2], data);

    if app.show_help {
        draw_help_overlay(f, area);
    }
}

fn draw_header(f: &mut Frame, area: Rect, data: &MultiFrameData, app: &AppState) {
    let pct = if data.total_size > 0 {
        data.total_downloaded as f64 / data.total_size as f64
    } else {
        0.0
    };

    let bar_width = 20usize;
    let filled = (bar_width as f64 * pct) as usize;
    let empty = bar_width - filled;
    let progress_bar = format!("{}{}", "\u{2501}".repeat(filled), "\u{2500}".repeat(empty),);

    let line1 = Line::from(vec![
        Span::styled(" aioduct-aria ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            format!("[{}/{}]", data.completed_files, data.total_files),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled("  ", Style::default()),
        Span::styled(
            format!(
                "{}/{}",
                format_size(data.total_downloaded),
                format_size(data.total_size)
            ),
            Style::default().fg(Color::Cyan),
        ),
        Span::styled(" ", Style::default()),
        Span::styled(&progress_bar, Style::default().fg(Color::Green)),
        Span::styled(
            format!(" {:.0}%", pct * 100.0),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
    ]);

    let tab_names = ["Files", "Pieces", "Workers", "Stats"];
    let mut tab_spans: Vec<Span> = vec![Span::styled(" ", Style::default())];
    for (i, name) in tab_names.iter().enumerate() {
        if i == app.active_tab {
            tab_spans.push(Span::styled(
                format!(" {name} "),
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ));
        } else {
            tab_spans.push(Span::styled(
                format!(" {name} "),
                Style::default().fg(Color::DarkGray),
            ));
        }
    }
    let line2 = Line::from(tab_spans);

    let text = vec![line1, line2];
    f.render_widget(Paragraph::new(text), area);
}

fn draw_footer(f: &mut Frame, area: Rect, data: &MultiFrameData) {
    let eta = if data.speed_bps > 0.0 && data.total_downloaded < data.total_size {
        let remaining = data.total_size - data.total_downloaded;
        let secs = remaining as f64 / data.speed_bps;
        format_eta(secs)
    } else if data.total_downloaded >= data.total_size && data.total_size > 0 {
        "done".to_string()
    } else {
        "---".to_string()
    };

    let line = Line::from(vec![
        Span::styled(" Speed: ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            format_speed(data.speed_bps),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" \u{2502} ETA: ", Style::default().fg(Color::DarkGray)),
        Span::styled(&eta, Style::default().fg(Color::Magenta)),
        Span::styled(" \u{2502} Workers: ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            format!("{}", data.num_workers),
            Style::default().fg(Color::White),
        ),
        Span::styled(" \u{2502} Files: ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            format!("{}/{} done", data.completed_files, data.total_files),
            Style::default().fg(Color::White),
        ),
        Span::styled(
            format!(
                " \u{2502} {:.0}%",
                if data.total_size > 0 {
                    data.total_downloaded as f64 / data.total_size as f64 * 100.0
                } else {
                    0.0
                }
            ),
            Style::default().fg(Color::DarkGray),
        ),
    ]);

    f.render_widget(Paragraph::new(line), area);
}

// ─── Files Tab ──────────────────────────────────────────────────────────────

fn draw_files_tab(f: &mut Frame, area: Rect, data: &MultiFrameData, app: &AppState) {
    if area.height == 0 || data.files.is_empty() {
        return;
    }

    let visible = area.height as usize;
    let total = data.files.len();

    // Scroll to keep selected visible
    let scroll = if app.selected_file >= visible {
        (app.selected_file - visible + 1) as u16
    } else {
        0
    };

    let mut lines: Vec<Line> = Vec::with_capacity(visible);

    for i in scroll as usize..(scroll as usize + visible).min(total) {
        let file = &data.files[i];
        let is_selected = i == app.selected_file;

        let indicator = if is_selected { "\u{25b6}" } else { " " };

        let status_icon = match file.status {
            FileStatus::Complete => Span::styled("\u{2713} ", Style::default().fg(Color::Green)),
            FileStatus::Active => Span::styled("\u{25cf} ", Style::default().fg(Color::Yellow)),
            FileStatus::Failed => Span::styled("\u{2717} ", Style::default().fg(Color::Red)),
            FileStatus::Pending => Span::styled("\u{25cb} ", Style::default().fg(Color::DarkGray)),
        };

        let name_style = match file.status {
            FileStatus::Complete => Style::default().fg(Color::Green),
            FileStatus::Active => Style::default().fg(Color::White),
            FileStatus::Failed => Style::default().fg(Color::Red),
            FileStatus::Pending => Style::default().fg(Color::DarkGray),
        };

        let downloaded = file.completed_pieces as u64 * file.piece_length as u64;
        let downloaded = downloaded.min(file.total_size);

        let pct = if file.total_size > 0 {
            downloaded as f64 / file.total_size as f64
        } else {
            0.0
        };

        // Progress bar (20 chars)
        let bar_w = 20usize;
        let filled = (bar_w as f64 * pct) as usize;
        let empty = bar_w - filled;
        let bar = format!("{}{}", "\u{2501}".repeat(filled), "\u{2500}".repeat(empty));

        let bar_color = match file.status {
            FileStatus::Complete => Color::Green,
            FileStatus::Active => Color::Yellow,
            _ => Color::DarkGray,
        };

        let name = truncate_str(&file.filename, 25);
        let size_str = format!(
            "{}/{}",
            format_size(downloaded),
            format_size(file.total_size)
        );

        let worker_str = if file.active_workers > 0 {
            format!(" W:{}", file.active_workers)
        } else {
            String::new()
        };

        let line = Line::from(vec![
            Span::styled(format!("{indicator} "), Style::default().fg(Color::Cyan)),
            status_icon,
            Span::styled(format!("{name:<25}"), name_style),
            Span::styled(
                format!(" {size_str:>15} "),
                Style::default().fg(Color::Cyan),
            ),
            Span::styled(format!("[{bar}]"), Style::default().fg(bar_color)),
            Span::styled(
                format!(" {:>3.0}%", pct * 100.0),
                Style::default().fg(Color::White),
            ),
            Span::styled(worker_str, Style::default().fg(Color::DarkGray)),
        ]);

        lines.push(line);
    }

    f.render_widget(Paragraph::new(lines), area);

    // Scrollbar
    if total > visible {
        let scrollbar_area = Rect {
            x: area.x + area.width - 1,
            y: area.y,
            width: 1,
            height: area.height,
        };
        let mut state = ScrollbarState::new(total).position(scroll as usize);
        f.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight),
            scrollbar_area,
            &mut state,
        );
    }
}

// ─── Pieces Tab ─────────────────────────────────────────────────────────────

fn draw_pieces_tab(
    f: &mut Frame,
    area: Rect,
    data: &MultiFrameData,
    app: &AppState,
    scheduler: &GlobalScheduler,
    worker_states: &SharedWorkerStates,
) {
    let file_id = data.files.get(app.selected_file).map(|f| f.id);
    let Some(file_id) = file_id else {
        let msg = Paragraph::new("No file selected");
        f.render_widget(msg, area);
        return;
    };

    let Some((pieces, piece_length)) = scheduler.snapshot_file_pieces(file_id) else {
        let msg = Paragraph::new("File not found");
        f.render_widget(msg, area);
        return;
    };

    let total_pieces = pieces.len() as u32;
    let params = HeatMapParams {
        pieces: &pieces,
        total_pieces,
        piece_length,
        scroll_offset: app.files_scroll,
        frame_count: app.frame_count,
    };

    let filename = &data.files[app.selected_file].filename;
    let completed = pieces
        .iter()
        .filter(|p| **p == super::piece_grid::PieceState::Complete)
        .count();
    let label = Line::from(vec![
        Span::styled(
            format!(" {} ", filename),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(" {}/{} pieces ", completed, total_pieces),
            Style::default().fg(Color::DarkGray),
        ),
    ]);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // file label
            Constraint::Length(1), // overview bar
            Constraint::Min(4),    // heat map grid
        ])
        .split(area);

    f.render_widget(Paragraph::new(label), chunks[0]);
    render_overview_bar(f, chunks[1], &params, worker_states);
    render_heat_map(f, chunks[2], &params, worker_states);
}

// ─── Workers Tab ────────────────────────────────────────────────────────────

fn draw_workers_tab(
    f: &mut Frame,
    area: Rect,
    data: &MultiFrameData,
    worker_states: &SharedWorkerStates,
    app: &AppState,
) {
    let workers = worker_states.lock().unwrap();
    let mut lines: Vec<Line> = Vec::new();

    // Header
    lines.push(Line::from(vec![Span::styled(
        format!(
            "{:<4} {:<14} {:<6} {:<22} {:<12} {:<4} {}",
            "ID", "File", "Piece", "Progress", "Speed", "Ret", "Status"
        ),
        Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::BOLD),
    )]));

    for w in workers.iter() {
        let id_str = format!("W/{}", w.id);
        let file_str = truncate_str(&w.file_name, 12).to_string();
        let piece_str = w
            .current_piece
            .map(|p| format!("#{p}"))
            .unwrap_or_else(|| "-".to_string());

        let (bar_str, pct_str) = if w.current_piece.is_some() {
            let ratio = if w.piece_length > 0 {
                (w.downloaded_bytes() as f64 / w.piece_length as f64).min(1.0)
            } else {
                0.0
            };
            let bar_w = 12;
            let filled = (bar_w as f64 * ratio) as usize;
            let empty = bar_w - filled;
            let bar = format!(
                "[{}{}]",
                "\u{2501}".repeat(filled),
                "\u{2500}".repeat(empty)
            );
            let pct = format!("{:>3.0}%", ratio * 100.0);
            (bar, pct)
        } else {
            ("              ".to_string(), "    ".to_string())
        };

        let speed_str = if w.status == WorkerStatus::Downloading {
            format_speed(app.live_speed_bps / data.num_workers.max(1) as f64)
        } else {
            "-".to_string()
        };

        let retries_str = if w.retries > 0 {
            format!("{}", w.retries)
        } else {
            "-".to_string()
        };

        let status_color = match w.status {
            WorkerStatus::Downloading => Color::Green,
            WorkerStatus::Retrying => Color::Yellow,
            WorkerStatus::Done => Color::DarkGray,
            WorkerStatus::Idle => Color::White,
        };

        let line = Line::from(vec![
            Span::styled(format!("{id_str:<4} "), Style::default().fg(Color::Cyan)),
            Span::styled(
                format!("{file_str:<14} "),
                Style::default().fg(Color::White),
            ),
            Span::styled(
                format!("{piece_str:<6} "),
                Style::default().fg(Color::Yellow),
            ),
            Span::styled(
                format!("{bar_str} {pct_str} "),
                Style::default().fg(Color::Green),
            ),
            Span::styled(
                format!("{speed_str:<12} "),
                Style::default().fg(Color::Cyan),
            ),
            Span::styled(
                format!("{retries_str:<4} "),
                Style::default().fg(Color::White),
            ),
            Span::styled(format!("{}", w.status), Style::default().fg(status_color)),
        ]);
        lines.push(line);
    }

    f.render_widget(Paragraph::new(lines), area);
}

// ─── Stats Tab ──────────────────────────────────────────────────────────────

fn draw_stats_tab(f: &mut Frame, area: Rect, app: &AppState, events: &SharedEventLog) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Event Log ")
        .border_style(Style::default().fg(Color::Yellow));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let events_lock = events.lock().unwrap();
    let raw_lines: Vec<String> = events_lock.iter().cloned().collect();
    drop(events_lock);

    let visible = inner.height as usize;
    let col_w = inner.width.max(1) as usize;

    let visual_rows: Vec<Line> = if raw_lines.is_empty() {
        vec![Line::styled(
            "No events yet.",
            Style::default().fg(Color::DarkGray),
        )]
    } else {
        raw_lines
            .iter()
            .flat_map(|s| wrap_line(s.as_str(), col_w))
            .collect()
    };
    let total = visual_rows.len();
    let max_scroll = total.saturating_sub(visible);
    let scroll = (app.event_scroll as usize).min(max_scroll);

    let visible_lines: Vec<Line> = visual_rows.into_iter().skip(scroll).take(visible).collect();
    f.render_widget(Paragraph::new(visible_lines), inner);

    if total > visible {
        let scrollbar_area = Rect {
            x: area.x + area.width - 1,
            y: area.y,
            width: 1,
            height: area.height,
        };
        let mut state = ScrollbarState::new(total).position(scroll);
        f.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight),
            scrollbar_area,
            &mut state,
        );
    }
}

// ─── Utilities ──────────────────────────────────────────────────────────────

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

fn draw_help_overlay(f: &mut Frame, area: Rect) {
    let width = 42u16;
    let height = 15u16;
    let x = area.width.saturating_sub(width) / 2;
    let y = area.height.saturating_sub(height) / 2;
    let popup_area = Rect::new(x, y, width.min(area.width), height.min(area.height));

    f.render_widget(Clear, popup_area);

    let lines = vec![
        Line::raw(""),
        Line::from(vec![
            Span::styled("  Tab", Style::default().fg(Color::Cyan)),
            Span::styled(" / ", Style::default().fg(Color::DarkGray)),
            Span::styled("1-4", Style::default().fg(Color::Cyan)),
            Span::raw("    Switch tabs"),
        ]),
        Line::from(vec![
            Span::styled("  ←→", Style::default().fg(Color::Cyan)),
            Span::styled(" / ", Style::default().fg(Color::DarkGray)),
            Span::styled("h l", Style::default().fg(Color::Cyan)),
            Span::raw("    Prev/Next tab"),
        ]),
        Line::from(vec![
            Span::styled("  ↑↓", Style::default().fg(Color::Cyan)),
            Span::styled(" / ", Style::default().fg(Color::DarkGray)),
            Span::styled("j k", Style::default().fg(Color::Cyan)),
            Span::raw("    Scroll"),
        ]),
        Line::from(vec![
            Span::styled("  PgUp", Style::default().fg(Color::Cyan)),
            Span::styled(" / ", Style::default().fg(Color::DarkGray)),
            Span::styled("PgDn", Style::default().fg(Color::Cyan)),
            Span::raw("  Page scroll"),
        ]),
        Line::from(vec![
            Span::styled("  Home", Style::default().fg(Color::Cyan)),
            Span::styled(" / ", Style::default().fg(Color::DarkGray)),
            Span::styled("End", Style::default().fg(Color::Cyan)),
            Span::raw("   Top / Bottom"),
        ]),
        Line::from(vec![
            Span::styled("  ?", Style::default().fg(Color::Cyan)),
            Span::raw("              Toggle help"),
        ]),
        Line::from(vec![
            Span::styled("  Esc", Style::default().fg(Color::Cyan)),
            Span::raw("            Close help"),
        ]),
        Line::from(vec![
            Span::styled("  q", Style::default().fg(Color::Cyan)),
            Span::styled(" / ", Style::default().fg(Color::DarkGray)),
            Span::styled("Ctrl+C", Style::default().fg(Color::Cyan)),
            Span::raw("  Quit & cancel"),
        ]),
    ];
    let block = ratatui::widgets::Block::default()
        .borders(ratatui::widgets::Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan).bg(Color::Black))
        .style(Style::default().bg(Color::Black))
        .title("Help [?]");
    let paragraph = Paragraph::new(lines).block(block);
    f.render_widget(paragraph, popup_area);
}

fn format_eta(secs: f64) -> String {
    if secs > 3600.0 {
        format!("{}h{:02}m", secs as u64 / 3600, (secs as u64 % 3600) / 60)
    } else if secs > 60.0 {
        format!("{}m{:02}s", secs as u64 / 60, secs as u64 % 60)
    } else {
        format!("{:.0}s", secs)
    }
}

fn truncate_str(s: &str, max: usize) -> &str {
    if s.len() <= max {
        s
    } else {
        let mut end = max;
        while end > 0 && !s.is_char_boundary(end) {
            end -= 1;
        }
        &s[..end]
    }
}
