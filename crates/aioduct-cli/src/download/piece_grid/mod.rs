use std::collections::VecDeque;
use std::io::{self, IsTerminal, stdout};
use std::path::PathBuf;
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
    Block, Borders, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState, Sparkline,
};
use tokio_util::sync::CancellationToken;

use crate::common::copy_to_clipboard;

use super::file_entry::FileId;
use super::piece::storage::PieceStorage;
use super::segment_man::SegmentMan;
use super::tui_common::{
    DOWNLOAD_TABS, EventFilter, HorizontalKeyAction, detail_line, draw_cancel_overlay,
    draw_download_help_overlay, format_size_compact, format_size_iec, format_speed,
    format_speed_compact, horizontal_key_action, open_path, percent, push_ui_event, truncate_chars,
    worker_status_color,
};
use super::tui_state::{
    DownloadEvent, EventSeverity, SharedEventLog, SharedWorkerStates, WorkerState, WorkerStatus,
    display_worker_id, format_duration_compact,
};

#[derive(Clone)]
pub struct PieceGridTarget {
    pub url: String,
    pub output: PathBuf,
    pub filename: String,
    pub control_path: PathBuf,
    pub supports_range: bool,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub created_at: String,
    pub resume_skipped_pieces: u32,
    pub allocation: &'static str,
    pub checksum_status: super::checksum::SharedChecksumStatus,
}

pub struct PieceGrid {
    cancel: CancellationToken,
    handle: Option<tokio::task::JoinHandle<()>>,
}

