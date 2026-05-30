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

mod actions;
mod input;
mod pieces;
mod render;
mod terminal;

use actions::{copy_visible_text, open_output_dir};
use input::{InputAction, poll_input};
pub use pieces::{
    HeatMapParams, collect_piece_snapshots, render_heat_map, render_overview_bar,
    render_piece_policy_line, render_piece_workers_panel, render_recovery_queue,
};
pub(crate) use pieces::{
    PIECE_DETAIL_MIN_HEIGHT, PIECE_DETAIL_TITLE, effective_selected_piece, piece_grid_panel_height,
    piece_size_policy_label, piece_viewport_for_area,
};
#[cfg(test)]
use pieces::{
    PIECE_GRID_MAX_HEIGHT, PieceVisualState, piece_grid_cells_per_row, piece_grid_columns,
    piece_viewport_for_inner, piece_visual_state,
};
use render::draw_ui;
pub(crate) use render::wrap_line;
use terminal::{restore_terminal, setup_terminal};

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
