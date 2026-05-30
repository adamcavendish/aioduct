use super::*;

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
pub(crate) const PIECE_DETAIL_MIN_HEIGHT: u16 = 9;
pub(crate) const PIECE_GRID_MAX_HEIGHT: u16 = 16;
const AUTO_MIN_PIECE: u64 = 64 * 1024;
const AUTO_MAX_PIECE: u64 = 4 * 1024 * 1024;
const SMALL_GRID_PIECES: u32 = 32;
const LARGE_GRID_PIECES: u32 = 128;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PieceVisualState {
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

pub(super) fn piece_visual_state(total_pieces: u32) -> PieceVisualState {
    if total_pieces <= SMALL_GRID_PIECES {
        PieceVisualState::Small
    } else if total_pieces <= LARGE_GRID_PIECES {
        PieceVisualState::Medium
    } else {
        PieceVisualState::Large
    }
}

pub(super) fn piece_grid_columns(width: usize, total_pieces: usize) -> usize {
    width.saturating_sub(1).max(1).min(total_pieces.max(1))
}

pub(super) fn piece_grid_cells_per_row(width: usize, total_pieces: usize) -> usize {
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

pub(super) fn piece_viewport_for_inner(
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
