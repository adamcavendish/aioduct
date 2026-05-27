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
    DownloadEvent, EventSeverity, SharedEventLog, SharedWorkerStates, WorkerStatus,
    display_worker_id, format_duration_compact,
};

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

enum InputAction {
    Quit,
    ForceQuit,
    Dismiss,
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
    FocusNext,
    Confirm,
    SetEventFilter(EventFilter),
    StartFilter,
    FilterChar(char),
    FilterBackspace,
    FilterSubmit,
    FilterCancel,
    OpenOrToggleOrder,
    ToggleWorkerSort,
    CopyVisible,
    PrevPiece,
    NextPiece,
    PrevFile,
    NextFile,
}

fn poll_input(editing_filter: bool, active_tab: usize) -> Option<InputAction> {
    if !event::poll(Duration::ZERO).unwrap_or(false) {
        return None;
    }
    if let Ok(Event::Key(key)) = event::read() {
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
        return match key.code {
            KeyCode::Char('q') => Some(InputAction::Quit),
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
            KeyCode::Char('o') => Some(InputAction::OpenOrToggleOrder),
            KeyCode::Char('y') => Some(InputAction::CopyVisible),
            KeyCode::Char('[') => Some(InputAction::PrevFile),
            KeyCode::Char(']') => Some(InputAction::NextFile),
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
    if area.width < 60 || area.height < 16 {
        draw_small_terminal_message(f, area, "download", data);
        return;
    }

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2), // header
            Constraint::Min(4),    // main content
            Constraint::Length(1), // footer
        ])
        .split(area);

    draw_header(f, chunks[0], data, app, worker_states);

    match app.active_tab {
        0 => draw_queue_tab(f, chunks[1], data, app, worker_states, events),
        1 => draw_file_tab(f, chunks[1], data, app, worker_states, events),
        2 => draw_pieces_tab(f, chunks[1], data, app, scheduler, worker_states),
        3 => draw_workers_tab(f, chunks[1], data, worker_states, app),
        4 => draw_events_tab(f, chunks[1], data, app, events),
        5 => draw_summary_tab(f, chunks[1], data, events),
        _ => {}
    }

    draw_footer(f, chunks[2], data, app);

    if app.show_cancel_confirm {
        draw_cancel_overlay(f, area, "download");
    } else if app.show_help {
        draw_download_help_overlay(f, area, "Help [?]");
    }
}

fn draw_header(
    f: &mut Frame,
    area: Rect,
    data: &MultiFrameData,
    app: &AppState,
    worker_states: &SharedWorkerStates,
) {
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
        Span::styled(" aioduct ", Style::default().fg(Color::DarkGray)),
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
        Span::styled(
            format!("  {}", format_speed(data.speed_bps)),
            Style::default().fg(Color::Cyan),
        ),
        Span::styled(
            format!(
                "  ETA {}",
                eta_label(data.total_downloaded, data.total_size, data.speed_bps)
            ),
            Style::default().fg(Color::Magenta),
        ),
        Span::styled(
            format!("  {} workers", data.num_workers),
            Style::default().fg(Color::Yellow),
        ),
        Span::styled("  resume on", Style::default().fg(Color::DarkGray)),
    ]);

    let (has_failures, has_retries) = tab_alerts_from_workers(data, worker_states);
    let mut tab_spans: Vec<Span> = vec![Span::styled(" ", Style::default())];
    for (i, name) in DOWNLOAD_TABS.iter().enumerate() {
        let alert_color = if has_failures && matches!(i, 0 | 1 | 4 | 5) {
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
                format!(" {} {name}{marker} ", i + 1),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ));
        } else {
            tab_spans.push(Span::styled(
                format!(" {} {name}{marker} ", i + 1),
                Style::default().fg(alert_color.unwrap_or(Color::DarkGray)),
            ));
        }
    }
    let line2 = Line::from(tab_spans);

    let text = vec![line1, line2];
    f.render_widget(Paragraph::new(text), area);
}