impl PieceGrid {
    #[allow(clippy::too_many_arguments)]
    pub fn start(
        segment_man: Arc<SegmentMan>,
        total_length: u64,
        target: PieceGridTarget,
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
            target,
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
    event_filter: EventFilter,
    filter_query: String,
    editing_filter: bool,
    selected_piece: Option<u32>,
    selected_worker: usize,
    worker_sort_by_id: bool,
    focus_index: usize,
    show_cancel_confirm: bool,
}

impl AppState {
    fn new() -> Self {
        let now = Instant::now();
        Self {
            active_tab: 1,
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
            event_filter: EventFilter::All,
            filter_query: String::new(),
            editing_filter: false,
            selected_piece: None,
            selected_worker: 0,
            worker_sort_by_id: false,
            focus_index: 0,
            show_cancel_confirm: false,
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
    Failed,
}

#[derive(Clone)]
pub struct PieceSnapshot {
    pub index: u32,
    pub state: PieceState,
    pub retry_count: u32,
    pub last_error: Option<String>,
}

struct FrameData {
    total_pieces: u32,
    completed: u32,
    piece_length: u32,
    total_length: u64,
    downloaded_bytes: u64,
    speed_bps: f64,
    filename: String,
    num_workers: usize,
    pieces: Vec<PieceSnapshot>,
}

#[allow(clippy::too_many_arguments)]
async fn run_tui(
    segment_man: Arc<SegmentMan>,
    total_length: u64,
    target: PieceGridTarget,
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

        if let Some(action) = poll_input(app.editing_filter, app.active_tab) {
            if app.show_cancel_confirm {
                match action {
                    InputAction::Quit | InputAction::ForceQuit => {
                        parent_cancel.cancel();
                        break;
                    }
                    InputAction::Dismiss | InputAction::Confirm => {
                        app.show_cancel_confirm = false;
                    }
                    _ => {}
                }
                continue;
            }
            match action {
                InputAction::Quit => {
                    if single_download_is_finished(&segment_man) {
                        parent_cancel.cancel();
                        break;
                    }
                    app.show_cancel_confirm = true;
                }
                InputAction::ForceQuit => {
                    parent_cancel.cancel();
                    break;
                }
                InputAction::Dismiss => {
                    if app.show_cancel_confirm {
                        app.show_cancel_confirm = false;
                    } else if app.show_help {
                        app.show_help = false;
                    }
                }
                InputAction::ToggleHelp => {
                    app.show_help = !app.show_help;
                }
                InputAction::PrevTab => {
                    app.active_tab =
                        (app.active_tab + DOWNLOAD_TABS.len() - 1) % DOWNLOAD_TABS.len();
                    app.focus_index = 0;
                }
                InputAction::NextTab => {
                    app.active_tab = (app.active_tab + 1) % DOWNLOAD_TABS.len();
                    app.focus_index = 0;
                }
                InputAction::Tab(n) => {
                    app.active_tab = n.min(DOWNLOAD_TABS.len() - 1);
                    app.focus_index = 0;
                }
                InputAction::FocusNext => {
                    app.focus_index = (app.focus_index + 1) % focus_count(app.active_tab);
                }
                InputAction::Confirm => {
                    if app.show_cancel_confirm {
                        app.show_cancel_confirm = false;
                    } else {
                        app.active_tab = match app.active_tab {
                            0 => 1,
                            2 => 3,
                            3 => {
                                if let Some(piece) =
                                    selected_worker_piece(&worker_states, app.selected_worker)
                                        .or_else(|| active_worker_piece(&worker_states))
                                {
                                    app.selected_piece = Some(piece);
                                }
                                2
                            }
                            page => page,
                        };
                        app.focus_index = 0;
                    }
                }
                InputAction::ScrollUp => match app.active_tab {
                    2 => app.scroll_offset = app.scroll_offset.saturating_sub(1),
                    3 => move_worker_selection(
                        &mut app.selected_worker,
                        &worker_states,
                        app.worker_sort_by_id,
                        -1,
                    ),
                    4 => app.event_scroll = app.event_scroll.saturating_sub(1),
                    _ => {}
                },
                InputAction::ScrollDown => match app.active_tab {
                    2 => app.scroll_offset = app.scroll_offset.saturating_add(1),
                    3 => move_worker_selection(
                        &mut app.selected_worker,
                        &worker_states,
                        app.worker_sort_by_id,
                        1,
                    ),
                    4 => app.event_scroll = app.event_scroll.saturating_add(1),
                    _ => {}
                },
                InputAction::ScrollPageUp => match app.active_tab {
                    2 => app.scroll_offset = app.scroll_offset.saturating_sub(10),
                    3 => move_worker_selection(
                        &mut app.selected_worker,
                        &worker_states,
                        app.worker_sort_by_id,
                        -10,
                    ),
                    4 => app.event_scroll = app.event_scroll.saturating_sub(10),
                    _ => {}
                },
                InputAction::ScrollPageDown => match app.active_tab {
                    2 => app.scroll_offset = app.scroll_offset.saturating_add(10),
                    3 => move_worker_selection(
                        &mut app.selected_worker,
                        &worker_states,
                        app.worker_sort_by_id,
                        10,
                    ),
                    4 => app.event_scroll = app.event_scroll.saturating_add(10),
                    _ => {}
                },
                InputAction::ScrollTop => match app.active_tab {
                    2 => app.scroll_offset = 0,
                    3 => select_worker_edge(
                        &mut app.selected_worker,
                        &worker_states,
                        app.worker_sort_by_id,
                        false,
                    ),
                    4 => app.event_scroll = 0,
                    _ => {}
                },
                InputAction::ScrollBottom => match app.active_tab {
                    2 => app.scroll_offset = u16::MAX,
                    3 => select_worker_edge(
                        &mut app.selected_worker,
                        &worker_states,
                        app.worker_sort_by_id,
                        true,
                    ),
                    4 => app.event_scroll = u16::MAX,
                    _ => {}
                },
                InputAction::SetEventFilter(filter) => {
                    app.event_filter = filter;
                    app.event_scroll = 0;
                }
                InputAction::StartFilter => {
                    app.editing_filter = true;
                    app.event_scroll = 0;
                }
                InputAction::FilterChar(ch) => {
                    app.filter_query.push(ch);
                    app.event_scroll = 0;
                }
                InputAction::FilterBackspace => {
                    app.filter_query.pop();
                    app.event_scroll = 0;
                }
                InputAction::FilterSubmit => {
                    app.editing_filter = false;
                }
                InputAction::FilterCancel => {
                    app.editing_filter = false;
                }
                InputAction::CopyVisible => {
                    copy_visible_text(&target, &app, &events);
                }
                InputAction::OpenOutput => {
                    open_output_dir(&target, &events);
                }
                InputAction::ToggleWorkerSort => {
                    if app.active_tab == 3 {
                        app.worker_sort_by_id = !app.worker_sort_by_id;
                        sync_worker_selection(
                            &mut app.selected_worker,
                            &worker_states,
                            app.worker_sort_by_id,
                        );
                    }
                }
                InputAction::PrevPiece => {
                    if app.active_tab == 2 {
                        let current = app
                            .selected_piece
                            .or_else(|| active_worker_piece(&worker_states))
                            .unwrap_or(0);
                        app.selected_piece = Some(current.saturating_sub(1));
                    } else {
                        app.focus_index = app
                            .focus_index
                            .saturating_sub(1)
                            .min(focus_count(app.active_tab).saturating_sub(1));
                    }
                }
                InputAction::NextPiece => {
                    if app.active_tab == 2 {
                        let current = app
                            .selected_piece
                            .or_else(|| active_worker_piece(&worker_states))
                            .unwrap_or(0);
                        app.selected_piece = Some(current.saturating_add(1));
                    } else {
                        app.focus_index = (app.focus_index + 1) % focus_count(app.active_tab);
                    }
                }
            }
        }

        let mut data = segment_man.snapshot_storage(|storage| FrameData {
            total_pieces: storage.total_pieces(),
            completed: storage.completed_count(),
            piece_length: storage.piece_length(),
            total_length,
            downloaded_bytes: 0,
            speed_bps: 0.0,
            filename: target.filename.clone(),
            num_workers,
            pieces: collect_piece_snapshots(storage),
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
            draw_ui(f, &data, &target, &app, &worker_states, &events);
        });
    }

    let _ = terminal.clear();
    let _ = restore_terminal();
}

enum InputAction {
    Quit,
    ForceQuit,
    Dismiss,
    ToggleHelp,
    PrevTab,
    NextTab,
    Tab(usize),
    FocusNext,
    Confirm,
    ScrollUp,
    ScrollDown,
    ScrollPageUp,
    ScrollPageDown,
    ScrollTop,
    ScrollBottom,
    SetEventFilter(EventFilter),
    StartFilter,
    FilterChar(char),
    FilterBackspace,
    FilterSubmit,
    FilterCancel,
    CopyVisible,
    OpenOutput,
    ToggleWorkerSort,
    PrevPiece,
    NextPiece,
}

fn poll_input(editing_filter: bool, active_tab: usize) -> Option<InputAction> {
    if !event::poll(Duration::ZERO).unwrap_or(false) {
        return None;
    }
    let Ok(Event::Key(key)) = event::read() else {
        return None;
    };
    if key.kind != KeyEventKind::Press {
        return None;
    }
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        return Some(InputAction::ForceQuit);
    }
    if editing_filter {
        return match key.code {
            KeyCode::Esc => Some(InputAction::FilterCancel),
            KeyCode::Enter => Some(InputAction::FilterSubmit),
            KeyCode::Backspace => Some(InputAction::FilterBackspace),
            KeyCode::Char(ch) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                Some(InputAction::FilterChar(ch))
            }
            _ => None,
        };
    }
    if let Some(action) = horizontal_key_action(key.code, active_tab) {
        return Some(match action {
            HorizontalKeyAction::PrevPage => InputAction::PrevTab,
            HorizontalKeyAction::NextPage => InputAction::NextTab,
            HorizontalKeyAction::PrevPiece => InputAction::PrevPiece,
            HorizontalKeyAction::NextPiece => InputAction::NextPiece,
        });
    }
    match key.code {
        KeyCode::Char('q') => Some(InputAction::Quit),
        KeyCode::Char('?') => Some(InputAction::ToggleHelp),
        KeyCode::Esc => Some(InputAction::Dismiss),
        KeyCode::Tab => Some(InputAction::FocusNext),
        KeyCode::Enter => Some(InputAction::Confirm),
        KeyCode::Char('1') => Some(InputAction::Tab(0)),
        KeyCode::Char('2') => Some(InputAction::Tab(1)),
        KeyCode::Char('3') => Some(InputAction::Tab(2)),
        KeyCode::Char('4') => Some(InputAction::Tab(3)),
        KeyCode::Char('5') => Some(InputAction::Tab(4)),
        KeyCode::Char('6') => Some(InputAction::Tab(5)),
        KeyCode::Char('a') => Some(InputAction::SetEventFilter(EventFilter::All)),
        KeyCode::Char('f') => Some(InputAction::SetEventFilter(EventFilter::Failures)),
        KeyCode::Char('r') => Some(InputAction::SetEventFilter(EventFilter::Retries)),
        KeyCode::Char('w') => Some(InputAction::SetEventFilter(EventFilter::Worker)),
        KeyCode::Char('s') if active_tab == 3 => Some(InputAction::ToggleWorkerSort),
        KeyCode::Char('s') => Some(InputAction::SetEventFilter(EventFilter::SelectedFile)),
        KeyCode::Char('/') => Some(InputAction::StartFilter),
        KeyCode::Char('y') => Some(InputAction::CopyVisible),
        KeyCode::Char('o') => Some(InputAction::OpenOutput),
        KeyCode::Char('[') => Some(InputAction::PrevPiece),
        KeyCode::Char(']') => Some(InputAction::NextPiece),
        KeyCode::Up | KeyCode::Char('k') => Some(InputAction::ScrollUp),
        KeyCode::Down | KeyCode::Char('j') => Some(InputAction::ScrollDown),
        KeyCode::PageUp => Some(InputAction::ScrollPageUp),
        KeyCode::PageDown => Some(InputAction::ScrollPageDown),
        KeyCode::Home | KeyCode::Char('g') => Some(InputAction::ScrollTop),
        KeyCode::End | KeyCode::Char('G') => Some(InputAction::ScrollBottom),
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

pub fn collect_piece_snapshots(storage: &PieceStorage) -> Vec<PieceSnapshot> {
    let total = storage.total_pieces();
    (0..total)
        .map(|i| {
            let metadata = storage.metadata(i);
            let state = if storage.is_complete(i) {
                PieceState::Complete
            } else if metadata.is_some_and(|meta| meta.failed) {
                PieceState::Failed
            } else if storage.is_in_flight(i) {
                PieceState::InFlight
            } else {
                PieceState::Pending
            };
            PieceSnapshot {
                index: i,
                state,
                retry_count: metadata.map(|meta| meta.retry_count).unwrap_or(0),
                last_error: metadata.and_then(|meta| meta.last_error.clone()),
            }
        })
        .collect()
}

#[derive(Clone, Copy)]
pub struct HeatMapParams<'a> {
    pub pieces: &'a [PieceSnapshot],
    pub total_pieces: u32,
    pub piece_length: u32,
    pub scroll_offset: u16,
    pub frame_count: u64,
    pub selected_piece: Option<u32>,
    pub viewport: Option<PieceViewport>,
}

pub(crate) const PIECE_DETAIL_TITLE: &str = " Piece detail ";
const PIECE_DETAIL_MIN_HEIGHT: u16 = 9;
const PIECE_GRID_MAX_HEIGHT: u16 = 16;
const AUTO_MIN_PIECE: u64 = 64 * 1024;
const AUTO_MAX_PIECE: u64 = 4 * 1024 * 1024;
const SMALL_GRID_PIECES: u32 = 32;
const LARGE_GRID_PIECES: u32 = 128;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PieceVisualState {
    Small,
    Medium,
    Large,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PieceViewport {
    pub cols: usize,
    pub rows_needed: usize,
    pub visible_rows: usize,
    pub scroll: u16,
    pub first_piece: u32,
    pub last_piece: u32,
}

fn piece_visual_state(total_pieces: u32) -> PieceVisualState {
    if total_pieces <= SMALL_GRID_PIECES {
        PieceVisualState::Small
    } else if total_pieces <= LARGE_GRID_PIECES {
        PieceVisualState::Medium
    } else {
        PieceVisualState::Large
    }
}

fn piece_grid_columns(width: usize, total_pieces: usize) -> usize {
    width.saturating_sub(1).max(1).min(total_pieces.max(1))
}

fn piece_grid_cells_per_row(width: usize, total_pieces: usize) -> usize {
    if piece_visual_state(total_pieces as u32) == PieceVisualState::Small {
        (width / 3).max(1).min(total_pieces.max(1))
    } else {
        piece_grid_columns(width, total_pieces)
    }
}

pub(crate) fn piece_viewport_for_area(area: Rect, params: &HeatMapParams) -> PieceViewport {
    piece_viewport_for_inner(
        area.width.saturating_sub(2),
        area.height.saturating_sub(2),
        params,
    )
}

fn piece_viewport_for_inner(
    inner_width: u16,
    inner_height: u16,
    params: &HeatMapParams,
) -> PieceViewport {
    let total = params.total_pieces as usize;
    if total == 0 {
        return PieceViewport {
            cols: 1,
            rows_needed: 0,
            visible_rows: 1,
            scroll: 0,
            first_piece: 0,
            last_piece: 0,
        };
    }

    let legend_height = 1usize;
    let usable_h = (inner_height as usize).saturating_sub(legend_height).max(1);
    let cols = piece_grid_cells_per_row(inner_width as usize, total);
    let rows_needed = total.div_ceil(cols);
    let visible_rows = usable_h.min(rows_needed).max(1);
    let max_scroll = rows_needed.saturating_sub(visible_rows) as u16;
    let scroll = params.scroll_offset.min(max_scroll);
    let first = scroll as usize * cols;
    let last = ((scroll as usize + visible_rows) * cols)
        .saturating_sub(1)
        .min(total.saturating_sub(1));

    PieceViewport {
        cols,
        rows_needed,
        visible_rows,
        scroll,
        first_piece: first as u32,
        last_piece: last as u32,
    }
}

pub(crate) fn piece_grid_panel_height(width: u16, total_pieces: u32) -> u16 {
    if total_pieces == 0 {
        return PIECE_DETAIL_MIN_HEIGHT;
    }

    let inner_width = width.saturating_sub(2) as usize;
    let cols = piece_grid_cells_per_row(inner_width, total_pieces as usize);
    let rows_needed = (total_pieces as usize).div_ceil(cols) as u16;
    rows_needed
        .saturating_add(3)
        .clamp(PIECE_DETAIL_MIN_HEIGHT, PIECE_GRID_MAX_HEIGHT)
}

pub(crate) fn piece_size_policy_label(piece_length: u32) -> String {
    let size = format_size_iec(piece_length as u64);
    match piece_length as u64 {
        0 => "piece unknown".to_string(),
        bytes if bytes <= AUTO_MIN_PIECE => format!("piece auto {size} min"),
        AUTO_MAX_PIECE => format!("piece auto {size} max"),
        bytes if bytes > AUTO_MAX_PIECE => format!("piece override {size}"),
        _ => format!("piece auto {size}"),
    }
}

pub fn render_piece_policy_line(f: &mut Frame, area: Rect, params: &HeatMapParams) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    let size = format_size_iec(params.piece_length as u64);
    let spans = Line::from(vec![
        Span::styled(
            " Policy ",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("{}  ", piece_size_policy_label(params.piece_length)),
            Style::default().fg(Color::White),
        ),
        Span::styled(
            "bounds 64 KiB..4 MiB  ",
            Style::default().fg(Color::DarkGray),
        ),
        Span::styled(
            "each cell = one retry unit  ",
            Style::default().fg(Color::DarkGray),
        ),
        Span::styled(
            format!("redo bound {size}"),
            Style::default().fg(Color::DarkGray),
        ),
    ]);

    f.render_widget(Paragraph::new(spans), area);
}

pub fn render_overview_bar(
    f: &mut Frame,
    area: Rect,
    params: &HeatMapParams,
    worker_states: &SharedWorkerStates,
) {
    if area.width == 0 || area.height == 0 || params.total_pieces == 0 {
        return;
    }

    let workers = worker_states.lock().unwrap();
    let completed = params
        .pieces
        .iter()
        .filter(|p| p.state == PieceState::Complete)
        .count();
    let selected_piece = params.selected_piece;
    let viewport = params.viewport;
    let prefix = " Map ";
    let suffix = if params.total_pieces > LARGE_GRID_PIECES {
        if let Some(viewport) = viewport {
            format!(
                "  {completed} / {} pieces  view {}-{}",
                params.total_pieces, viewport.first_piece, viewport.last_piece
            )
        } else {
            format!("  {completed} / {} pieces", params.total_pieces)
        }
    } else {
        format!("  {completed} / {} pieces", params.total_pieces)
    };
    let bar_width = area
        .width
        .saturating_sub((prefix.len() + suffix.len()) as u16)
        .max(8) as usize;
    let bar_width = piece_grid_columns(bar_width, params.total_pieces as usize);
    let mut spans: Vec<Span> = Vec::with_capacity(bar_width + 4);

    spans.push(Span::styled(
        prefix,
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    ));

    for col in 0..bar_width {
        let start = (col as u64 * params.total_pieces as u64) / bar_width as u64;
        let end = ((col as u64 + 1) * params.total_pieces as u64) / bar_width as u64;

        let mut failed = 0u32;
        let mut inflight = 0u32;
        let mut complete = 0u32;
        let mut pending = 0u32;

        for idx in start..end {
            if (idx as u32) < params.total_pieces {
                match params.pieces[idx as usize].state {
                    PieceState::Complete => complete += 1,
                    PieceState::InFlight => inflight += 1,
                    PieceState::Failed => failed += 1,
                    PieceState::Pending => pending += 1,
                }
            }
        }
        let contains_selected = selected_piece.is_some_and(|piece| {
            let idx = piece as u64;
            idx >= start && idx < end
        });
        let contains_viewport_edge = viewport.is_some_and(|viewport| {
            let first = viewport.first_piece as u64;
            let last = viewport.last_piece as u64;
            params.total_pieces > LARGE_GRID_PIECES
                && ((first >= start && first < end) || (last >= start && last < end))
        });

        let is_retrying = workers.iter().any(|w| {
            w.current_piece.is_some_and(|piece| {
                let idx = piece as u64;
                idx >= start && idx < end
            }) && w.status == WorkerStatus::Retrying
        });
        let (glyph, color) = if contains_selected || contains_viewport_edge {
            ("█", Color::Cyan)
        } else if failed > 0 || is_retrying {
            ("▒", Color::Red)
        } else if inflight > 0 {
            ("▓", Color::Yellow)
        } else if complete > 0 && pending == 0 {
            ("█", Color::Green)
        } else if complete > 0 {
            ("▓", Color::Green)
        } else {
            ("░", Color::DarkGray)
        };

        spans.push(Span::styled(glyph, Style::default().fg(color)));
    }
    spans.push(Span::styled(suffix, Style::default().fg(Color::DarkGray)));
    drop(workers);

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

    let mode = piece_visual_state(params.total_pieces);
    let viewport = params
        .viewport
        .unwrap_or_else(|| piece_viewport_for_area(area, params));
    let title = match mode {
        PieceVisualState::Small => " Retry units ".to_string(),
        PieceVisualState::Medium => " Grid ".to_string(),
        PieceVisualState::Large => {
            format!(
                " Visible grid pieces {}-{} ",
                viewport.first_piece, viewport.last_piece
            )
        }
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(Style::default().fg(Color::Cyan));
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let total = params.total_pieces as usize;
    let cols = viewport.cols;
    let rows_needed = viewport.rows_needed;
    let visible_grid_rows = viewport.visible_rows;
    let scroll = viewport.scroll;

    let states = worker_states.lock().unwrap();
    let mut lines = Vec::with_capacity(inner.height as usize);

    for grid_row in 0..visible_grid_rows {
        let actual_row = grid_row + scroll as usize;
        if actual_row >= rows_needed {
            break;
        }

        let mut spans = Vec::with_capacity(cols);
        for col in 0..cols {
            let piece_idx = actual_row * cols + col;
            if piece_idx >= total {
                break;
            }

            let piece = &params.pieces[piece_idx];
            let is_selected = params.selected_piece == Some(piece_idx as u32);
            let is_retrying = states.iter().any(|w| {
                w.current_piece == Some(piece_idx as u32) && w.status == WorkerStatus::Retrying
            });
            let (glyph, color) = piece_glyph(piece.state, is_retrying, is_selected);
            if mode == PieceVisualState::Small {
                spans.push(Span::styled(
                    format!("{:02} ", piece.index),
                    Style::default().fg(color),
                ));
            } else {
                spans.push(Span::styled(glyph, Style::default().fg(color)));
            }
        }
        lines.push(Line::from(spans));
    }
    if lines.is_empty() {
        lines.push(Line::styled(
            "No pieces in visible range.",
            Style::default().fg(Color::DarkGray),
        ));
    }
    lines.push(piece_legend_line());
    drop(states);

    f.render_widget(Paragraph::new(lines), inner);

    if rows_needed > visible_grid_rows {
        let scroll_area = Rect {
            x: area.x + area.width.saturating_sub(1),
            y: area.y + 1,
            width: 1,
            height: area.height.saturating_sub(2),
        };
        let mut scrollbar_state = ScrollbarState::new(rows_needed).position(scroll as usize);
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight);
        f.render_stateful_widget(scrollbar, scroll_area, &mut scrollbar_state);
    }
}

pub fn render_recovery_queue(
    f: &mut Frame,
    area: Rect,
    params: &HeatMapParams,
    worker_states: &SharedWorkerStates,
    file_id: Option<FileId>,
) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Recovery queue ")
        .border_style(Style::default().fg(Color::Yellow));
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let lines = recovery_queue_lines(
        params,
        worker_states,
        file_id,
        inner.width as usize,
        inner.height as usize,
    );
    f.render_widget(Paragraph::new(lines), inner);
}

