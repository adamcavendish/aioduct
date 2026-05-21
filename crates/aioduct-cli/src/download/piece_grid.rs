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
use ratatui::layout::{Constraint, Direction, Layout, Margin, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Borders, Clear, Paragraph, Row, Scrollbar, ScrollbarOrientation, ScrollbarState,
    Sparkline, Table, Wrap,
};
use tokio_util::sync::CancellationToken;

use super::piece::storage::PieceStorage;
use super::segment_man::SegmentMan;
use super::tui_state::{SharedEventLog, SharedWorkerStates, WorkerStatus};

pub struct PieceGrid {
    cancel: CancellationToken,
    handle: Option<tokio::task::JoinHandle<()>>,
}

impl PieceGrid {
    #[allow(clippy::too_many_arguments)]
    pub fn start(
        segment_man: Arc<SegmentMan>,
        total_length: u64,
        filename: String,
        num_workers: usize,
        worker_states: SharedWorkerStates,
        events: SharedEventLog,
        cancel: CancellationToken,
    ) -> Self {
        if !stdout().is_terminal() {
            return Self {
                cancel: cancel.clone(),
                handle: None,
            };
        }

        let grid_cancel = cancel.child_token();
        let handle = tokio::spawn(run_tui(
            segment_man,
            total_length,
            filename,
            num_workers,
            worker_states,
            events,
            grid_cancel.clone(),
            cancel,
        ));

        Self {
            cancel: grid_cancel,
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
    active_tab: usize,
    scroll_offset: u16,
    event_scroll: u16,
    show_help: bool,
    speed_history: VecDeque<u64>,
    last_speed_sample: Instant,
    frame_count: u64,
    // Live speed tracking: computed from worker byte deltas between frames
    live_speed_bps: f64,
    prev_downloaded: u64,
    prev_download_time: Instant,
    // Per-worker live speed tracking
    worker_prev_bytes: Vec<u64>,
    worker_live_speed: Vec<f64>,
    worker_speed_time: Instant,
}

impl AppState {
    fn new() -> Self {
        let now = Instant::now();
        Self {
            active_tab: 0,
            scroll_offset: 0,
            event_scroll: 0,
            show_help: false,
            speed_history: VecDeque::with_capacity(60),
            last_speed_sample: now,
            frame_count: 0,
            live_speed_bps: 0.0,
            prev_downloaded: 0,
            prev_download_time: now,
            worker_prev_bytes: Vec::new(),
            worker_live_speed: Vec::new(),
            worker_speed_time: now,
        }
    }

    fn update_live_speed(&mut self, current_downloaded: u64) {
        let elapsed = self.prev_download_time.elapsed();
        if elapsed >= Duration::from_millis(200) {
            let delta = current_downloaded.saturating_sub(self.prev_downloaded);
            let bps = delta as f64 / elapsed.as_secs_f64();
            self.live_speed_bps = self.live_speed_bps * 0.6 + bps * 0.4;
            self.prev_downloaded = current_downloaded;
            self.prev_download_time = Instant::now();
        }
    }

    fn update_worker_speeds(&mut self, worker_states: &SharedWorkerStates) {
        let elapsed = self.worker_speed_time.elapsed();
        if elapsed < Duration::from_millis(500) {
            return;
        }

        let states = worker_states.lock().unwrap();
        let n = states.len();

        if self.worker_prev_bytes.len() != n {
            self.worker_prev_bytes = vec![0; n];
            self.worker_live_speed = vec![0.0; n];
        }

        let dt = elapsed.as_secs_f64();
        for (i, ws) in states.iter().enumerate() {
            let current = ws.downloaded_bytes();
            if ws.current_piece.is_some() {
                let delta = current.saturating_sub(self.worker_prev_bytes[i]);
                let bps = delta as f64 / dt;
                self.worker_live_speed[i] = self.worker_live_speed[i] * 0.5 + bps * 0.5;
            } else {
                self.worker_live_speed[i] = 0.0;
            }
            self.worker_prev_bytes[i] = current;
        }

        self.worker_speed_time = Instant::now();
    }

    fn worker_speed(&self, worker_id: usize) -> f64 {
        self.worker_live_speed
            .get(worker_id)
            .copied()
            .unwrap_or(0.0)
    }

    fn sample_speed(&mut self, bps: f64) {
        if self.last_speed_sample.elapsed() >= Duration::from_secs(1) {
            if self.speed_history.len() >= 60 {
                self.speed_history.pop_front();
            }
            self.speed_history.push_back(bps as u64);
            self.last_speed_sample = Instant::now();
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
pub enum PieceState {
    Pending,
    InFlight,
    Complete,
}

struct FrameData {
    total_pieces: u32,
    completed: u32,
    remaining: u32,
    piece_length: u32,
    total_length: u64,
    downloaded_bytes: u64,
    speed_bps: f64,
    filename: String,
    num_workers: usize,
    pieces: Vec<PieceState>,
}

#[allow(clippy::too_many_arguments)]
async fn run_tui(
    segment_man: Arc<SegmentMan>,
    total_length: u64,
    filename: String,
    num_workers: usize,
    worker_states: SharedWorkerStates,
    events: SharedEventLog,
    cancel: CancellationToken,
    parent_cancel: CancellationToken,
) {
    if setup_terminal().is_err() {
        return;
    }

    let backend = CrosstermBackend::new(stdout());
    let mut terminal = match ratatui::Terminal::new(backend) {
        Ok(t) => t,
        Err(_) => {
            let _ = restore_terminal();
            return;
        }
    };

    let mut app = AppState::new();
    let mut interval = tokio::time::interval(Duration::from_millis(100));

    loop {
        tokio::select! {
            _ = interval.tick() => {}
            _ = cancel.cancelled() => break,
        }

        if let Some(action) = poll_input() {
            match action {
                InputAction::Quit => {
                    parent_cancel.cancel();
                    break;
                }
                InputAction::ToggleHelp => {
                    app.show_help = !app.show_help;
                }
                InputAction::NextTab => {
                    app.active_tab = (app.active_tab + 1) % 3;
                }
                InputAction::Tab(n) => {
                    app.active_tab = n.min(2);
                }
                InputAction::ScrollUp => match app.active_tab {
                    0 => app.scroll_offset = app.scroll_offset.saturating_sub(1),
                    2 => app.event_scroll = app.event_scroll.saturating_sub(1),
                    _ => {}
                },
                InputAction::ScrollDown => match app.active_tab {
                    0 => app.scroll_offset = app.scroll_offset.saturating_add(1),
                    2 => app.event_scroll = app.event_scroll.saturating_add(1),
                    _ => {}
                },
            }
        }

        let mut data = segment_man.snapshot_storage(|storage| FrameData {
            total_pieces: storage.total_pieces(),
            completed: storage.completed_count(),
            remaining: storage.remaining_pieces(),
            piece_length: storage.piece_length(),
            total_length,
            downloaded_bytes: 0,
            speed_bps: 0.0,
            filename: filename.clone(),
            num_workers,
            pieces: collect_piece_states(storage),
        });

        // Compute live speed from worker byte progress (instant feedback)
        let current_downloaded = {
            let completed_bytes = data.completed as u64 * data.piece_length as u64;
            let inflight_bytes: u64 = worker_states
                .lock()
                .unwrap()
                .iter()
                .filter(|w| w.current_piece.is_some())
                .map(|w| w.downloaded_bytes())
                .sum();
            (completed_bytes + inflight_bytes).min(total_length)
        };
        data.downloaded_bytes = current_downloaded;
        app.update_live_speed(current_downloaded);
        app.update_worker_speeds(&worker_states);

        // Always use live speed (smooth byte-delta EMA) for the UI.
        // SpeedMonitor records entire pieces at completion, causing spikes.
        data.speed_bps = app.live_speed_bps;

        app.sample_speed(data.speed_bps);
        app.frame_count += 1;

        let _ = terminal.draw(|f| {
            draw_ui(f, &data, &app, &worker_states, &events);
        });
    }

    let _ = terminal.clear();
    let _ = restore_terminal();
}

enum InputAction {
    Quit,
    ToggleHelp,
    NextTab,
    Tab(usize),
    ScrollUp,
    ScrollDown,
}

fn poll_input() -> Option<InputAction> {
    if !event::poll(Duration::ZERO).unwrap_or(false) {
        return None;
    }
    let Ok(Event::Key(key)) = event::read() else {
        return None;
    };
    if key.kind != KeyEventKind::Press {
        return None;
    }
    match key.code {
        KeyCode::Char('q') => Some(InputAction::Quit),
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            Some(InputAction::Quit)
        }
        KeyCode::Char('?') => Some(InputAction::ToggleHelp),
        KeyCode::Esc => Some(InputAction::ToggleHelp),
        KeyCode::Tab => Some(InputAction::NextTab),
        KeyCode::Char('1') => Some(InputAction::Tab(0)),
        KeyCode::Char('2') => Some(InputAction::Tab(1)),
        KeyCode::Char('3') => Some(InputAction::Tab(2)),
        KeyCode::Up | KeyCode::Char('k') => Some(InputAction::ScrollUp),
        KeyCode::Down | KeyCode::Char('j') => Some(InputAction::ScrollDown),
        _ => None,
    }
}

fn setup_terminal() -> io::Result<()> {
    enable_raw_mode()?;
    stdout().execute(EnterAlternateScreen)?;
    Ok(())
}

fn restore_terminal() -> io::Result<()> {
    stdout().execute(LeaveAlternateScreen)?;
    disable_raw_mode()?;
    Ok(())
}

pub fn collect_piece_states(storage: &PieceStorage) -> Vec<PieceState> {
    let total = storage.total_pieces();
    (0..total)
        .map(|i| {
            if storage.is_complete(i) {
                PieceState::Complete
            } else if storage.is_in_flight(i) {
                PieceState::InFlight
            } else {
                PieceState::Pending
            }
        })
        .collect()
}

pub struct HeatMapParams<'a> {
    pub pieces: &'a [PieceState],
    pub total_pieces: u32,
    pub piece_length: u32,
    pub scroll_offset: u16,
    pub frame_count: u64,
}

pub fn render_overview_bar(
    f: &mut Frame,
    area: Rect,
    params: &HeatMapParams,
    worker_states: &SharedWorkerStates,
) {
    if area.width == 0 || params.total_pieces == 0 {
        return;
    }

    let workers = worker_states.lock().unwrap();
    let width = area.width as u32;
    let mut spans: Vec<Span> = Vec::with_capacity(width as usize);

    for col in 0..width {
        let start = (col as u64 * params.total_pieces as u64) / width as u64;
        let end = ((col as u64 + 1) * params.total_pieces as u64) / width as u64;

        let mut complete = 0u32;
        let mut inflight = 0u32;
        let mut total = 0u32;
        let mut max_fill: f64 = 0.0;

        for idx in start..end {
            if (idx as u32) < params.total_pieces {
                total += 1;
                match params.pieces[idx as usize] {
                    PieceState::Complete => complete += 1,
                    PieceState::InFlight => {
                        inflight += 1;
                        for w in workers.iter() {
                            if w.current_piece == Some(idx as u32) {
                                let ratio =
                                    (w.downloaded_bytes() as f64 / w.piece_length as f64).min(1.0);
                                max_fill = max_fill.max(ratio);
                            }
                        }
                    }
                    PieceState::Pending => {}
                }
            }
        }

        let color = if total == 0 {
            Color::Rgb(45, 50, 65)
        } else if complete == total {
            Color::Rgb(40, 200, 80)
        } else if inflight > 0 {
            let g = 140 + (60.0 * max_fill) as u8;
            Color::Rgb(220, g, 0)
        } else if complete > 0 {
            Color::Rgb(0, 128, 0)
        } else {
            Color::Rgb(45, 50, 65)
        };

        spans.push(Span::styled("▮", Style::default().fg(color)));
    }

    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

pub fn render_heat_map(
    f: &mut Frame,
    area: Rect,
    params: &HeatMapParams,
    worker_states: &SharedWorkerStates,
) {
    if area.width == 0 || area.height == 0 || params.total_pieces == 0 {
        return;
    }

    let total = params.total_pieces as usize;

    let margin_left: u16 = 6;
    let usable_w = area.width.saturating_sub(margin_left) as usize;
    let usable_h = area.height as usize;

    let (cell_w, cell_h, cols) = compute_cell_size(total, usable_w, usable_h);
    let gap_y: u16 = if cell_h > 2 { 1 } else { 0 };
    let stride_y = cell_h + gap_y as usize;
    let rows_needed = total.div_ceil(cols);

    let visible_grid_rows = usable_h.checked_div(stride_y).unwrap_or(1);
    let max_scroll = rows_needed.saturating_sub(visible_grid_rows) as u16;
    let scroll = params.scroll_offset.min(max_scroll);

    let states = worker_states.lock().unwrap();
    let buf = f.buffer_mut();

    for grid_row in 0..visible_grid_rows {
        let actual_row = grid_row + scroll as usize;
        if actual_row >= rows_needed {
            break;
        }

        let byte_offset = actual_row as u64 * cols as u64 * params.piece_length as u64;
        let label = format_offset_label(byte_offset);
        let label_y = area.y + (grid_row * stride_y) as u16;
        if label_y < area.y + area.height {
            let label_style = Style::default().fg(Color::DarkGray);
            for (i, ch) in label.chars().enumerate() {
                let x = area.x + i as u16;
                if x < area.x + margin_left
                    && let Some(cell) = buf.cell_mut((x, label_y))
                {
                    cell.set_char(ch);
                    cell.set_style(label_style);
                }
            }
        }

        for col in 0..cols {
            let piece_idx = actual_row * cols + col;
            if piece_idx >= total {
                break;
            }

            let state = params.pieces[piece_idx];

            let fill_ratio = match state {
                PieceState::Complete => 1.0,
                PieceState::InFlight => states
                    .iter()
                    .find(|w| w.current_piece == Some(piece_idx as u32))
                    .map(|w| {
                        if w.piece_length > 0 {
                            (w.downloaded_bytes() as f64 / w.piece_length as f64).min(1.0)
                        } else {
                            0.0
                        }
                    })
                    .unwrap_or(0.0),
                PieceState::Pending => 0.0,
            };

            let bg_color = progress_color(fill_ratio, state, params.frame_count, piece_idx);
            let border_color = dim_color(bg_color, 0.4);

            let cell_x = area.x + margin_left + (col * cell_w) as u16;
            let cell_y = area.y + (grid_row * stride_y) as u16;

            for dy in 0..cell_h as u16 {
                for dx in 0..cell_w as u16 {
                    let x = cell_x + dx;
                    let y = cell_y + dy;
                    if x < area.x + area.width
                        && y < area.y + area.height
                        && let Some(cell) = buf.cell_mut((x, y))
                    {
                        let is_border = dx == cell_w as u16 - 1 && cell_w > 2;
                        let color = if is_border { border_color } else { bg_color };
                        cell.set_char(' ');
                        cell.set_style(Style::default().bg(color));
                    }
                }
            }
        }
    }

    if rows_needed > visible_grid_rows {
        let scroll_area = Rect {
            x: area.x + area.width - 1,
            y: area.y,
            width: 1,
            height: area.height,
        };
        let mut scrollbar_state = ScrollbarState::new(rows_needed).position(scroll as usize);
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight);
        f.render_stateful_widget(scrollbar, scroll_area, &mut scrollbar_state);
    }
}

// ─── Main UI Layout ─────────────────────────────────────────────────────────

fn draw_ui(
    f: &mut Frame,
    data: &FrameData,
    app: &AppState,
    worker_states: &SharedWorkerStates,
    events: &SharedEventLog,
) {
    let area = f.area();

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(4), // header (title + tabs)
            Constraint::Min(5),    // tab content
            Constraint::Length(1), // footer status line
        ])
        .split(area);

    draw_header(f, chunks[0], data, app);

    match app.active_tab {
        0 => draw_pieces_tab(f, chunks[1], data, app, worker_states),
        1 => draw_workers_tab(f, chunks[1], data, app, worker_states),
        2 => draw_stats_tab(f, chunks[1], app, events),
        _ => {}
    }

    draw_footer(f, chunks[2], data);

    if app.show_help {
        draw_help_overlay(f, area);
    }
}

// ─── Header ─────────────────────────────────────────────────────────────────

fn draw_header(f: &mut Frame, area: Rect, data: &FrameData, app: &AppState) {
    let downloaded = data.downloaded_bytes;
    let pct = if data.total_length > 0 {
        downloaded as f64 / data.total_length as f64
    } else {
        0.0
    };

    // Line 1: app name + filename + size + inline progress + percent + help hint
    let progress_bar_width = 20usize;
    let filled = (progress_bar_width as f64 * pct) as usize;
    let empty = progress_bar_width - filled;
    let progress_bar = format!("{}╸{}", "━".repeat(filled), "━".repeat(empty));

    let line1 = Line::from(vec![
        Span::styled(" aioduct-aria ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            &data.filename,
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled("  ", Style::default()),
        Span::styled(
            format!(
                "{}/{}",
                format_size_compact(downloaded),
                format_size_compact(data.total_length)
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
        Span::styled("   ?:help", Style::default().fg(Color::DarkGray)),
    ]);

    // Line 2: separator
    let sep = "─".repeat(area.width as usize);
    let line2 = Line::styled(&sep, Style::default().fg(Color::Rgb(60, 60, 60)));

    // Line 3: tabs
    let tab_names = ["Pieces", "Workers", "Stats"];
    let mut tab_spans: Vec<Span> = vec![Span::raw("  ")];
    for (i, name) in tab_names.iter().enumerate() {
        if i == app.active_tab {
            tab_spans.push(Span::styled(
                *name,
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ));
        } else {
            tab_spans.push(Span::styled(*name, Style::default().fg(Color::DarkGray)));
        }
        if i < 2 {
            tab_spans.push(Span::styled("    ", Style::default()));
        }
    }
    let line3 = Line::from(tab_spans);

    // Line 4: underline the active tab
    let mut underline = String::from("  ");
    for (i, name) in tab_names.iter().enumerate() {
        if i == app.active_tab {
            underline.push_str(&"═".repeat(name.len()));
        } else {
            underline.push_str(&" ".repeat(name.len()));
        }
        if i < 2 {
            underline.push_str("    ");
        }
    }
    let line4 = Line::styled(underline, Style::default().fg(Color::Yellow));

    let paragraph = Paragraph::new(vec![line1, line2, line3, line4]);
    f.render_widget(paragraph, area);
}

// ─── Tab 1: Pieces ──────────────────────────────────────────────────────────

fn draw_pieces_tab(
    f: &mut Frame,
    area: Rect,
    data: &FrameData,
    app: &AppState,
    worker_states: &SharedWorkerStates,
) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // overview bar
            Constraint::Min(4),    // heat map grid
            Constraint::Length(2), // info strip (workers)
        ])
        .split(area);

    draw_overview_bar(f, chunks[0], data, worker_states);
    draw_heat_map(f, chunks[1], data, app, worker_states);
    draw_info_strip(f, chunks[2], app, worker_states);
}

fn draw_overview_bar(
    f: &mut Frame,
    area: Rect,
    data: &FrameData,
    worker_states: &SharedWorkerStates,
) {
    if area.width == 0 || data.total_pieces == 0 {
        return;
    }

    let workers = worker_states.lock().unwrap();
    let width = area.width as u32;
    let mut spans: Vec<Span> = Vec::with_capacity(width as usize);

    for col in 0..width {
        let start = (col as u64 * data.total_pieces as u64) / width as u64;
        let end = ((col as u64 + 1) * data.total_pieces as u64) / width as u64;

        let mut complete = 0u32;
        let mut inflight = 0u32;
        let mut total = 0u32;
        let mut max_fill: f64 = 0.0;

        for idx in start..end {
            if (idx as u32) < data.total_pieces {
                total += 1;
                match data.pieces[idx as usize] {
                    PieceState::Complete => complete += 1,
                    PieceState::InFlight => {
                        inflight += 1;
                        for w in workers.iter() {
                            if w.current_piece == Some(idx as u32) {
                                let ratio =
                                    (w.downloaded_bytes() as f64 / w.piece_length as f64).min(1.0);
                                max_fill = max_fill.max(ratio);
                            }
                        }
                    }
                    PieceState::Pending => {}
                }
            }
        }

        let color = if total == 0 {
            Color::Rgb(45, 50, 65)
        } else if complete == total {
            Color::Rgb(40, 200, 80)
        } else if inflight > 0 {
            let g = 140 + (60.0 * max_fill) as u8;
            Color::Rgb(220, g, 0)
        } else if complete > 0 {
            Color::Rgb(0, 128, 0)
        } else {
            Color::Rgb(45, 50, 65)
        };

        spans.push(Span::styled("▮", Style::default().fg(color)));
    }

    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn draw_heat_map(
    f: &mut Frame,
    area: Rect,
    data: &FrameData,
    app: &AppState,
    worker_states: &SharedWorkerStates,
) {
    if area.width == 0 || area.height == 0 || data.total_pieces == 0 {
        return;
    }

    let total = data.total_pieces as usize;

    // Calculate grid dimensions: fit all pieces into the available area
    let margin_left: u16 = 6; // "999M " byte offset label
    let usable_w = area.width.saturating_sub(margin_left) as usize;
    let usable_h = area.height as usize;

    // Find best cell size to fill the area (cell_h includes 1-row gap between rows)
    let (cell_w, cell_h, cols) = compute_cell_size(total, usable_w, usable_h);
    let gap_y: u16 = if cell_h > 2 { 1 } else { 0 };
    let stride_y = cell_h + gap_y as usize;
    let rows_needed = total.div_ceil(cols);

    // Scrolling
    let visible_grid_rows = usable_h.checked_div(stride_y).unwrap_or(1);
    let max_scroll = rows_needed.saturating_sub(visible_grid_rows) as u16;
    let scroll = app.scroll_offset.min(max_scroll);

    let states = worker_states.lock().unwrap();

    let buf = f.buffer_mut();

    for grid_row in 0..visible_grid_rows {
        let actual_row = grid_row + scroll as usize;
        if actual_row >= rows_needed {
            break;
        }

        // Draw byte offset label on left margin
        let byte_offset = actual_row as u64 * cols as u64 * data.piece_length as u64;
        let label = format_offset_label(byte_offset);
        let label_y = area.y + (grid_row * stride_y) as u16;
        if label_y < area.y + area.height {
            let label_style = Style::default().fg(Color::DarkGray);
            for (i, ch) in label.chars().enumerate() {
                let x = area.x + i as u16;
                if x < area.x + margin_left
                    && let Some(cell) = buf.cell_mut((x, label_y))
                {
                    cell.set_char(ch);
                    cell.set_style(label_style);
                }
            }
        }

        for col in 0..cols {
            let piece_idx = actual_row * cols + col;
            if piece_idx >= total {
                break;
            }

            let state = data.pieces[piece_idx];

            // Get fill ratio for in-flight pieces
            let fill_ratio = match state {
                PieceState::Complete => 1.0,
                PieceState::InFlight => states
                    .iter()
                    .find(|w| w.current_piece == Some(piece_idx as u32))
                    .map(|w| {
                        if w.piece_length > 0 {
                            (w.downloaded_bytes() as f64 / w.piece_length as f64).min(1.0)
                        } else {
                            0.0
                        }
                    })
                    .unwrap_or(0.0),
                PieceState::Pending => 0.0,
            };

            // Compute background color from gradient
            let bg_color = progress_color(fill_ratio, state, app.frame_count, piece_idx);
            let border_color = dim_color(bg_color, 0.4);

            // Draw the cell with right-edge border
            let cell_x = area.x + margin_left + (col * cell_w) as u16;
            let cell_y = area.y + (grid_row * stride_y) as u16;

            for dy in 0..cell_h as u16 {
                for dx in 0..cell_w as u16 {
                    let x = cell_x + dx;
                    let y = cell_y + dy;
                    if x < area.x + area.width
                        && y < area.y + area.height
                        && let Some(cell) = buf.cell_mut((x, y))
                    {
                        let is_border = dx == cell_w as u16 - 1 && cell_w > 2;
                        let color = if is_border { border_color } else { bg_color };
                        cell.set_char(' ');
                        cell.set_style(Style::default().bg(color));
                    }
                }
            }
        }
    }

    // Scrollbar if needed
    if rows_needed > visible_grid_rows {
        let scroll_area = Rect {
            x: area.x + area.width - 1,
            y: area.y,
            width: 1,
            height: area.height,
        };
        let mut scrollbar_state = ScrollbarState::new(rows_needed).position(scroll as usize);
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight);
        f.render_stateful_widget(scrollbar, scroll_area, &mut scrollbar_state);
    }
}

fn compute_cell_size(
    total_pieces: usize,
    usable_w: usize,
    usable_h: usize,
) -> (usize, usize, usize) {
    // Try cell sizes from large to small, pick the largest that fits all pieces
    // Account for 1-row gap between rows when cell_h > 2
    let candidates: &[(usize, usize)] = &[
        (12, 5),
        (10, 4),
        (8, 4),
        (8, 3),
        (6, 3),
        (6, 2),
        (5, 2),
        (4, 2),
        (3, 2),
        (3, 1),
        (2, 1),
        (1, 1),
    ];

    for &(cw, ch) in candidates {
        let cols = usable_w / cw;
        let gap = if ch > 2 { 1 } else { 0 };
        let stride = ch + gap;
        let rows = usable_h / stride;
        if cols > 0 && rows > 0 && cols * rows >= total_pieces {
            return (cw, ch, cols);
        }
    }

    // Fallback: 1 char per cell, as many cols as possible
    let cols = usable_w.max(1);
    (1, 1, cols)
}

fn progress_color(ratio: f64, state: PieceState, frame_count: u64, piece_idx: usize) -> Color {
    match state {
        PieceState::Pending => Color::Rgb(45, 50, 65),
        PieceState::Complete => Color::Rgb(40, 200, 80),
        PieceState::InFlight => {
            // Breathing animation: modulate brightness with sine wave
            let phase = (frame_count as f64 + piece_idx as f64 * 3.0) * 0.15;
            let breath = (phase.sin() * 0.15 + 1.0).clamp(0.85, 1.15);

            // Gradient: dark red → orange → yellow based on ratio
            let (r, g, b) = if ratio < 0.25 {
                let t = ratio / 0.25;
                lerp_rgb((80, 15, 5), (150, 40, 0), t)
            } else if ratio < 0.5 {
                let t = (ratio - 0.25) / 0.25;
                lerp_rgb((150, 40, 0), (200, 100, 0), t)
            } else if ratio < 0.75 {
                let t = (ratio - 0.5) / 0.25;
                lerp_rgb((200, 100, 0), (220, 180, 0), t)
            } else {
                let t = (ratio - 0.75) / 0.25;
                lerp_rgb((220, 180, 0), (100, 220, 30), t)
            };

            let r = (r as f64 * breath).clamp(0.0, 255.0) as u8;
            let g = (g as f64 * breath).clamp(0.0, 255.0) as u8;
            let b = (b as f64 * breath).clamp(0.0, 255.0) as u8;
            Color::Rgb(r, g, b)
        }
    }
}

fn lerp_rgb(from: (u8, u8, u8), to: (u8, u8, u8), t: f64) -> (u8, u8, u8) {
    let r = from.0 as f64 + (to.0 as f64 - from.0 as f64) * t;
    let g = from.1 as f64 + (to.1 as f64 - from.1 as f64) * t;
    let b = from.2 as f64 + (to.2 as f64 - from.2 as f64) * t;
    (r as u8, g as u8, b as u8)
}

fn dim_color(color: Color, factor: f64) -> Color {
    match color {
        Color::Rgb(r, g, b) => Color::Rgb(
            (r as f64 * factor) as u8,
            (g as f64 * factor) as u8,
            (b as f64 * factor) as u8,
        ),
        _ => Color::Rgb(20, 20, 25),
    }
}

fn format_offset_label(bytes: u64) -> String {
    const MIB: u64 = 1024 * 1024;
    const GIB: u64 = 1024 * 1024 * 1024;

    if bytes >= GIB {
        format!("{:>4}G ", bytes / GIB)
    } else {
        format!("{:>4}M ", bytes / MIB)
    }
}

fn draw_info_strip(f: &mut Frame, area: Rect, app: &AppState, worker_states: &SharedWorkerStates) {
    let states = worker_states.lock().unwrap();

    let active_workers: Vec<_> = states
        .iter()
        .filter(|w| w.status == WorkerStatus::Downloading || w.status == WorkerStatus::Retrying)
        .collect();

    if active_workers.is_empty() {
        return;
    }

    let mut spans: Vec<Span> = Vec::new();
    let bar_width = 8usize;

    for (i, w) in active_workers.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled("  ", Style::default()));
        }

        // Worker badge
        let badge_color = if w.status == WorkerStatus::Retrying {
            Color::Yellow
        } else {
            Color::Green
        };
        spans.push(Span::styled(
            format!(" W/{} ", w.id),
            Style::default().fg(Color::Black).bg(badge_color),
        ));

        // Mini progress bar
        let ratio = if w.piece_length > 0 {
            (w.downloaded_bytes() as f64 / w.piece_length as f64).min(1.0)
        } else {
            0.0
        };
        let filled = (bar_width as f64 * ratio) as usize;
        let empty = bar_width.saturating_sub(filled);
        spans.push(Span::styled(
            "━".repeat(filled),
            Style::default().fg(Color::Cyan),
        ));
        spans.push(Span::styled("╸", Style::default().fg(Color::Cyan)));
        spans.push(Span::styled(
            "━".repeat(empty),
            Style::default().fg(Color::Rgb(60, 60, 60)),
        ));

        // Speed (use live speed from AppState)
        let live_speed = app.worker_speed(w.id);
        let speed_str = if live_speed > 0.0 {
            format_speed_compact(live_speed)
        } else if w.speed_bps > 0.0 {
            format_speed_compact(w.speed_bps)
        } else {
            "---".to_string()
        };
        spans.push(Span::styled(
            format!(" {}", speed_str),
            Style::default().fg(Color::White),
        ));
    }

    let line = Line::from(spans);
    f.render_widget(Paragraph::new(vec![Line::raw(""), line]), area);
}