fn draw_footer(f: &mut Frame, area: Rect, data: &MultiFrameData, app: &AppState) {
    let quit_hint = if data.total_downloaded >= data.total_size && data.total_size > 0 {
        "q close"
    } else {
        "q cancel"
    };
    let hint = if app.editing_filter {
        format!("filter: {}_   Enter apply   Esc close", app.filter_query)
    } else {
        match app.active_tab {
            0 => format!(
                "1-6 pages   Tab focus:{}   ↑/↓ select file   Enter file   / filter   f failed   o order   {quit_hint}   ? help",
                focus_label(app.active_tab, app.focus_index)
            ),
            1 => format!(
                "1-6 pages   Tab focus:{}   [/] file   3 pieces   4 workers   y copy path   {quit_hint}   ? help",
                focus_label(app.active_tab, app.focus_index)
            ),
            2 => format!(
                "1-6 pages   ←/→ select piece   j/k scroll rows   Enter workers   [/] file   {quit_hint}   ? help"
            ),
            3 => format!(
                "1-6 pages   ↑/↓ select worker   Enter jump to piece   s sort:{}   w worker events   {quit_hint}   ? help",
                if app.worker_sort_by_id { "id" } else { "state" },
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

fn draw_small_terminal_message(
    f: &mut Frame,
    area: Rect,
    mode: &'static str,
    data: &MultiFrameData,
) {
    let pct = percent(data.total_downloaded, data.total_size);
    let lines = vec![
        Line::styled(
            format!("aioduct {mode} is still running"),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Line::raw(""),
        detail_line(
            "progress",
            format!(
                "{pct:.1}%  {}/{}",
                format_size(data.total_downloaded),
                format_size(data.total_size)
            ),
            Color::Yellow,
        ),
        detail_line("speed", format_speed(data.speed_bps), Color::Cyan),
        detail_line("need", "terminal >= 60x16", Color::DarkGray),
    ];
    f.render_widget(Paragraph::new(lines), area);
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
            0 => "files",
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
) -> Option<(Option<FileId>, u32)> {
    worker_states
        .lock()
        .unwrap()
        .iter()
        .find(|worker| worker.id == selected_worker)
        .and_then(|worker| worker.current_piece.map(|piece| (worker.file_id, piece)))
        .or_else(|| active_worker_piece(worker_states))
}

fn active_worker_piece(worker_states: &SharedWorkerStates) -> Option<(Option<FileId>, u32)> {
    worker_states
        .lock()
        .unwrap()
        .iter()
        .find(|worker| {
            worker.status == WorkerStatus::Downloading || worker.status == WorkerStatus::Retrying
        })
        .and_then(|worker| worker.current_piece.map(|piece| (worker.file_id, piece)))
}

fn worker_order(worker_states: &SharedWorkerStates, sort_by_id: bool) -> Vec<usize> {
    let workers = worker_states.lock().unwrap();
    let mut ordered: Vec<&super::tui_state::WorkerState> = workers.iter().collect();
    if sort_by_id {
        ordered.sort_by_key(|w| w.id);
    } else {
        ordered.sort_by_key(|w| worker_sort_key(w.status, w.id));
    }
    ordered.into_iter().map(|w| w.id).collect()
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

fn tab_alerts(data: &MultiFrameData, worker_states: Option<&SharedWorkerStates>) -> (bool, bool) {
    let has_failures = data
        .files
        .iter()
        .any(|file| file.status == FileStatus::Failed);
    let has_retries = worker_states
        .map(|workers| {
            workers
                .lock()
                .unwrap()
                .iter()
                .any(|worker| worker.status == WorkerStatus::Retrying || worker.retries > 0)
        })
        .unwrap_or(false);
    (has_failures, has_retries)
}

fn tab_alerts_from_workers(
    data: &MultiFrameData,
    worker_states: &SharedWorkerStates,
) -> (bool, bool) {
    tab_alerts(data, Some(worker_states))
}

fn download_is_finished(files: &[FileSnapshot]) -> bool {
    !files.is_empty()
        && files
            .iter()
            .all(|file| matches!(file.status, FileStatus::Complete | FileStatus::Failed))
}

// ─── Queue Tab ──────────────────────────────────────────────────────────────

fn draw_queue_tab(
    f: &mut Frame,
    area: Rect,
    data: &MultiFrameData,
    app: &AppState,
    worker_states: &SharedWorkerStates,
    events: &SharedEventLog,
) {
    if area.height == 0 || data.files.is_empty() {
        return;
    }

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(6), Constraint::Length(4)])
        .split(area);
    let panes = if chunks[0].width >= 92 {
        Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(56), Constraint::Percentage(44)])
            .split(chunks[0])
    } else {
        Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Percentage(58), Constraint::Percentage(42)])
            .split(chunks[0])
    };
    let bottom = if chunks[1].width >= 92 {
        Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(56), Constraint::Percentage(44)])
            .split(chunks[1])
    } else {
        Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
            .split(chunks[1])
    };

    draw_file_list(f, panes[0], data, app);
    if let Some(file) = selected_file(data, app) {
        draw_selected_file_preview(f, panes[1], file);
    }
    draw_worker_strip(f, bottom[0], worker_states, app);
    draw_event_tail(f, bottom[1], events, "Event tail");
}