pub fn render_piece_workers_panel(
    f: &mut Frame,
    area: Rect,
    worker_states: &SharedWorkerStates,
    file_id: Option<FileId>,
) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Workers ")
        .border_style(Style::default().fg(Color::Yellow));
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let workers = worker_states.lock().unwrap();
    let mut lines = Vec::new();
    for worker in workers
        .iter()
        .filter(|worker| worker_matches_file(worker, file_id))
        .filter(|worker| worker.current_piece.is_some())
        .take(inner.height as usize)
    {
        let piece = worker.current_piece.unwrap_or_default();
        let downloaded = format_size_iec(worker.downloaded_bytes());
        let total = format_size_iec(worker.piece_length);
        let status = worker.status.to_string();
        lines.push(Line::from(vec![
            Span::styled(
                format!("{} ", display_worker_id(worker.id)),
                Style::default().fg(Color::Green),
            ),
            Span::styled(format!("#{piece:<4} "), Style::default().fg(Color::Cyan)),
            Span::styled(
                format!("{downloaded}/{total} "),
                Style::default().fg(Color::White),
            ),
            Span::styled(
                status,
                Style::default().fg(worker_status_color(worker.status)),
            ),
        ]));
    }
    drop(workers);

    if lines.is_empty() {
        lines.push(Line::styled(
            "No active workers for this file.",
            Style::default().fg(Color::DarkGray),
        ));
    }

    f.render_widget(Paragraph::new(lines), inner);
}