// ─── Tab 2: Workers ─────────────────────────────────────────────────────────

fn draw_workers_tab(
    f: &mut Frame,
    area: Rect,
    data: &FrameData,
    app: &AppState,
    worker_states: &SharedWorkerStates,
) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Workers ")
        .border_style(Style::default().fg(Color::Cyan));

    let inner = block.inner(area);
    f.render_widget(block, area);

    let states = worker_states.lock().unwrap();

    let header = Row::new(vec![
        "ID", "Piece", "Progress", "Speed", "Retries", "Status",
    ])
    .style(
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    );

    let rows: Vec<Row> = states
        .iter()
        .map(|w| {
            let piece_str = w
                .current_piece
                .map(|p| format!("#{p}"))
                .unwrap_or_else(|| "-".to_string());

            let progress_str = if w.piece_length > 0 && w.current_piece.is_some() {
                let downloaded = w.downloaded_bytes();
                let pct = (downloaded as f64 / w.piece_length as f64 * 100.0).min(100.0);
                let bar_width = 10;
                let filled = (pct / 100.0 * bar_width as f64) as usize;
                format!(
                    "[{}{}] {:>3.0}%",
                    "━".repeat(filled),
                    "─".repeat(bar_width - filled),
                    pct
                )
            } else {
                "-".to_string()
            };

            let live_speed = app.worker_speed(w.id);
            let speed_str = if live_speed > 0.0 {
                format_speed(live_speed)
            } else if w.speed_bps > 0.0 {
                format_speed(w.speed_bps)
            } else {
                "-".to_string()
            };

            let retries_str = if w.retries > 0 {
                format!("{}", w.retries)
            } else {
                "-".to_string()
            };

            let status_style = match w.status {
                WorkerStatus::Downloading => Style::default().fg(Color::Green),
                WorkerStatus::Retrying => Style::default().fg(Color::Yellow),
                WorkerStatus::Done => Style::default().fg(Color::DarkGray),
                WorkerStatus::Idle => Style::default().fg(Color::White),
            };

            Row::new(vec![
                format!("W/{}", w.id),
                piece_str,
                progress_str,
                speed_str,
                retries_str,
                w.status.to_string(),
            ])
            .style(status_style)
        })
        .collect();

    let widths = [
        Constraint::Length(5),
        Constraint::Length(7),
        Constraint::Length(18),
        Constraint::Length(12),
        Constraint::Length(8),
        Constraint::Length(12),
    ];

    let table = Table::new(rows, widths)
        .header(header)
        .row_highlight_style(Style::default().add_modifier(Modifier::REVERSED));

    let _ = data.num_workers;
    f.render_widget(table, inner);
}