fn draw_file_list(f: &mut Frame, area: Rect, data: &MultiFrameData, app: &AppState) {
    let title = if app.filter_query.is_empty() {
        if app.original_order {
            " Files (original) ".to_string()
        } else {
            " Files (active first) ".to_string()
        }
    } else if app.original_order {
        format!(" Files (original, filter: {}) ", app.filter_query)
    } else {
        format!(" Files (active first, filter: {}) ", app.filter_query)
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(Style::default().fg(Color::Cyan));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let visible = inner.height as usize;
    let ordered = queue_order(&data.files, app.original_order, &app.filter_query);
    let selected_pos = ordered
        .iter()
        .position(|idx| *idx == app.selected_file)
        .unwrap_or(0);
    let total = ordered.len();

    let scroll = if selected_pos >= visible {
        (selected_pos - visible + 1) as u16
    } else {
        0
    };

    let mut lines: Vec<Line> = Vec::with_capacity(visible);

    for i in ordered.iter().skip(scroll as usize).take(visible).copied() {
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

        let downloaded = file_downloaded(file);
        let pct = percent(downloaded, file.total_size);
        let name_width = inner.width.saturating_sub(30).clamp(12, 34) as usize;
        let name = truncate_str(&file.filename, name_width);
        let worker_str = if file.active_workers > 0 {
            format!(" W{}", file.active_workers)
        } else {
            String::new()
        };

        let line = Line::from(vec![
            Span::styled(format!("{indicator} "), Style::default().fg(Color::Cyan)),
            status_icon,
            Span::styled(format!("{name:<name_width$}"), name_style),
            Span::styled(
                format!(" {:>8} ", format_size_iec(file.total_size)),
                Style::default().fg(Color::Cyan),
            ),
            Span::styled(format!(" {:>3.0}%", pct), Style::default().fg(Color::White)),
            Span::styled(worker_str, Style::default().fg(Color::DarkGray)),
        ]);

        lines.push(line);
    }

    f.render_widget(Paragraph::new(lines), inner);

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

fn draw_selected_file_preview(f: &mut Frame, area: Rect, file: &FileSnapshot) {
    let downloaded = file_downloaded(file);
    let pct = percent(downloaded, file.total_size);
    let bar = progress_bar(pct, 30);
    let lines = vec![
        Line::styled(
            truncate_str(&file.filename, area.width.saturating_sub(4) as usize).to_string(),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
        Line::from(vec![
            Span::styled(bar, Style::default().fg(status_color(file.status))),
            Span::styled(format!(" {pct:>5.1}%"), Style::default().fg(Color::Yellow)),
        ]),
        detail_line(
            "pieces",
            format!("{}/{} complete", file.completed_pieces, file.total_pieces),
            Color::White,
        ),
        detail_line(
            "piece",
            piece_size_policy_label(file.piece_length),
            Color::White,
        ),
        detail_line(
            "range",
            if file.supports_range { "yes" } else { "no" },
            Color::White,
        ),
        detail_line("checksum", file.checksum_status.clone(), Color::DarkGray),
        detail_line("output", file.output.display().to_string(), Color::Cyan),
    ];
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Selected ")
        .border_style(Style::default().fg(Color::Cyan));
    f.render_widget(Paragraph::new(lines).block(block), area);
}

// ─── File Tab ───────────────────────────────────────────────────────────────

fn draw_file_tab(
    f: &mut Frame,
    area: Rect,
    data: &MultiFrameData,
    app: &AppState,
    worker_states: &SharedWorkerStates,
    events: &SharedEventLog,
) {
    let Some(file) = selected_file(data, app) else {
        f.render_widget(Paragraph::new("No file selected"), area);
        return;
    };

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(5),
            Constraint::Min(6),
            Constraint::Length(4),
        ])
        .split(area);

    let downloaded = file_downloaded(file);
    let pct = percent(downloaded, file.total_size);
    let pending_pieces = file
        .total_pieces
        .saturating_sub(file.completed_pieces)
        .saturating_sub(file.active_workers);
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
                    format_size_iec(downloaded),
                    format_size_iec(file.total_size)
                ),
                Style::default().fg(Color::White),
            ),
            Span::styled(format!("  {pct:.1}%"), Style::default().fg(Color::Yellow)),
        ]),
        Line::from(vec![Span::styled(
            progress_bar(pct, rows[0].width.saturating_sub(8) as usize),
            Style::default().fg(status_color(file.status)),
        )]),
        Line::from(vec![
            Span::styled(
                format!(
                    "pieces {} complete, {} active, {} pending",
                    file.completed_pieces, file.active_workers, pending_pieces
                ),
                Style::default().fg(Color::DarkGray),
            ),
            Span::styled(
                format!("  {}", piece_size_policy_label(file.piece_length)),
                Style::default().fg(Color::DarkGray),
            ),
        ]),
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
            .constraints([Constraint::Percentage(58), Constraint::Percentage(42)])
            .split(rows[1])
    };

    let mut source_lines = vec![
        detail_line(
            "Status",
            file_status_label(file.status),
            status_color(file.status),
        ),
        detail_line("Workers", file.active_workers.to_string(), Color::White),
        detail_line(
            "Range",
            if file.supports_range {
                "supported"
            } else {
                "no"
            },
            if file.supports_range {
                Color::Green
            } else {
                Color::Red
            },
        ),
        detail_line("URL", file.url.clone(), Color::Cyan),
    ];
    if let Some(last_modified) = &file.last_modified {
        source_lines.push(detail_line(
            "Modified",
            last_modified.clone(),
            Color::DarkGray,
        ));
    }
    if let Some(etag) = &file.etag {
        source_lines.push(detail_line("ETag", etag.clone(), Color::DarkGray));
    }

    let output_lines = vec![
        detail_line("Output", file.output.display().to_string(), Color::Cyan),
        detail_line(
            "Control",
            file.control_path.display().to_string(),
            Color::DarkGray,
        ),
        detail_line(
            "Resume",
            if file.resume_skipped_pieces > 0 {
                format!("yes, skipped {} pieces", file.resume_skipped_pieces)
            } else {
                "ready".to_string()
            },
            Color::Green,
        ),
        detail_line("Allocation", file.allocation, Color::Green),
        detail_line("Checksum", file.checksum_status.clone(), Color::DarkGray),
        detail_line("Created", file.created_at.clone(), Color::DarkGray),
    ];

    let source_block = Block::default()
        .borders(Borders::ALL)
        .title(" Source ")
        .border_style(Style::default().fg(Color::Cyan));
    f.render_widget(Paragraph::new(source_lines).block(source_block), chunks[0]);

    let output_block = Block::default()
        .borders(Borders::ALL)
        .title(" Output ")
        .border_style(Style::default().fg(Color::Cyan));
    f.render_widget(Paragraph::new(output_lines).block(output_block), chunks[1]);

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
    draw_selected_workers(f, footer[0], file.id, worker_states);
    draw_event_tail(f, footer[1], events, "Last event");
}