fn recovery_queue_lines(
    params: &HeatMapParams,
    worker_states: &SharedWorkerStates,
    file_id: Option<FileId>,
    width: usize,
    height: usize,
) -> Vec<Line<'static>> {
    let mut rows = Vec::new();
    let mut seen = Vec::new();
    let workers = worker_states.lock().unwrap();

    for worker in workers
        .iter()
        .filter(|worker| worker_matches_file(worker, file_id))
        .filter(|worker| worker.status == WorkerStatus::Retrying)
    {
        if let Some(piece) = worker.current_piece {
            seen.push(piece);
            rows.push(recovery_line(
                piece,
                format!(
                    "retry {}  {}",
                    worker.retries.max(1),
                    worker.last_error.as_deref().unwrap_or("retrying")
                ),
                Color::Red,
                width,
            ));
        }
    }

    for piece in params
        .pieces
        .iter()
        .filter(|piece| piece.state == PieceState::Failed)
    {
        if !seen.contains(&piece.index) {
            seen.push(piece.index);
            rows.push(recovery_line(
                piece.index,
                piece.last_error.as_deref().unwrap_or("failed").to_string(),
                Color::Red,
                width,
            ));
        }
    }

    for worker in workers
        .iter()
        .filter(|worker| worker_matches_file(worker, file_id))
        .filter(|worker| worker.status == WorkerStatus::Downloading)
    {
        if let Some(piece) = worker.current_piece
            && !seen.contains(&piece)
        {
            seen.push(piece);
            rows.push(recovery_line(
                piece,
                format!("active {}", display_worker_id(worker.id)),
                Color::Yellow,
                width,
            ));
        }
    }
    drop(workers);

    let anchor = params
        .selected_piece
        .unwrap_or_else(|| first_unfinished_piece(params.pieces).unwrap_or(0));
    let mut pending: Vec<_> = params
        .pieces
        .iter()
        .filter(|piece| piece.state == PieceState::Pending && !seen.contains(&piece.index))
        .collect();
    pending.sort_by_key(|piece| piece.index.abs_diff(anchor));
    for piece in pending.into_iter().take(2) {
        rows.push(recovery_line(
            piece.index,
            "pending nearest gap".to_string(),
            Color::DarkGray,
            width,
        ));
    }

    if rows.is_empty() {
        rows.push(Line::styled(
            format!(
                "No hot pieces. Redo bound {} per cell.",
                format_size_iec(params.piece_length as u64)
            ),
            Style::default().fg(Color::DarkGray),
        ));
    }

    rows.push(Line::from(vec![
        Span::styled("redo bound ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            format_size_iec(params.piece_length as u64),
            Style::default().fg(Color::White),
        ),
    ]));
    rows.truncate(height);
    rows
}

fn recovery_line(piece: u32, detail: String, color: Color, width: usize) -> Line<'static> {
    let detail_width = width.saturating_sub(8).max(8);
    Line::from(vec![
        Span::styled(format!("#{piece:<4} "), Style::default().fg(color)),
        Span::styled(
            truncate_chars(&detail, detail_width),
            Style::default().fg(color),
        ),
    ])
}

fn first_unfinished_piece(pieces: &[PieceSnapshot]) -> Option<u32> {
    pieces
        .iter()
        .find(|piece| piece.state != PieceState::Complete)
        .map(|piece| piece.index)
}

fn worker_matches_file(worker: &WorkerState, file_id: Option<FileId>) -> bool {
    file_id.is_none_or(|file_id| worker.file_id == Some(file_id))
}

pub(crate) fn effective_selected_piece(
    pieces: &[PieceSnapshot],
    selected_piece: Option<u32>,
    worker_states: &SharedWorkerStates,
    file_id: Option<FileId>,
) -> Option<u32> {
    selected_piece
        .filter(|piece| (*piece as usize) < pieces.len())
        .or_else(|| active_piece_for_file(worker_states, file_id))
        .or_else(|| {
            pieces
                .iter()
                .position(|piece| piece.state == PieceState::Pending)
                .map(|index| index as u32)
        })
        .or_else(|| {
            pieces
                .iter()
                .rposition(|piece| piece.state == PieceState::Complete)
                .map(|index| index as u32)
        })
}

fn active_piece_for_file(
    worker_states: &SharedWorkerStates,
    file_id: Option<FileId>,
) -> Option<u32> {
    let workers = worker_states.lock().unwrap();
    workers
        .iter()
        .filter(|worker| worker_matches_file(worker, file_id))
        .find(|worker| worker.status == WorkerStatus::Retrying && worker.current_piece.is_some())
        .or_else(|| {
            workers
                .iter()
                .filter(|worker| worker_matches_file(worker, file_id))
                .find(|worker| {
                    worker.status == WorkerStatus::Downloading && worker.current_piece.is_some()
                })
        })
        .and_then(|worker| worker.current_piece)
}

// ─── Main UI Layout ─────────────────────────────────────────────────────────

fn draw_ui(
    f: &mut Frame,
    data: &FrameData,
    target: &PieceGridTarget,
    app: &AppState,
    worker_states: &SharedWorkerStates,
    events: &SharedEventLog,
) {
    let area = f.area();
    if area.width < 60 || area.height < 16 {
        draw_small_terminal_message(f, area, data);
        return;
    }

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2), // status + tabs
            Constraint::Min(5),    // tab content
            Constraint::Length(1), // footer key hints
        ])
        .split(area);

    draw_header(f, chunks[0], data, app, worker_states);

    match app.active_tab {
        0 => draw_queue_tab(f, chunks[1], data, target, app, worker_states, events),
        1 => draw_file_tab(f, chunks[1], data, target, worker_states, app, events),
        2 => draw_pieces_tab(f, chunks[1], data, app, worker_states, events),
        3 => draw_workers_tab(f, chunks[1], data, app, worker_states),
        4 => draw_events_tab(f, chunks[1], app, events),
        5 => draw_summary_tab(f, chunks[1], data, target, events),
        _ => {}
    }

    draw_footer(f, chunks[2], data, app);

    if app.show_cancel_confirm {
        draw_cancel_overlay(f, area, "download");
    } else if app.show_help {
        draw_download_help_overlay(f, area, "Key Bindings");
    }
}

// ─── Header ─────────────────────────────────────────────────────────────────

fn draw_header(
    f: &mut Frame,
    area: Rect,
    data: &FrameData,
    app: &AppState,
    worker_states: &SharedWorkerStates,
) {
    let downloaded = data.downloaded_bytes;
    let pct = if data.total_length > 0 {
        downloaded as f64 / data.total_length as f64
    } else {
        0.0
    };

    let progress_bar_width = 20usize;
    let filled = (progress_bar_width as f64 * pct).round() as usize;
    let filled = filled.min(progress_bar_width);
    let empty = progress_bar_width - filled;
    let progress_bar = format!("{}{}", "━".repeat(filled), "─".repeat(empty));

    let line1 = Line::from(vec![
        Span::styled(" aioduct download ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            truncate_chars(&data.filename, area.width.saturating_sub(48) as usize),
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
        Span::styled(
            format!("  {}", format_speed(data.speed_bps)),
            Style::default().fg(Color::Cyan),
        ),
        Span::styled(
            format!(
                "  ETA {}",
                eta_label(data.downloaded_bytes, data.total_length, data.speed_bps)
            ),
            Style::default().fg(Color::Magenta),
        ),
        Span::styled(
            format!("  {} workers", data.num_workers),
            Style::default().fg(Color::Yellow),
        ),
    ]);

    let (has_failures, has_retries) = tab_alerts(data, worker_states);
    let mut tab_spans: Vec<Span> = vec![Span::raw(" ")];
    for (i, name) in DOWNLOAD_TABS.iter().enumerate() {
        if i > 0 {
            tab_spans.push(Span::raw("  "));
        }
        let alert_color = if has_failures && matches!(i, 1 | 4 | 5) {
            Some(Color::Red)
        } else if has_retries && matches!(i, 2..=4) {
            Some(Color::Yellow)
        } else {
            None
        };
        let marker = match alert_color {
            Some(Color::Red) => "!",
            Some(Color::Yellow) => "*",
            _ => "",
        };
        if i == app.active_tab {
            tab_spans.push(Span::styled(
                format!("{} {name}{marker}", i + 1),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ));
        } else {
            tab_spans.push(Span::styled(
                format!("{} {name}{marker}", i + 1),
                Style::default().fg(alert_color.unwrap_or(Color::DarkGray)),
            ));
        }
    }

    let paragraph = Paragraph::new(vec![line1, Line::from(tab_spans)]);
    f.render_widget(paragraph, area);
}

// ─── Tab 1: Queue ───────────────────────────────────────────────────────────

fn draw_queue_tab(
    f: &mut Frame,
    area: Rect,
    data: &FrameData,
    target: &PieceGridTarget,
    app: &AppState,
    worker_states: &SharedWorkerStates,
    events: &SharedEventLog,
) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(6), Constraint::Length(4)])
        .split(area);
    let chunks = if rows[0].width >= 100 {
        Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(58), Constraint::Percentage(42)])
            .split(rows[0])
    } else {
        Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Percentage(56), Constraint::Percentage(44)])
            .split(rows[0])
    };
    let bottom = if rows[1].width >= 100 {
        Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(58), Constraint::Percentage(42)])
            .split(rows[1])
    } else {
        Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
            .split(rows[1])
    };

    let pct = percent(data.downloaded_bytes, data.total_length);
    let in_flight = data
        .pieces
        .iter()
        .filter(|p| p.state == PieceState::InFlight)
        .count();
    let pending = data
        .pieces
        .iter()
        .filter(|p| p.state == PieceState::Pending)
        .count();
    let status = if data.completed == data.total_pieces {
        ("complete", Color::Green)
    } else if in_flight > 0 {
        ("running", Color::Yellow)
    } else {
        ("queued", Color::DarkGray)
    };

    let file_lines = vec![
        Line::from(vec![
            Span::styled("› ", Style::default().fg(Color::Cyan)),
            Span::styled(
                if data.completed == data.total_pieces {
                    "✓ "
                } else if in_flight > 0 {
                    "● "
                } else {
                    "○ "
                },
                Style::default().fg(status.1),
            ),
            Span::styled(
                truncate_chars(&data.filename, chunks[0].width.saturating_sub(24) as usize),
                Style::default().fg(Color::White),
            ),
            Span::styled(
                format!("  {:>5.1}%", pct),
                Style::default().fg(Color::Yellow),
            ),
        ]),
        Line::raw(""),
        detail_line(
            "Bytes",
            format!(
                "{}/{}",
                format_size_iec(data.downloaded_bytes),
                format_size_iec(data.total_length)
            ),
            Color::Cyan,
        ),
        detail_line("Workers", data.num_workers.to_string(), Color::White),
        detail_line("Speed", format_speed(data.speed_bps), Color::Cyan),
    ];

    let file_block = Block::default()
        .borders(Borders::ALL)
        .title(" Files ")
        .border_style(Style::default().fg(Color::Cyan));
    f.render_widget(Paragraph::new(file_lines).block(file_block), chunks[0]);

    let selected_lines = vec![
        Line::styled(
            truncate_chars(&target.filename, chunks[1].width.saturating_sub(4) as usize),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
        Line::from(vec![
            Span::styled(progress_bar(pct, 30), Style::default().fg(status.1)),
            Span::styled(format!(" {pct:>5.1}%"), Style::default().fg(Color::Yellow)),
        ]),
        detail_line("Status", status.0, status.1),
        detail_line(
            "Pieces",
            format!("{}/{} complete", data.completed, data.total_pieces),
            Color::White,
        ),
        detail_line("Active pieces", in_flight.to_string(), Color::Yellow),
        detail_line("Pending pieces", pending.to_string(), Color::DarkGray),
        detail_line(
            "Range",
            if target.supports_range { "yes" } else { "no" },
            Color::White,
        ),
        detail_line("Output", target.output.display().to_string(), Color::Cyan),
    ];

    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Selected ")
        .border_style(Style::default().fg(Color::Cyan));
    f.render_widget(Paragraph::new(selected_lines).block(block), chunks[1]);

    draw_active_workers_panel(f, bottom[0], worker_states, app);
    draw_event_tail(f, bottom[1], events);
}

