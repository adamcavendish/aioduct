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
    Block, Borders, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState,
};
use tokio_util::sync::CancellationToken;

use crate::common::copy_to_clipboard;

use super::file_entry::{FileId, FileStatus};
use super::piece_grid::{
    HeatMapParams, PIECE_DETAIL_TITLE, PieceSnapshot, PieceState, effective_selected_piece,
    piece_grid_panel_height, piece_size_policy_label, piece_viewport_for_area, render_heat_map,
    render_overview_bar, render_piece_policy_line, render_piece_workers_panel,
    render_recovery_queue, wrap_line,
};
use super::progress::format_size;
use super::scheduler::{FileSnapshot, GlobalScheduler};
use super::tui_common::{
    DOWNLOAD_TABS, EventFilter, HorizontalKeyAction, detail_line, draw_cancel_overlay,
    draw_download_help_overlay, format_size_iec, format_speed, horizontal_key_action, open_path,
    percent, push_ui_event, truncate_str, worker_status_color,
};
use super::tui_state::{
    DownloadEvent, EventSeverity, SharedEventLog, SharedWorkerStates, WorkerState, WorkerStatus,
    display_worker_id, format_duration_compact,
};

mod actions;
mod input;
mod render;
mod terminal;

use actions::{copy_visible_text, integrity_summary, open_selected_output};
use input::{InputAction, poll_input};
use render::{
    active_worker_piece, download_is_finished, draw_multi_ui, focus_count, move_worker_selection,
    select_worker_edge, selected_worker_piece, sync_worker_selection,
};
use terminal::{restore_terminal, setup_terminal};

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
    active_tab: usize,
    selected_file: usize,
    piece_scroll: u16,
    event_scroll: u16,
    show_help: bool,
    frame_count: u64,
    live_speed_bps: f64,
    prev_downloaded: u64,
    prev_download_time: Instant,
    speed_history: VecDeque<u64>,
    last_speed_sample: Instant,
    event_filter: EventFilter,
    filter_query: String,
    editing_filter: bool,
    original_order: bool,
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
            active_tab: 0,
            selected_file: 0,
            piece_scroll: 0,
            event_scroll: 0,
            show_help: false,
            frame_count: 0,
            live_speed_bps: 0.0,
            prev_downloaded: 0,
            prev_download_time: now,
            speed_history: VecDeque::with_capacity(60),
            last_speed_sample: now,
            event_filter: EventFilter::All,
            filter_query: String::new(),
            editing_filter: false,
            original_order: false,
            selected_piece: None,
            selected_worker: 0,
            worker_sort_by_id: false,
            focus_index: 0,
            show_cancel_confirm: false,
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
        while let Some(action) = poll_input(app.editing_filter, app.active_tab) {
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
                    if download_is_finished(&scheduler.snapshot_files()) {
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
                                if let Some((file_id, piece)) =
                                    selected_worker_piece(&worker_states, app.selected_worker)
                                {
                                    if let Some(file_id) = file_id {
                                        let files = scheduler.snapshot_files();
                                        if let Some(idx) =
                                            files.iter().position(|f| f.id == file_id)
                                        {
                                            app.selected_file = idx;
                                        }
                                    }
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
                    0 => move_queue_selection(
                        &mut app.selected_file,
                        &scheduler.snapshot_files(),
                        app.original_order,
                        &app.filter_query,
                        -1,
                    ),
                    2 => app.piece_scroll = app.piece_scroll.saturating_sub(1),
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
                    0 => move_queue_selection(
                        &mut app.selected_file,
                        &scheduler.snapshot_files(),
                        app.original_order,
                        &app.filter_query,
                        1,
                    ),
                    2 => app.piece_scroll = app.piece_scroll.saturating_add(1),
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
                    0 => move_queue_selection(
                        &mut app.selected_file,
                        &scheduler.snapshot_files(),
                        app.original_order,
                        &app.filter_query,
                        -10,
                    ),
                    2 => app.piece_scroll = app.piece_scroll.saturating_sub(10),
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
                    0 => move_queue_selection(
                        &mut app.selected_file,
                        &scheduler.snapshot_files(),
                        app.original_order,
                        &app.filter_query,
                        10,
                    ),
                    2 => app.piece_scroll = app.piece_scroll.saturating_add(10),
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
                    0 => {
                        let ordered = queue_order(
                            &scheduler.snapshot_files(),
                            app.original_order,
                            &app.filter_query,
                        );
                        if let Some(first) = ordered.first() {
                            app.selected_file = *first;
                        }
                    }
                    2 => app.piece_scroll = 0,
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
                    0 => {
                        let ordered = queue_order(
                            &scheduler.snapshot_files(),
                            app.original_order,
                            &app.filter_query,
                        );
                        if let Some(last) = ordered.last() {
                            app.selected_file = *last;
                        }
                    }
                    2 => app.piece_scroll = u16::MAX,
                    3 => select_worker_edge(
                        &mut app.selected_worker,
                        &worker_states,
                        app.worker_sort_by_id,
                        true,
                    ),
                    4 => app.event_scroll = u16::MAX,
                    _ => {}
                },
                InputAction::ToggleHelp => {
                    app.show_help = !app.show_help;
                }
                InputAction::SetEventFilter(filter) => {
                    if app.active_tab == 0 && filter == EventFilter::Failures {
                        app.filter_query = "failed".to_string();
                        sync_queue_selection(
                            &mut app.selected_file,
                            &scheduler.snapshot_files(),
                            app.original_order,
                            &app.filter_query,
                        );
                    } else {
                        app.event_filter = filter;
                    }
                    app.event_scroll = 0;
                }
                InputAction::StartFilter => {
                    app.editing_filter = true;
                    app.event_scroll = 0;
                    app.piece_scroll = 0;
                }
                InputAction::FilterChar(ch) => {
                    app.filter_query.push(ch);
                    app.event_scroll = 0;
                    app.piece_scroll = 0;
                    sync_queue_selection(
                        &mut app.selected_file,
                        &scheduler.snapshot_files(),
                        app.original_order,
                        &app.filter_query,
                    );
                }
                InputAction::FilterBackspace => {
                    app.filter_query.pop();
                    app.event_scroll = 0;
                    app.piece_scroll = 0;
                    sync_queue_selection(
                        &mut app.selected_file,
                        &scheduler.snapshot_files(),
                        app.original_order,
                        &app.filter_query,
                    );
                }
                InputAction::FilterSubmit => {
                    app.editing_filter = false;
                }
                InputAction::FilterCancel => {
                    app.editing_filter = false;
                }
                InputAction::OpenOrToggleOrder => {
                    if app.active_tab == 0 {
                        app.original_order = !app.original_order;
                        sync_queue_selection(
                            &mut app.selected_file,
                            &scheduler.snapshot_files(),
                            app.original_order,
                            &app.filter_query,
                        );
                    } else {
                        open_selected_output(&app, &scheduler.snapshot_files(), &events);
                    }
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
                InputAction::CopyVisible => {
                    copy_visible_text(&app, &scheduler.snapshot_files(), &events);
                }
                InputAction::PrevPiece => {
                    if app.active_tab == 2 {
                        let current = app
                            .selected_piece
                            .or_else(|| active_worker_piece(&worker_states).map(|(_, piece)| piece))
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
                            .or_else(|| active_worker_piece(&worker_states).map(|(_, piece)| piece))
                            .unwrap_or(0);
                        app.selected_piece = Some(current.saturating_add(1));
                    } else {
                        app.focus_index = (app.focus_index + 1) % focus_count(app.active_tab);
                    }
                }
                InputAction::PrevFile => {
                    move_queue_selection(
                        &mut app.selected_file,
                        &scheduler.snapshot_files(),
                        app.original_order,
                        &app.filter_query,
                        -1,
                    );
                    app.selected_piece = None;
                }
                InputAction::NextFile => {
                    move_queue_selection(
                        &mut app.selected_file,
                        &scheduler.snapshot_files(),
                        app.original_order,
                        &app.filter_query,
                        1,
                    );
                    app.selected_piece = None;
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

fn selected_file<'a>(data: &'a MultiFrameData, app: &AppState) -> Option<&'a FileSnapshot> {
    data.files.get(app.selected_file)
}

fn queue_order(files: &[FileSnapshot], original_order: bool, filter_query: &str) -> Vec<usize> {
    let mut ordered: Vec<usize> = files
        .iter()
        .enumerate()
        .filter(|(_, file)| file_matches_query(file, filter_query))
        .map(|(idx, _)| idx)
        .collect();
    if !original_order {
        ordered.sort_by_key(|idx| {
            let file = &files[*idx];
            let rank = match file.status {
                FileStatus::Active => 0,
                FileStatus::Failed => 1,
                FileStatus::Pending => 2,
                FileStatus::Complete => 3,
            };
            (rank, *idx)
        });
    }
    ordered
}

fn move_queue_selection(
    selected_file: &mut usize,
    files: &[FileSnapshot],
    original_order: bool,
    filter_query: &str,
    delta: isize,
) {
    let ordered = queue_order(files, original_order, filter_query);
    if ordered.is_empty() {
        *selected_file = 0;
        return;
    }
    let current = ordered
        .iter()
        .position(|idx| *idx == *selected_file)
        .unwrap_or(0);
    let next = (current as isize + delta).clamp(0, ordered.len() as isize - 1) as usize;
    *selected_file = ordered[next];
}

fn sync_queue_selection(
    selected_file: &mut usize,
    files: &[FileSnapshot],
    original_order: bool,
    filter_query: &str,
) {
    let ordered = queue_order(files, original_order, filter_query);
    if ordered.is_empty() {
        *selected_file = 0;
    } else if !ordered.contains(selected_file) {
        *selected_file = ordered[0];
    }
}

fn file_matches_query(file: &FileSnapshot, filter_query: &str) -> bool {
    let query = filter_query.trim().to_ascii_lowercase();
    if query.is_empty() {
        return true;
    }
    let haystack = format!(
        "{} {} {} {} {}",
        file.filename,
        file.output.display(),
        file.url,
        file_status_label(file.status),
        file.checksum_status
    )
    .to_ascii_lowercase();
    haystack.contains(&query)
}

fn event_matches_query(event: &DownloadEvent, filter_query: &str) -> bool {
    let query = filter_query.trim().to_ascii_lowercase();
    if query.is_empty() {
        return true;
    }
    event.display_line().to_ascii_lowercase().contains(&query)
}

fn event_filter_label(app: &AppState, data: &MultiFrameData) -> String {
    match app.event_filter {
        EventFilter::Worker => format!("worker {}", display_worker_id(app.selected_worker)),
        EventFilter::SelectedFile => data
            .files
            .get(app.selected_file)
            .map(|file| format!("file {}", truncate_str(&file.filename, 28)))
            .unwrap_or_else(|| app.event_filter.label().to_string()),
        _ => app.event_filter.label().to_string(),
    }
}

fn file_downloaded(file: &FileSnapshot) -> u64 {
    (file.completed_pieces as u64 * file.piece_length as u64).min(file.total_size)
}

fn file_status_label(status: FileStatus) -> &'static str {
    match status {
        FileStatus::Pending => "pending",
        FileStatus::Active => "active",
        FileStatus::Complete => "complete",
        FileStatus::Failed => "failed",
    }
}

fn status_color(status: FileStatus) -> Color {
    match status {
        FileStatus::Pending => Color::DarkGray,
        FileStatus::Active => Color::Yellow,
        FileStatus::Complete => Color::Green,
        FileStatus::Failed => Color::Red,
    }
}