fn draw_selected_workers(
    f: &mut Frame,
    area: Rect,
    file_id: super::file_entry::FileId,
    worker_states: &SharedWorkerStates,
) {
    let workers = worker_states.lock().unwrap();
    let mut lines = Vec::new();
    for worker in workers.iter().filter(|w| w.file_id == Some(file_id)) {
        let piece = worker
            .current_piece
            .map(|p| format!("#{p}"))
            .unwrap_or_else(|| "-".into());
        let pct = if worker.piece_length > 0 {
            percent(worker.downloaded_bytes(), worker.piece_length)
        } else {
            0.0
        };
        lines.push(Line::from(vec![
            Span::styled(
                format!("{} ", display_worker_id(worker.id)),
                Style::default().fg(Color::Cyan),
            ),
            Span::styled(format!("{piece:<8}"), Style::default().fg(Color::Yellow)),
            Span::styled(format!("{pct:>5.1}% "), Style::default().fg(Color::Green)),
            Span::styled(
                worker.status.to_string(),
                Style::default().fg(worker_status_color(worker.status)),
            ),
        ]));
    }
    if lines.is_empty() {
        lines.push(Line::styled(
            "No workers assigned.",
            Style::default().fg(Color::DarkGray),
        ));
    }

    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Active workers ")
        .border_style(Style::default().fg(Color::Yellow));
    f.render_widget(Paragraph::new(lines).block(block), area);
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
    let selected_piece =
        effective_selected_piece(&pieces, app.selected_piece, worker_states, Some(file_id));
    let mut params = HeatMapParams {
        pieces: &pieces,
        total_pieces,
        piece_length,
        scroll_offset: app.piece_scroll,
        frame_count: app.frame_count,
        selected_piece,
        viewport: None,
    };

    let file = &data.files[app.selected_file];
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(6),
        ])
        .split(area);

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
        render_recovery_queue(f, panes[1], &params, worker_states, Some(file_id));

        if rows.len() > 1 {
            let bottom = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
                .split(rows[1]);
            draw_piece_detail_panel(f, bottom[0], file, &pieces, selected_piece, worker_states);
            render_piece_workers_panel(f, bottom[1], worker_states, Some(file_id));
        }
    } else if content.height >= 17 {
        let grid_height = piece_grid_panel_height(content.width, params.total_pieces)
            .min(content.height)
            .max(1);
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(grid_height),
                Constraint::Length(5),
                Constraint::Min(7),
            ])
            .split(content);
        params.viewport = Some(piece_viewport_for_area(rows[0], &params));
        render_piece_policy_line(f, chunks[0], &params);
        render_overview_bar(f, chunks[1], &params, worker_states);
        render_heat_map(f, rows[0], &params, worker_states);
        render_recovery_queue(f, rows[1], &params, worker_states, Some(file_id));
        draw_piece_detail_panel(f, rows[2], file, &pieces, selected_piece, worker_states);
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
    file: &FileSnapshot,
    pieces: &[PieceSnapshot],
    selected_piece: Option<u32>,
    worker_states: &SharedWorkerStates,
) {
    let workers = worker_states.lock().unwrap();
    let active = workers.iter().find_map(|w| {
        if w.file_id == Some(file.id) {
            w.current_piece.map(|piece| (piece, Some(w)))
        } else {
            None
        }
    });
    let selected = selected_piece
        .filter(|piece| (*piece as usize) < pieces.len())
        .map(|piece| {
            let worker = workers
                .iter()
                .find(|w| w.file_id == Some(file.id) && w.current_piece == Some(piece));
            (piece, worker)
        })
        .or(active)
        .or_else(|| {
            pieces
                .iter()
                .position(|p| p.state == PieceState::Pending)
                .map(|idx| (idx as u32, None))
        })
        .or_else(|| {
            pieces
                .iter()
                .rposition(|p| p.state == PieceState::Complete)
                .map(|idx| (idx as u32, None))
        });

    let mut lines = Vec::new();

    if let Some((piece, worker)) = selected {
        let snapshot = pieces
            .get(piece as usize)
            .cloned()
            .unwrap_or(PieceSnapshot {
                index: piece,
                state: PieceState::Pending,
                retry_count: 0,
                last_error: None,
            });
        let state = snapshot.state;
        let start = piece as u64 * file.piece_length as u64;
        let end = (start + file.piece_length as u64)
            .saturating_sub(1)
            .min(file.total_size.saturating_sub(1));
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
                piece_size_policy_label(file.piece_length),
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
                format_size_iec(file.piece_length as u64),
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

// ─── Workers Tab ────────────────────────────────────────────────────────────

fn draw_workers_tab(
    f: &mut Frame,
    area: Rect,
    data: &MultiFrameData,
    worker_states: &SharedWorkerStates,
    app: &AppState,
) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Workers ")
        .border_style(Style::default().fg(Color::Cyan));
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let workers = worker_states.lock().unwrap();
    let downloading = workers
        .iter()
        .filter(|w| w.status == WorkerStatus::Downloading)
        .count();
    let retrying = workers
        .iter()
        .filter(|w| w.status == WorkerStatus::Retrying)
        .count();
    let idle = workers
        .iter()
        .filter(|w| w.status == WorkerStatus::Idle)
        .count();
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(2),
        ])
        .split(inner);

    let header = Line::from(vec![
        Span::styled(
            format!("{} workers", workers.len()),
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
    ]);
    f.render_widget(Paragraph::new(header), rows[0]);

    let mut table_lines: Vec<Line> = vec![Line::from(vec![Span::styled(
        format!(
            "{:<4} {:<20} {:<7} {:<20} {:<12} {:<4} {}",
            "ID", "File", "Piece", "Progress", "Speed", "Ret", "Status"
        ),
        Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::BOLD),
    )])];

    let mut ordered: Vec<_> = workers.iter().collect();
    if app.worker_sort_by_id {
        ordered.sort_by_key(|w| w.id);
    } else {
        ordered.sort_by_key(|w| worker_sort_key(w.status, w.id));
    }
    let table_capacity = rows[1].height.saturating_sub(1) as usize;
    for w in ordered.into_iter().take(table_capacity) {
        let is_selected = w.id == app.selected_worker;
        let indicator = if is_selected { "›" } else { " " };
        let id_str = display_worker_id(w.id);
        let file_str = truncate_str(&w.file_name, 18).to_string();
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
            let bar_w = 10;
            let filled = (bar_w as f64 * ratio) as usize;
            let empty = bar_w - filled;
            let bar = format!("{}{}", "\u{2501}".repeat(filled), "\u{2500}".repeat(empty));
            let pct = format!("{:>3.0}%", ratio * 100.0);
            (bar, pct)
        } else {
            ("waiting   ".to_string(), "    ".to_string())
        };

        let speed_str = if w.status == WorkerStatus::Downloading {
            format_speed(app.live_speed_bps / data.num_workers.max(1) as f64)
        } else if w.status == WorkerStatus::Retrying {
            "retrying".to_string()
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

        table_lines.push(Line::from(vec![
            Span::styled(format!("{indicator} "), Style::default().fg(Color::Cyan)),
            Span::styled(format!("{id_str:<4} "), Style::default().fg(Color::Cyan)),
            Span::styled(
                format!("{file_str:<20} "),
                Style::default().fg(if is_selected {
                    Color::Cyan
                } else {
                    Color::White
                }),
            ),
            Span::styled(
                format!("{piece_str:<7} "),
                Style::default().fg(Color::Yellow),
            ),
            Span::styled(
                format!("{bar_str} {pct_str:<4} "),
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
            Span::styled(format!("{}", w.status), Style::default().fg(status_color)),
        ]));
    }
    f.render_widget(Paragraph::new(table_lines), rows[1]);

    let footer = if let Some(selected) = workers
        .iter()
        .find(|w| w.id == app.selected_worker)
        .or_else(|| {
            workers.iter().find(|w| {
                w.status == WorkerStatus::Retrying || w.status == WorkerStatus::Downloading
            })
        }) {
        vec![
            Line::raw(""),
            Line::from(vec![
                Span::styled("selected ", Style::default().fg(Color::DarkGray)),
                Span::styled(
                    display_worker_id(selected.id),
                    Style::default().fg(Color::Cyan),
                ),
                Span::styled(
                    format!("  file {}", selected.file_name),
                    Style::default().fg(Color::White),
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
            ]),
        ]
    } else {
        vec![
            Line::raw(""),
            Line::styled("No worker selected.", Style::default().fg(Color::DarkGray)),
        ]
    };
    f.render_widget(Paragraph::new(footer), rows[2]);
}

// ─── Events Tab ─────────────────────────────────────────────────────────────

fn draw_events_tab(
    f: &mut Frame,
    area: Rect,
    data: &MultiFrameData,
    app: &AppState,
    events: &SharedEventLog,
) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Events ")
        .border_style(Style::default().fg(Color::Yellow));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let events_lock = events.lock().unwrap();
    let raw_events: Vec<DownloadEvent> = events_lock.iter().cloned().collect();
    drop(events_lock);
    let selected_file_id = data.files.get(app.selected_file).map(|file| file.id);
    let selected_worker_id = Some(app.selected_worker);
    let filtered_events: Vec<DownloadEvent> = raw_events
        .iter()
        .filter(|event| {
            app.event_filter
                .matches(event, selected_file_id, selected_worker_id)
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
            Span::styled(
                event_filter_label(app, data),
                Style::default().fg(Color::Cyan),
            ),
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

// ─── Summary Tab ────────────────────────────────────────────────────────────

fn draw_summary_tab(f: &mut Frame, area: Rect, data: &MultiFrameData, events: &SharedEventLog) {
    let failed = data
        .files
        .iter()
        .filter(|f| f.status == FileStatus::Failed)
        .count();
    let active = data
        .files
        .iter()
        .filter(|f| f.status == FileStatus::Active)
        .count();
    let pct = percent(data.total_downloaded, data.total_size);
    let pending = data
        .files
        .iter()
        .filter(|f| f.status == FileStatus::Pending)
        .count();
    let mut lines = vec![
        Line::styled(
            if failed > 0 {
                "partial"
            } else if data.completed_files == data.total_files && data.total_files > 0 {
                "complete"
            } else {
                "running"
            },
            Style::default()
                .fg(if failed > 0 {
                    Color::Yellow
                } else {
                    Color::Green
                })
                .add_modifier(Modifier::BOLD),
        ),
        Line::raw(""),
        detail_line(
            "Files",
            format!("{}/{} complete", data.completed_files, data.total_files),
            Color::White,
        ),
        detail_line("Active files", active.to_string(), Color::Yellow),
        detail_line(
            "Failed files",
            failed.to_string(),
            if failed > 0 {
                Color::Red
            } else {
                Color::DarkGray
            },
        ),
        detail_line("Progress", format!("{pct:.1}%"), Color::Yellow),
        detail_line(
            "Bytes",
            format!(
                "{}/{}",
                format_size_iec(data.total_downloaded),
                format_size_iec(data.total_size)
            ),
            Color::Cyan,
        ),
        detail_line("Speed", format_speed(data.speed_bps), Color::Cyan),
        detail_line("Pending files", pending.to_string(), Color::DarkGray),
        detail_line("Integrity", integrity_summary(&data.files), Color::DarkGray),
    ];

    lines.push(Line::raw(""));
    lines.push(Line::styled("Files", Style::default().fg(Color::Cyan)));
    for file in data.files.iter().take(8) {
        let icon = match file.status {
            FileStatus::Complete => "\u{2713}",
            FileStatus::Active => "\u{25cf}",
            FileStatus::Failed => "\u{2717}",
            FileStatus::Pending => "\u{25cb}",
        };
        lines.push(Line::from(vec![
            Span::styled(
                format!("  {icon} "),
                Style::default().fg(status_color(file.status)),
            ),
            Span::styled(
                truncate_str(&file.filename, 34).to_string(),
                Style::default().fg(Color::White),
            ),
            Span::styled(
                format!("  {}", format_size_iec(file.total_size)),
                Style::default().fg(Color::DarkGray),
            ),
        ]));
    }
    if data.files.len() > 8 {
        lines.push(Line::styled(
            format!("  ... {} more", data.files.len() - 8),
            Style::default().fg(Color::DarkGray),
        ));
    }

    let events_lock = events.lock().unwrap();
    let latest = events_lock.back().map(DownloadEvent::display_line);
    drop(events_lock);
    if let Some(last) = latest {
        lines.push(Line::raw(""));
        lines.push(Line::styled(
            "Latest Event",
            Style::default().fg(Color::DarkGray),
        ));
        lines.push(Line::raw(last));
    }

    if failed > 0 {
        lines.push(Line::raw(""));
        lines.push(Line::styled(
            "Failed Files",
            Style::default().fg(Color::Red),
        ));
        for file in data.files.iter().filter(|f| f.status == FileStatus::Failed) {
            lines.push(Line::from(vec![
                Span::styled("  \u{2717} ", Style::default().fg(Color::Red)),
                Span::styled(file.filename.clone(), Style::default().fg(Color::White)),
            ]));
            if let Some(error) = &file.last_error {
                lines.push(Line::styled(
                    format!("    {}", truncate_str(error, 72)),
                    Style::default().fg(Color::Red),
                ));
            }
        }
        lines.push(Line::raw(""));
        lines.push(Line::styled(
            "Resume: rerun the same command to continue partial files.",
            Style::default().fg(Color::DarkGray),
        ));
    }

    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Summary ")
        .border_style(Style::default().fg(Color::Green));
    f.render_widget(Paragraph::new(lines).block(block), area);
}

// ─── Utilities ──────────────────────────────────────────────────────────────

fn progress_bar(pct: f64, width: usize) -> String {
    let width = width.clamp(4, 48);
    let ratio = (pct / 100.0).clamp(0.0, 1.0);
    let filled = (width as f64 * ratio).round() as usize;
    format!(
        "{}{}",
        "\u{2501}".repeat(filled),
        "\u{2500}".repeat(width - filled)
    )
}

fn draw_worker_strip(
    f: &mut Frame,
    area: Rect,
    worker_states: &SharedWorkerStates,
    app: &AppState,
) {
    let workers = worker_states.lock().unwrap();
    let mut spans = Vec::new();
    for (idx, worker) in workers
        .iter()
        .filter(|w| w.status != WorkerStatus::Done)
        .take(6)
        .enumerate()
    {
        if idx > 0 {
            spans.push(Span::raw("  "));
        }
        let color = worker_status_color(worker.status);
        let piece = worker
            .current_piece
            .map(|p| format!("p{p}"))
            .unwrap_or_else(|| "idle".into());
        let speed = if worker.status == WorkerStatus::Downloading {
            format_speed(app.live_speed_bps / workers.len().max(1) as f64)
        } else if worker.status == WorkerStatus::Retrying {
            "retry".into()
        } else {
            "-".into()
        };
        spans.push(Span::styled(
            display_worker_id(worker.id),
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(
            format!(" {piece} {speed}"),
            Style::default().fg(Color::White),
        ));
    }
    if spans.is_empty() {
        spans.push(Span::styled(
            "No active workers.",
            Style::default().fg(Color::DarkGray),
        ));
    }
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Worker strip ")
        .border_style(Style::default().fg(Color::Yellow));
    f.render_widget(Paragraph::new(Line::from(spans)).block(block), area);
}

fn draw_event_tail(f: &mut Frame, area: Rect, events: &SharedEventLog, title: &'static str) {
    let events_lock = events.lock().unwrap();
    let raw_lines: Vec<String> = events_lock
        .iter()
        .rev()
        .take(3)
        .map(DownloadEvent::display_line)
        .collect();
    drop(events_lock);
    let lines: Vec<Line> = if raw_lines.is_empty() {
        vec![Line::styled(
            "No events yet.",
            Style::default().fg(Color::DarkGray),
        )]
    } else {
        raw_lines
            .into_iter()
            .rev()
            .map(|line| {
                Line::raw(truncate_str(&line, area.width.saturating_sub(4) as usize).to_string())
            })
            .collect()
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(Style::default().fg(Color::Yellow));
    f.render_widget(Paragraph::new(lines).block(block), area);
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

fn copy_visible_text(app: &AppState, files: &[FileSnapshot], events: &SharedEventLog) {
    let text = match app.active_tab {
        4 => filtered_event_text(app, files, events),
        5 => summary_text(files, events),
        _ => files
            .get(app.selected_file)
            .map(selected_file_text)
            .unwrap_or_else(|| "No file selected".to_string()),
    };
    match copy_to_clipboard(&text) {
        Ok(()) => push_ui_event(events, EventSeverity::Info, "copied visible download text"),
        Err(e) => push_ui_event(events, EventSeverity::Error, format!("copy failed: {e}")),
    }
}

fn open_selected_output(app: &AppState, files: &[FileSnapshot], events: &SharedEventLog) {
    let Some(file) = files.get(app.selected_file) else {
        push_ui_event(
            events,
            EventSeverity::Error,
            "open failed: no file selected",
        );
        return;
    };
    let path = file
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

fn selected_file_text(file: &FileSnapshot) -> String {
    format!(
        "{}\nstatus: {}\nprogress: {}/{} pieces\nchecksum: {}\noutput: {}\nurl: {}",
        file.filename,
        file_status_label(file.status),
        file.completed_pieces,
        file.total_pieces,
        file.checksum_status,
        file.output.display(),
        file.url
    )
}

fn filtered_event_text(app: &AppState, files: &[FileSnapshot], events: &SharedEventLog) -> String {
    let selected_file_id = files.get(app.selected_file).map(|file| file.id);
    let selected_worker_id = Some(app.selected_worker);
    let events_lock = events.lock().unwrap();
    let lines: Vec<String> = events_lock
        .iter()
        .filter(|event| {
            app.event_filter
                .matches(event, selected_file_id, selected_worker_id)
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

fn summary_text(files: &[FileSnapshot], events: &SharedEventLog) -> String {
    let completed = files
        .iter()
        .filter(|file| file.status == FileStatus::Complete)
        .count();
    let failed = files
        .iter()
        .filter(|file| file.status == FileStatus::Failed)
        .count();
    let bytes: u64 = files.iter().map(|file| file.total_size).sum();
    let mut lines = vec![format!(
        "download summary: {completed}/{} complete, {failed} failed, {} total",
        files.len(),
        format_size(bytes)
    )];
    for file in files {
        lines.push(format!(
            "{}\t{}\t{}\t{}",
            file_status_label(file.status),
            file.filename,
            format_size(file.total_size),
            file.output.display()
        ));
    }
    let events_lock = events.lock().unwrap();
    if let Some(last) = events_lock.back().map(DownloadEvent::display_line) {
        lines.push(format!("latest event: {last}"));
    }
    lines.join("\n")
}

fn integrity_summary(files: &[FileSnapshot]) -> String {
    let configured: Vec<&FileSnapshot> = files
        .iter()
        .filter(|file| file.checksum_status != "not configured")
        .collect();
    if configured.is_empty() {
        return "checksum not configured".to_string();
    }
    let verified = configured
        .iter()
        .filter(|file| file.checksum_status.contains("verified"))
        .count();
    let failed = configured
        .iter()
        .filter(|file| {
            file.checksum_status.contains("mismatch") || file.checksum_status.contains("failed")
        })
        .count();
    let pending = configured.len().saturating_sub(verified + failed);
    format!("{verified} verified, {failed} failed, {pending} pending")
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