fn draw_small_terminal_message(f: &mut Frame, area: Rect, data: &FrameData) {
    let pct = percent(data.downloaded_bytes, data.total_length);
    let lines = vec![
        Line::styled(
            "aioduct download is still running",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Line::raw(""),
        detail_line(
            "progress",
            format!(
                "{pct:.1}%  {}/{}",
                format_size_compact(data.downloaded_bytes),
                format_size_compact(data.total_length)
            ),
            Color::Yellow,
        ),
        detail_line("speed", format_speed(data.speed_bps), Color::Cyan),
        detail_line("need", "terminal >= 60x16", Color::DarkGray),
    ];
    f.render_widget(Paragraph::new(lines), area);
}

// ─── Tab 2: File ────────────────────────────────────────────────────────────

fn draw_file_tab(
    f: &mut Frame,
    area: Rect,
    data: &FrameData,
    target: &PieceGridTarget,
    worker_states: &SharedWorkerStates,
    app: &AppState,
    events: &SharedEventLog,
) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(5),
            Constraint::Min(6),
            Constraint::Length(4),
        ])
        .split(area);
    let pct = percent(data.downloaded_bytes, data.total_length);
    let in_flight = data
        .pieces
        .iter()
        .filter(|p| p.state == PieceState::InFlight)
        .count();
    let pending = data
        .pieces
        .iter()
        .filter(|p| p.state == PieceState::Pending)
        .count();
    let progress_lines = vec![
        Line::from(vec![
            Span::styled(
                "Progress ",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!(
                    "{}/{}",
                    format_size_iec(data.downloaded_bytes),
                    format_size_iec(data.total_length)
                ),
                Style::default().fg(Color::White),
            ),
            Span::styled(format!("  {pct:.1}%"), Style::default().fg(Color::Yellow)),
        ]),
        Line::from(vec![Span::styled(
            progress_bar(pct, rows[0].width.saturating_sub(8) as usize),
            Style::default().fg(Color::Green),
        )]),
        Line::styled(
            format!(
                "pieces {} complete, {} active, {} pending  {}",
                data.completed,
                in_flight,
                pending,
                piece_size_policy_label(data.piece_length)
            ),
            Style::default().fg(Color::DarkGray),
        ),
    ];
    f.render_widget(
        Paragraph::new(progress_lines).block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Progress ")
                .border_style(Style::default().fg(Color::Cyan)),
        ),
        rows[0],
    );

    let chunks = if rows[1].width >= 100 {
        Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(62), Constraint::Percentage(38)])
            .split(rows[1])
    } else {
        Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Percentage(60), Constraint::Percentage(40)])
            .split(rows[1])
    };

    let mut source_lines = vec![
        detail_line("Workers", data.num_workers.to_string(), Color::White),
        detail_line(
            "Range",
            if target.supports_range {
                "supported"
            } else {
                "no"
            },
            if target.supports_range {
                Color::Green
            } else {
                Color::Red
            },
        ),
        detail_line("URL", target.url.clone(), Color::Cyan),
    ];
    if let Some(etag) = &target.etag {
        source_lines.push(detail_line("ETag", etag.clone(), Color::DarkGray));
    }
    if let Some(last_modified) = &target.last_modified {
        source_lines.push(detail_line(
            "Modified",
            last_modified.clone(),
            Color::DarkGray,
        ));
    }

    let output_lines = vec![
        detail_line("Output", target.output.display().to_string(), Color::Cyan),
        detail_line(
            "Control",
            target.control_path.display().to_string(),
            Color::DarkGray,
        ),
        detail_line(
            "Resume",
            if target.resume_skipped_pieces > 0 {
                format!("yes, skipped {} pieces", target.resume_skipped_pieces)
            } else {
                "ready".to_string()
            },
            Color::Green,
        ),
        detail_line("Allocation", target.allocation, Color::Green),
        detail_line(
            "Checksum",
            super::checksum::read_status(&target.checksum_status),
            Color::DarkGray,
        ),
        detail_line("Created", target.created_at.clone(), Color::DarkGray),
    ];

    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Source ")
        .border_style(Style::default().fg(Color::Cyan));
    f.render_widget(Paragraph::new(source_lines).block(block), chunks[0]);

    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Output ")
        .border_style(Style::default().fg(Color::Cyan));
    f.render_widget(Paragraph::new(output_lines).block(block), chunks[1]);

    let footer = if rows[2].width >= 92 {
        Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(68), Constraint::Percentage(32)])
            .split(rows[2])
    } else {
        Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
            .split(rows[2])
    };
    draw_active_workers_panel(f, footer[0], worker_states, app);
    draw_event_tail(f, footer[1], events);
}

fn draw_active_workers_panel(
    f: &mut Frame,
    area: Rect,
    worker_states: &SharedWorkerStates,
    app: &AppState,
) {
    let states = worker_states.lock().unwrap();
    let mut lines = Vec::new();
    for worker in states.iter().filter(|w| w.current_piece.is_some()) {
        let piece = worker
            .current_piece
            .map(|p| format!("#{p}"))
            .unwrap_or_else(|| "-".into());
        let pct = if worker.piece_length > 0 {
            percent(worker.downloaded_bytes(), worker.piece_length)
        } else {
            0.0
        };
        let live_speed = app.worker_speed(worker.id);
        let speed = if live_speed > 0.0 {
            format_speed_compact(live_speed)
        } else if worker.speed_bps > 0.0 {
            format_speed_compact(worker.speed_bps)
        } else {
            "---".into()
        };
        lines.push(Line::from(vec![
            Span::styled(
                format!("{} ", display_worker_id(worker.id)),
                Style::default().fg(Color::Cyan),
            ),
            Span::styled(format!("{piece:<8}"), Style::default().fg(Color::Yellow)),
            Span::styled(format!("{pct:>5.1}% "), Style::default().fg(Color::Green)),
            Span::styled(format!("{speed:<9} "), Style::default().fg(Color::White)),
            Span::styled(
                worker.status.to_string(),
                Style::default().fg(worker_status_color(worker.status)),
            ),
        ]));
    }
    if lines.is_empty() {
        lines.push(Line::styled(
            "No active workers.",
            Style::default().fg(Color::DarkGray),
        ));
    }

    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Active Workers ")
        .border_style(Style::default().fg(Color::Yellow));
    f.render_widget(Paragraph::new(lines).block(block), area);
}

// ─── Tab 3: Pieces ──────────────────────────────────────────────────────────