// ─── Tab 3: Stats ───────────────────────────────────────────────────────────

fn draw_stats_tab(f: &mut Frame, area: Rect, app: &AppState, events: &SharedEventLog) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(5), // sparkline
            Constraint::Min(3),    // event log
        ])
        .split(area);

    draw_sparkline(f, chunks[0], app);
    draw_event_log(f, chunks[1], app, events);
}

fn draw_sparkline(f: &mut Frame, area: Rect, app: &AppState) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Speed History (60s) ")
        .border_style(Style::default().fg(Color::Green));

    let inner = block.inner(area);
    f.render_widget(block, area);

    let data: Vec<u64> = app.speed_history.iter().copied().collect();
    if data.is_empty() {
        return;
    }

    let sparkline = Sparkline::default()
        .data(&data)
        .style(Style::default().fg(Color::Cyan));
    f.render_widget(sparkline, inner);
}

fn draw_event_log(f: &mut Frame, area: Rect, app: &AppState, events: &SharedEventLog) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Event Log ")
        .border_style(Style::default().fg(Color::Yellow));

    let inner = block.inner(area);
    f.render_widget(block, area);

    let log = events.lock().unwrap();
    let lines: Vec<Line> = log.iter().map(|s| Line::raw(s.as_str())).collect();
    let total_lines = lines.len() as u16;

    let max_scroll = total_lines.saturating_sub(inner.height);
    let scroll = app.event_scroll.min(max_scroll);

    let paragraph = Paragraph::new(lines)
        .scroll((scroll, 0))
        .wrap(Wrap { trim: false });
    f.render_widget(paragraph, inner);

    if total_lines > inner.height {
        let mut scrollbar_state =
            ScrollbarState::new(total_lines as usize).position(scroll as usize);
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight);
        f.render_stateful_widget(
            scrollbar,
            inner.inner(Margin {
                vertical: 0,
                horizontal: 0,
            }),
            &mut scrollbar_state,
        );
    }
}

