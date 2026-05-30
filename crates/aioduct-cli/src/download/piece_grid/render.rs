use super::*;

// ─── Main UI Layout ─────────────────────────────────────────────────────────

pub(super) fn draw_ui(
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
            crate::download::checksum::read_status(&target.checksum_status),
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
            crate::download::checksum::read_status(&target.checksum_status),
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
        .filter(|event| event.severity == EventSeverity::Retry)
        .count();
    let failure_count = raw_events
        .iter()
        .filter(|event| event.severity == EventSeverity::Error)
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