fn draw_pieces_tab(
    f: &mut Frame,
    area: Rect,
    data: &FrameData,
    app: &AppState,
    worker_states: &SharedWorkerStates,
    events: &SharedEventLog,
) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(6),
        ])
        .split(area);

    let selected_piece =
        effective_selected_piece(&data.pieces, app.selected_piece, worker_states, None);
    let mut params = HeatMapParams {
        pieces: &data.pieces,
        total_pieces: data.total_pieces,
        piece_length: data.piece_length,
        scroll_offset: app.scroll_offset,
        frame_count: app.frame_count,
        selected_piece,
        viewport: None,
    };
    let content = chunks[2];
    if content.width >= 92 {
        let grid_width = ((content.width as u32 * 68) / 100) as u16;
        let top_height = piece_grid_panel_height(grid_width, params.total_pieces)
            .min(content.height)
            .max(1);
        let rows = if content.height > top_height.saturating_add(6) {
            Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Length(top_height), Constraint::Min(5)])
                .split(content)
        } else {
            Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Percentage(100)])
                .split(content)
        };
        let panes = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(68), Constraint::Percentage(32)])
            .split(rows[0]);
        params.viewport = Some(piece_viewport_for_area(panes[0], &params));
        render_piece_policy_line(f, chunks[0], &params);
        render_overview_bar(f, chunks[1], &params, worker_states);
        render_heat_map(f, panes[0], &params, worker_states);
        render_recovery_queue(f, panes[1], &params, worker_states, None);

        if rows.len() > 1 {
            let bottom = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
                .split(rows[1]);
            draw_piece_detail_panel(f, bottom[0], data, selected_piece, worker_states);
            render_piece_workers_panel(f, bottom[1], worker_states, None);
        }
    } else if content.height >= PIECE_DETAIL_MIN_HEIGHT.saturating_add(8) {
        let grid_height = piece_grid_panel_height(content.width, params.total_pieces)
            .min(content.height)
            .max(1);
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(grid_height),
                Constraint::Length(5),
                Constraint::Length(PIECE_DETAIL_MIN_HEIGHT),
                Constraint::Min(3),
            ])
            .split(content);
        params.viewport = Some(piece_viewport_for_area(rows[0], &params));
        render_piece_policy_line(f, chunks[0], &params);
        render_overview_bar(f, chunks[1], &params, worker_states);
        render_heat_map(f, rows[0], &params, worker_states);
        render_recovery_queue(f, rows[1], &params, worker_states, None);
        draw_piece_detail_panel(f, rows[2], data, selected_piece, worker_states);
        if rows[3].height > 0 {
            draw_event_tail(f, rows[3], events);
        }
    } else {
        params.viewport = Some(piece_viewport_for_area(content, &params));
        render_piece_policy_line(f, chunks[0], &params);
        render_overview_bar(f, chunks[1], &params, worker_states);
        render_heat_map(f, content, &params, worker_states);
    }
}

fn draw_piece_detail_panel(
    f: &mut Frame,
    area: Rect,
    data: &FrameData,
    selected_piece: Option<u32>,
    worker_states: &SharedWorkerStates,
) {
    let workers = worker_states.lock().unwrap();
    let active = workers
        .iter()
        .find_map(|worker| worker.current_piece.map(|piece| (piece, Some(worker))));
    let selected = selected_piece
        .filter(|piece| (*piece as usize) < data.pieces.len())
        .map(|piece| {
            let worker = workers
                .iter()
                .find(|worker| worker.current_piece == Some(piece));
            (piece, worker)
        })
        .or(active)
        .or_else(|| {
            data.pieces
                .iter()
                .position(|p| p.state == PieceState::Pending)
                .map(|idx| (idx as u32, None))
        })
        .or_else(|| {
            data.pieces
                .iter()
                .rposition(|p| p.state == PieceState::Complete)
                .map(|idx| (idx as u32, None))
        });

    let mut lines = Vec::new();

    if let Some((piece, worker)) = selected {
        let snapshot = data
            .pieces
            .get(piece as usize)
            .cloned()
            .unwrap_or(PieceSnapshot {
                index: piece,
                state: PieceState::Pending,
                retry_count: 0,
                last_error: None,
            });
        let state = snapshot.state;
        let start = piece as u64 * data.piece_length as u64;
        let end = (start + data.piece_length as u64)
            .saturating_sub(1)
            .min(data.total_length.saturating_sub(1));
        let is_retrying = worker
            .as_ref()
            .is_some_and(|w| w.status == WorkerStatus::Retrying);
        let (status, color) = match state {
            PieceState::Complete => ("complete", Color::Green),
            PieceState::InFlight => {
                if is_retrying {
                    ("retrying", Color::Yellow)
                } else {
                    ("active", Color::Yellow)
                }
            }
            PieceState::Failed => ("failed", Color::Red),
            PieceState::Pending => ("pending", Color::DarkGray),
        };
        let worker_label = worker
            .as_ref()
            .map(|w| display_worker_id(w.id))
            .unwrap_or_else(|| "-".into());
        let progress = worker
            .as_ref()
            .map(|w| {
                format!(
                    "{}/{}",
                    format_size_iec(w.downloaded_bytes()),
                    format_size_iec(w.piece_length)
                )
            })
            .unwrap_or_else(|| "-".into());
        let retries = worker
            .as_ref()
            .map(|w| {
                if w.retries > 0 {
                    w.retries.to_string()
                } else {
                    snapshot.retry_count.to_string()
                }
            })
            .unwrap_or_else(|| snapshot.retry_count.to_string());
        let last_error = worker
            .as_ref()
            .and_then(|w| w.last_error.clone())
            .or_else(|| snapshot.last_error.clone())
            .unwrap_or_else(|| "-".into());

        lines.extend([
            detail_line("selected", format!("#{}", snapshot.index), Color::Cyan),
            detail_line("status", status, color),
            detail_line("worker", worker_label, Color::White),
            detail_line(
                "size",
                piece_size_policy_label(data.piece_length),
                Color::White,
            ),
            detail_line(
                "range",
                format!("{}-{}", format_size_iec(start), format_size_iec(end)),
                Color::White,
            ),
            detail_line("progress", progress, Color::Yellow),
            detail_line("retries", retries, Color::White),
            detail_line(
                "redo bound",
                format_size_iec(data.piece_length as u64),
                Color::DarkGray,
            ),
            detail_line("last error", last_error, Color::Red),
        ]);
    } else {
        lines.push(Line::styled(
            "No pieces available.",
            Style::default().fg(Color::DarkGray),
        ));
    }
    drop(workers);

    let block = Block::default()
        .borders(Borders::ALL)
        .title(PIECE_DETAIL_TITLE)
        .border_style(Style::default().fg(Color::Cyan));
    f.render_widget(Paragraph::new(lines).block(block), area);
}

fn piece_glyph(state: PieceState, retrying: bool, selected: bool) -> (&'static str, Color) {
    if selected {
        return ("█", Color::Cyan);
    }
    if retrying {
        return ("▒", Color::Red);
    }
    match state {
        PieceState::Pending => ("░", Color::DarkGray),
        PieceState::InFlight => ("▓", Color::Yellow),
        PieceState::Complete => ("█", Color::Green),
        PieceState::Failed => ("▒", Color::Red),
    }
}

fn piece_legend_line() -> Line<'static> {
    Line::from(vec![
        Span::styled(" legend ", Style::default().fg(Color::DarkGray)),
        Span::styled("complete ", Style::default().fg(Color::Green)),
        Span::styled("active ", Style::default().fg(Color::Yellow)),
        Span::styled("retry ", Style::default().fg(Color::Red)),
        Span::styled("pending ", Style::default().fg(Color::DarkGray)),
        Span::styled("selected", Style::default().fg(Color::Cyan)),
    ])
}