// ─── Footer ─────────────────────────────────────────────────────────────────

fn draw_footer(f: &mut Frame, area: Rect, data: &FrameData) {
    let eta = if data.speed_bps > 0.0 && data.downloaded_bytes < data.total_length {
        let remaining_bytes = data.total_length - data.downloaded_bytes;
        let secs = remaining_bytes as f64 / data.speed_bps;
        format_eta(secs)
    } else if data.downloaded_bytes >= data.total_length && data.total_length > 0 {
        "done".to_string()
    } else {
        "---".to_string()
    };

    let in_flight = data
        .pieces
        .iter()
        .filter(|p| **p == PieceState::InFlight)
        .count();

    let line = Line::from(vec![
        Span::styled(" Speed: ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            format_speed(data.speed_bps),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" │ ETA: ", Style::default().fg(Color::DarkGray)),
        Span::styled(&eta, Style::default().fg(Color::Magenta)),
        Span::styled(" │ CN: ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            format!("{}/{}", in_flight, data.num_workers),
            Style::default().fg(Color::White),
        ),
        Span::styled(" │ Piece: ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            format_size_compact(data.piece_length as u64),
            Style::default().fg(Color::White),
        ),
        Span::styled(
            format!(
                " │ {:.0}% complete",
                if data.total_length > 0 {
                    data.downloaded_bytes as f64 / data.total_length as f64 * 100.0
                } else {
                    0.0
                }
            ),
            Style::default().fg(Color::DarkGray),
        ),
    ]);

    f.render_widget(Paragraph::new(line), area);
}

// ─── Help Overlay ───────────────────────────────────────────────────────────

fn draw_help_overlay(f: &mut Frame, area: Rect) {
    let width = 40u16;
    let height = 14u16;
    let x = area.width.saturating_sub(width) / 2;
    let y = area.height.saturating_sub(height) / 2;

    let popup_area = Rect::new(x, y, width.min(area.width), height.min(area.height));

    f.render_widget(Clear, popup_area);

    let lines = vec![
        Line::raw(""),
        Line::from(vec![
            Span::styled("  Tab", Style::default().fg(Color::Cyan)),
            Span::styled(" / ", Style::default().fg(Color::DarkGray)),
            Span::styled("1-3", Style::default().fg(Color::Cyan)),
            Span::raw("    Switch tabs"),
        ]),
        Line::from(vec![
            Span::styled("  ↑↓", Style::default().fg(Color::Cyan)),
            Span::styled(" / ", Style::default().fg(Color::DarkGray)),
            Span::styled("j k", Style::default().fg(Color::Cyan)),
            Span::raw("    Scroll"),
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
        Line::raw(""),
        Line::styled(
            "  Pieces tab:",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        Line::from(vec![
            Span::styled("  ↑↓", Style::default().fg(Color::Cyan)),
            Span::raw("             Scroll grid"),
        ]),
        Line::raw(""),
        Line::styled(
            "  Stats tab:",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        Line::from(vec![
            Span::styled("  ↑↓", Style::default().fg(Color::Cyan)),
            Span::raw("             Scroll event log"),
        ]),
        Line::raw(""),
    ];

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(" Key Bindings ");

    let paragraph = Paragraph::new(lines).block(block);
    f.render_widget(paragraph, popup_area);
}

// ─── Formatting helpers ─────────────────────────────────────────────────────

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

fn format_speed_compact(bps: f64) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = KIB * 1024.0;

    if bps >= MIB {
        format!("{:.1}M/s", bps / MIB)
    } else if bps >= KIB {
        format!("{:.0}K/s", bps / KIB)
    } else {
        format!("{:.0}B/s", bps)
    }
}

fn format_size_compact(bytes: u64) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = KIB * 1024.0;
    const GIB: f64 = MIB * 1024.0;

    let b = bytes as f64;
    if b >= GIB {
        format!("{:.1}G", b / GIB)
    } else if b >= MIB {
        format!("{:.1}M", b / MIB)
    } else if b >= KIB {
        format!("{:.0}K", b / KIB)
    } else {
        format!("{bytes}B")
    }
}

fn format_eta(secs: f64) -> String {
    let s = secs as u64;
    if s >= 3600 {
        format!("{}h{:02}m", s / 3600, (s % 3600) / 60)
    } else if s >= 60 {
        format!("{}m{:02}s", s / 60, s % 60)
    } else {
        format!("{s}s")
    }
}