// ─── Tab 4: Workers ─────────────────────────────────────────────────────────

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
    let downloading = states
        .iter()
        .filter(|w| w.status == WorkerStatus::Downloading)
        .count();
    let retrying = states
        .iter()
        .filter(|w| w.status == WorkerStatus::Retrying)
        .count();
    let idle = states
        .iter()
        .filter(|w| w.status == WorkerStatus::Idle)
        .count();
    let mut lines = vec![
        Line::from(vec![
            Span::styled(
                format!("{} workers", states.len()),
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("  {downloading} downloading"),
                Style::default().fg(Color::Green),
            ),
            Span::styled(
                format!("  {retrying} retrying"),
                Style::default().fg(if retrying > 0 {
                    Color::Yellow
                } else {
                    Color::DarkGray
                }),
            ),
            Span::styled(
                format!("  {idle} idle"),
                Style::default().fg(Color::DarkGray),
            ),
            Span::styled(
                format!("  {}", format_speed(data.speed_bps)),
                Style::default().fg(Color::Cyan),
            ),
        ]),
        Line::raw(""),
        Line::from(vec![Span::styled(
            format!(
                "{:<4} {:<7} {:<20} {:<12} {:<4} {}",
                "ID", "Piece", "Progress", "Speed", "Ret", "Status"
            ),
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )]),
    ];

    let mut ordered: Vec<_> = states.iter().collect();
    if app.worker_sort_by_id {
        ordered.sort_by_key(|w| w.id);
    } else {
        ordered.sort_by_key(|w| worker_sort_key(w.status, w.id));
    }
    for w in ordered {
        let is_selected = w.id == app.selected_worker;
        let indicator = if is_selected { "›" } else { " " };
        let piece_str = w
            .current_piece
            .map(|p| format!("#{p}"))
            .unwrap_or_else(|| "-".to_string());
        let ratio = if w.piece_length > 0 && w.current_piece.is_some() {
            (w.downloaded_bytes() as f64 / w.piece_length as f64).min(1.0)
        } else {
            0.0
        };
        let bar_width = 10usize;
        let filled = (ratio * bar_width as f64) as usize;
        let progress_str = if w.current_piece.is_some() {
            format!(
                "{}{} {:>3.0}%",
                "━".repeat(filled),
                "─".repeat(bar_width - filled),
                ratio * 100.0
            )
        } else {
            "waiting".to_string()
        };
        let live_speed = app.worker_speed(w.id);
        let speed_str = if live_speed > 0.0 {
            format_speed(live_speed)
        } else if w.speed_bps > 0.0 {
            format_speed(w.speed_bps)
        } else if w.status == WorkerStatus::Retrying {
            "retrying".to_string()
        } else {
            "-".to_string()
        };
        let retries_str = if w.retries > 0 {
            w.retries.to_string()
        } else {
            "-".to_string()
        };
        lines.push(Line::from(vec![
            Span::styled(format!("{indicator} "), Style::default().fg(Color::Cyan)),
            Span::styled(
                format!("{:<4} ", display_worker_id(w.id)),
                Style::default().fg(Color::Cyan),
            ),
            Span::styled(
                format!("{piece_str:<7} "),
                Style::default().fg(Color::Yellow),
            ),
            Span::styled(
                format!("{progress_str:<20} "),
                Style::default().fg(if w.status == WorkerStatus::Retrying {
                    Color::Yellow
                } else {
                    Color::Green
                }),
            ),
            Span::styled(
                format!("{speed_str:<12} "),
                Style::default().fg(Color::Cyan),
            ),
            Span::styled(
                format!("{retries_str:<4} "),
                Style::default().fg(Color::White),
            ),
            Span::styled(
                w.status.to_string(),
                Style::default().fg(worker_status_color(w.status)),
            ),
        ]));
    }

    if let Some(selected) = states
        .iter()
        .find(|w| w.id == app.selected_worker)
        .or_else(|| {
            states.iter().find(|w| {
                w.status == WorkerStatus::Retrying || w.status == WorkerStatus::Downloading
            })
        })
    {
        lines.push(Line::raw(""));
        lines.push(Line::from(vec![
            Span::styled("selected ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                display_worker_id(selected.id),
                Style::default().fg(Color::Cyan),
            ),
            Span::styled(
                format!(
                    "  piece {}",
                    selected
                        .current_piece
                        .map(|p| format!("#{p}"))
                        .unwrap_or_else(|| "-".into())
                ),
                Style::default().fg(Color::Yellow),
            ),
            Span::styled(
                format!(
                    "  age {}",
                    selected
                        .assignment_age()
                        .map(format_duration_compact)
                        .unwrap_or_else(|| "-".into())
                ),
                Style::default().fg(Color::DarkGray),
            ),
            Span::styled(
                format!(
                    "  last error {}",
                    selected.last_error.as_deref().unwrap_or("-")
                ),
                Style::default().fg(if selected.last_error.is_some() {
                    Color::Red
                } else {
                    Color::DarkGray
                }),
            ),
        ]));
    }

    f.render_widget(Paragraph::new(lines), inner);
}

// ─── Tab 5: Events ──────────────────────────────────────────────────────────

fn draw_events_tab(f: &mut Frame, area: Rect, app: &AppState, events: &SharedEventLog) {
    draw_event_log(f, area, app, events, "Events");
}

// ─── Tab 6: Summary ─────────────────────────────────────────────────────────

fn draw_summary_tab(
    f: &mut Frame,
    area: Rect,
    data: &FrameData,
    target: &PieceGridTarget,
    events: &SharedEventLog,
) {
    let pending = data
        .pieces
        .iter()
        .filter(|p| p.state == PieceState::Pending)
        .count();
    let in_flight = data
        .pieces
        .iter()
        .filter(|p| p.state == PieceState::InFlight)
        .count();
    let pct = percent(data.downloaded_bytes, data.total_length);
    let done = data.completed == data.total_pieces && data.total_pieces > 0;

    let mut lines = vec![
        Line::styled(
            if done { "complete" } else { "running" },
            Style::default()
                .fg(if done { Color::Green } else { Color::Yellow })
                .add_modifier(Modifier::BOLD),
        ),
        Line::raw(""),
        detail_line("File", target.filename.clone(), Color::White),
        detail_line("Progress", format!("{pct:.1}%"), Color::Yellow),
        detail_line(
            "Bytes",
            format!(
                "{}/{}",
                format_size_compact(data.downloaded_bytes),
                format_size_compact(data.total_length)
            ),
            Color::Cyan,
        ),
        detail_line(
            "Pieces",
            format!("{}/{} complete", data.completed, data.total_pieces),
            Color::White,
        ),
        detail_line("In flight", in_flight.to_string(), Color::Yellow),
        detail_line("Pending", pending.to_string(), Color::DarkGray),
        detail_line("Speed", format_speed(data.speed_bps), Color::Cyan),
        detail_line("Output", target.output.display().to_string(), Color::Cyan),
        detail_line(
            "Control",
            target.control_path.display().to_string(),
            Color::DarkGray,
        ),
        detail_line(
            "Integrity",
            super::checksum::read_status(&target.checksum_status),
            Color::DarkGray,
        ),
    ];

    let events_lock = events.lock().unwrap();
    if let Some(last) = events_lock.back().map(DownloadEvent::display_line) {
        lines.push(Line::raw(""));
        lines.push(Line::styled(
            "Latest Event",
            Style::default().fg(Color::DarkGray),
        ));
        lines.push(Line::raw(last));
    }
    drop(events_lock);

    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Summary ")
        .border_style(Style::default().fg(Color::Green));
    f.render_widget(Paragraph::new(lines).block(block), area);
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

fn progress_bar(pct: f64, width: usize) -> String {
    let width = width.clamp(4, 48);
    let ratio = (pct / 100.0).clamp(0.0, 1.0);
    let filled = (width as f64 * ratio).round() as usize;
    format!("{}{}", "━".repeat(filled), "─".repeat(width - filled))
}

/// Split `line` into visual rows at `col_width` char boundaries,
/// returning borrowed `Line`s to avoid allocations.
pub(crate) fn wrap_line(line: &str, col_width: usize) -> Vec<Line<'_>> {
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

fn draw_event_tail(f: &mut Frame, area: Rect, events: &SharedEventLog) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Latest Events ")
        .border_style(Style::default().fg(Color::Yellow));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let log = events.lock().unwrap();
    let visible = inner.height as usize;
    let lines: Vec<Line> = if log.is_empty() {
        vec![Line::styled(
            "No events yet.",
            Style::default().fg(Color::DarkGray),
        )]
    } else {
        log.iter()
            .rev()
            .take(visible)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .map(DownloadEvent::display_line)
            .map(Line::raw)
            .collect()
    };
    drop(log);

    f.render_widget(Paragraph::new(lines), inner);
}

fn draw_event_log(f: &mut Frame, area: Rect, app: &AppState, events: &SharedEventLog, title: &str) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" {title} "))
        .border_style(Style::default().fg(Color::Yellow));

    let inner = block.inner(area);
    f.render_widget(block, area);

    let log = events.lock().unwrap();
    let raw_events: Vec<DownloadEvent> = log.iter().cloned().collect();
    drop(log);
    let filtered_events: Vec<DownloadEvent> = raw_events
        .iter()
        .filter(|event| {
            app.event_filter
                .matches(event, None, Some(app.selected_worker))
        })
        .filter(|event| event_matches_query(event, &app.filter_query))
        .cloned()
        .collect();

    let retry_count = raw_events
        .iter()
        .filter(|event| event.severity == super::tui_state::EventSeverity::Retry)
        .count();
    let failure_count = raw_events
        .iter()
        .filter(|event| event.severity == super::tui_state::EventSeverity::Error)
        .count();
    let raw_lines: Vec<String> = filtered_events
        .iter()
        .map(DownloadEvent::display_line)
        .collect();
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(inner);
    let header = vec![
        Line::from(vec![
            Span::styled("filter ", Style::default().fg(Color::DarkGray)),
            Span::styled(event_filter_label(app), Style::default().fg(Color::Cyan)),
            Span::styled("  text ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                if app.filter_query.is_empty() {
                    "-".to_string()
                } else if app.editing_filter {
                    format!("{}/", app.filter_query)
                } else {
                    app.filter_query.clone()
                },
                Style::default().fg(if app.editing_filter {
                    Color::Yellow
                } else {
                    Color::Cyan
                }),
            ),
            Span::styled(
                format!(
                    "  showing {} / {} retained",
                    filtered_events.len(),
                    raw_events.len()
                ),
                Style::default().fg(Color::DarkGray),
            ),
            Span::styled(
                format!("  retries {retry_count}"),
                Style::default().fg(if retry_count > 0 {
                    Color::Yellow
                } else {
                    Color::DarkGray
                }),
            ),
            Span::styled(
                format!("  failures {failure_count}"),
                Style::default().fg(if failure_count > 0 {
                    Color::Red
                } else {
                    Color::DarkGray
                }),
            ),
        ]),
        Line::raw(""),
    ];
    f.render_widget(Paragraph::new(header), rows[0]);

    let visible = rows[1].height as usize;
    let col_w = rows[1].width.max(1) as usize;

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
    f.render_widget(Paragraph::new(visible_lines), rows[1]);
    f.render_widget(
        Paragraph::new(Line::styled(
            if app.editing_filter {
                "type to filter text, Enter apply, Esc close"
            } else {
                "filters: a all  f failures  r retries  w worker  s selected file  / text  y copy"
            },
            Style::default().fg(Color::DarkGray),
        )),
        rows[2],
    );

    if total > visible {
        let mut scrollbar_state = ScrollbarState::new(total).position(scroll);
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight);
        f.render_stateful_widget(scrollbar, rows[1], &mut scrollbar_state);
    }
}

fn event_matches_query(event: &DownloadEvent, filter_query: &str) -> bool {
    let query = filter_query.trim().to_ascii_lowercase();
    if query.is_empty() {
        return true;
    }
    event.display_line().to_ascii_lowercase().contains(&query)
}

fn event_filter_label(app: &AppState) -> String {
    match app.event_filter {
        EventFilter::Worker => format!("worker {}", display_worker_id(app.selected_worker)),
        _ => app.event_filter.label().to_string(),
    }
}

fn copy_visible_text(target: &PieceGridTarget, app: &AppState, events: &SharedEventLog) {
    let text = match app.active_tab {
        4 => filtered_event_text(app, events),
        5 => summary_text(target, events),
        _ => format!(
            "{}\nchecksum: {}\noutput: {}\nurl: {}",
            target.filename,
            super::checksum::read_status(&target.checksum_status),
            target.output.display(),
            target.url
        ),
    };
    match copy_to_clipboard(&text) {
        Ok(()) => push_ui_event(events, EventSeverity::Info, "copied visible download text"),
        Err(e) => push_ui_event(events, EventSeverity::Error, format!("copy failed: {e}")),
    }
}

fn filtered_event_text(app: &AppState, events: &SharedEventLog) -> String {
    let log = events.lock().unwrap();
    let lines: Vec<String> = log
        .iter()
        .filter(|event| {
            app.event_filter
                .matches(event, None, Some(app.selected_worker))
        })
        .filter(|event| event_matches_query(event, &app.filter_query))
        .map(DownloadEvent::display_line)
        .collect();
    if lines.is_empty() {
        "No events matched.".to_string()
    } else {
        lines.join("\n")
    }
}

fn summary_text(target: &PieceGridTarget, events: &SharedEventLog) -> String {
    let log = events.lock().unwrap();
    let mut lines = vec![format!(
        "download summary: {}\nchecksum: {}\noutput: {}\nurl: {}",
        target.filename,
        super::checksum::read_status(&target.checksum_status),
        target.output.display(),
        target.url
    )];
    if let Some(last) = log.back().map(DownloadEvent::display_line) {
        lines.push(format!("latest event: {last}"));
    }
    lines.join("\n")
}

fn open_output_dir(target: &PieceGridTarget, events: &SharedEventLog) {
    let path = target
        .output
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."));
    match open_path(path) {
        Ok(()) => push_ui_event(
            events,
            EventSeverity::Info,
            format!("opened output dir {}", path.display()),
        ),
        Err(e) => push_ui_event(events, EventSeverity::Error, format!("open failed: {e}")),
    }
}

// ─── Footer ─────────────────────────────────────────────────────────────────

fn draw_footer(f: &mut Frame, area: Rect, data: &FrameData, app: &AppState) {
    let quit_hint = if data.downloaded_bytes >= data.total_length && data.total_length > 0 {
        "q close"
    } else {
        "q cancel"
    };
    let hint = if app.editing_filter {
        format!("filter: {}_   Enter apply   Esc close", app.filter_query)
    } else {
        match app.active_tab {
            0 => format!(
                "1-6 pages   Tab focus:{}   Enter file   / filter   f failed   o output   {quit_hint}   ? help",
                focus_label(app.active_tab, app.focus_index)
            ),
            1 => format!(
                "1-6 pages   Tab focus:{}   3 pieces   4 workers   y copy path   o output   {quit_hint}   ? help",
                focus_label(app.active_tab, app.focus_index)
            ),
            2 => format!(
                "1-6 pages   ←/→ select piece   j/k scroll rows   Enter workers   y copy visible   {quit_hint}   ? help"
            ),
            3 => format!(
                "1-6 pages   ↑/↓ select worker   Enter jump to piece   s sort:{}   w worker events   {quit_hint}   ? help",
                if app.worker_sort_by_id { "id" } else { "state" }
            ),
            4 => format!(
                "1-6 pages   j/k scroll   g/G top/bottom   y copy visible   a/f/r/w/s filters   {quit_hint}   ? help"
            ),
            5 => format!("1-6 pages   y copy summary   o open output dir   {quit_hint}   ? help"),
            _ => format!("1-6 pages   {quit_hint}   ? help"),
        }
    };
    let line = Line::styled(hint, Style::default().fg(Color::DarkGray));
    f.render_widget(Paragraph::new(line), area);
}

fn focus_count(active_tab: usize) -> usize {
    match active_tab {
        0 => 4,
        1 => 2,
        3 => 2,
        _ => 1,
    }
}

fn focus_label(active_tab: usize, focus_index: usize) -> &'static str {
    match active_tab {
        0 => match focus_index % focus_count(active_tab) {
            0 => "file",
            1 => "selected",
            2 => "workers",
            _ => "events",
        },
        1 => match focus_index % focus_count(active_tab) {
            0 => "details",
            _ => "workers",
        },
        3 => match focus_index % focus_count(active_tab) {
            0 => "table",
            _ => "detail",
        },
        _ => "main",
    }
}

fn selected_worker_piece(
    worker_states: &SharedWorkerStates,
    selected_worker: usize,
) -> Option<u32> {
    worker_states
        .lock()
        .unwrap()
        .iter()
        .find(|worker| worker.id == selected_worker)
        .and_then(|worker| worker.current_piece)
}

fn active_worker_piece(worker_states: &SharedWorkerStates) -> Option<u32> {
    worker_states
        .lock()
        .unwrap()
        .iter()
        .find(|worker| {
            worker.status == WorkerStatus::Downloading || worker.status == WorkerStatus::Retrying
        })
        .and_then(|worker| worker.current_piece)
}

fn worker_order(worker_states: &SharedWorkerStates, sort_by_id: bool) -> Vec<usize> {
    let workers = worker_states.lock().unwrap();
    let mut ordered: Vec<&super::tui_state::WorkerState> = workers.iter().collect();
    if sort_by_id {
        ordered.sort_by_key(|worker| worker.id);
    } else {
        ordered.sort_by_key(|worker| worker_sort_key(worker.status, worker.id));
    }
    ordered.into_iter().map(|worker| worker.id).collect()
}

fn worker_sort_key(status: WorkerStatus, id: usize) -> (u8, usize) {
    let rank = match status {
        WorkerStatus::Downloading => 0,
        WorkerStatus::Retrying => 1,
        WorkerStatus::Idle => 2,
        WorkerStatus::Done => 3,
    };
    (rank, id)
}

fn sync_worker_selection(
    selected_worker: &mut usize,
    worker_states: &SharedWorkerStates,
    sort_by_id: bool,
) {
    let ordered = worker_order(worker_states, sort_by_id);
    if ordered.is_empty() {
        *selected_worker = 0;
    } else if !ordered.contains(selected_worker) {
        *selected_worker = ordered[0];
    }
}

fn move_worker_selection(
    selected_worker: &mut usize,
    worker_states: &SharedWorkerStates,
    sort_by_id: bool,
    delta: isize,
) {
    let ordered = worker_order(worker_states, sort_by_id);
    if ordered.is_empty() {
        *selected_worker = 0;
        return;
    }
    let current = ordered
        .iter()
        .position(|id| *id == *selected_worker)
        .unwrap_or(0);
    let next = (current as isize + delta).clamp(0, ordered.len() as isize - 1) as usize;
    *selected_worker = ordered[next];
}

fn select_worker_edge(
    selected_worker: &mut usize,
    worker_states: &SharedWorkerStates,
    sort_by_id: bool,
    bottom: bool,
) {
    let ordered = worker_order(worker_states, sort_by_id);
    if let Some(id) = if bottom {
        ordered.last()
    } else {
        ordered.first()
    } {
        *selected_worker = *id;
    }
}

fn eta_label(downloaded: u64, total: u64, speed_bps: f64) -> String {
    if total == 0 || downloaded >= total {
        return "--".to_string();
    }
    if speed_bps <= 1.0 {
        return "calculating".to_string();
    }
    super::tui_common::format_eta((total - downloaded) as f64 / speed_bps)
}

fn tab_alerts(data: &FrameData, worker_states: &SharedWorkerStates) -> (bool, bool) {
    let has_failures = data
        .pieces
        .iter()
        .any(|piece| piece.state == PieceState::Failed);
    let has_retries = worker_states
        .lock()
        .unwrap()
        .iter()
        .any(|worker| worker.status == WorkerStatus::Retrying || worker.retries > 0);
    (has_failures, has_retries)
}

fn single_download_is_finished(segment_man: &SegmentMan) -> bool {
    segment_man.snapshot_storage(|storage| storage.completed_count() >= storage.total_pieces())
}

#[cfg(test)]
mod tests;

// ─── Formatting helpers ─────────────────────────────────────────────────────
