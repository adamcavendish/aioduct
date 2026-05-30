use super::*;

pub(super) fn render(f: &mut Frame, state: &mut TuiState) {
    let size = f.area();
    if size.width < 60 || size.height < 16 {
        let msg = Paragraph::new(vec![
            Line::styled(
                "aioduct http is still running",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Line::raw(""),
            fact_line("Status", &state.status_label, Color::Yellow),
            fact_line(
                "Body",
                human_bytes(state.body_bytes_received as u64),
                Color::Cyan,
            ),
            fact_line("Need", "terminal >= 60x16", Color::DarkGray),
        ]);
        f.render_widget(msg, size);
        return;
    }

    let main_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),
            Constraint::Min(6),
            Constraint::Length(1),
        ])
        .split(size);

    render_http_header(f, main_layout[0], state);

    if let Some(error) = &state.fatal_error {
        render_error_page(f, main_layout[1], error);
    } else {
        match state.active_page {
            0 => render_overview_page(f, main_layout[1], state),
            1 => render_trace_page(f, main_layout[1], state),
            2 => render_headers_page(f, main_layout[1], state),
            3 => render_body_page(f, main_layout[1], state),
            4 => render_events_page(f, main_layout[1], state),
            5 => render_summary_page(f, main_layout[1], state),
            _ => {}
        }
    }

    render_footer(f, main_layout[2], state);

    if state.show_help {
        render_help(f, size);
    }
}

fn render_http_header(f: &mut Frame, area: Rect, state: &TuiState) {
    let status_color = if state.final_status_is_error {
        Color::Red
    } else if state.done {
        Color::Green
    } else {
        Color::Yellow
    };
    let timing = state
        .total_duration_ms
        .map(|ms| format!(" {ms:.1}ms"))
        .unwrap_or_default();
    let protocol = if state.protocol_label.is_empty() {
        String::new()
    } else {
        format!("  {}", state.protocol_label)
    };
    let remote = if state.remote_label.is_empty() {
        String::new()
    } else {
        format!("  {}", state.remote_label)
    };
    let reserve = 28usize + protocol.len() + remote.len();
    let target = truncate_chars(
        &state.target_label,
        area.width.saturating_sub(reserve as u16) as usize,
    );
    let line1 = Line::from(vec![
        Span::styled(" aioduct http ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            format!("{} ", state.method_label),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(target, Style::default().fg(Color::Cyan)),
        Span::raw("  "),
        Span::styled(&state.status_label, Style::default().fg(status_color)),
        Span::styled(protocol, Style::default().fg(Color::Blue)),
        Span::styled(remote, Style::default().fg(Color::DarkGray)),
        Span::styled(timing, Style::default().fg(Color::Yellow)),
    ]);

    let mut tab_spans = Vec::new();
    for (idx, name) in HTTP_TABS.iter().enumerate() {
        if idx > 0 {
            tab_spans.push(Span::raw("  "));
        }
        let style = if idx == state.active_page {
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        tab_spans.push(Span::styled(format!("{} {}", idx + 1, name), style));
    }

    f.render_widget(Paragraph::new(vec![line1, Line::from(tab_spans)]), area);
}

fn render_overview_page(f: &mut Frame, area: Rect, state: &TuiState) {
    if area.width >= 92 {
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Percentage(68), Constraint::Length(4)])
            .split(area);
        let columns = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(52), Constraint::Percentage(48)])
            .split(rows[0]);
        render_timeline_panel(f, columns[0], state, "Lifecycle");
        let right = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Percentage(52), Constraint::Percentage(48)])
            .split(columns[1]);
        render_facts_panel(f, right[0], state);
        render_body_preview_panel(f, right[1], state);
        render_metrics_strip(f, rows[1], state);
    } else {
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Percentage(42),
                Constraint::Percentage(32),
                Constraint::Percentage(26),
            ])
            .split(area);
        render_timeline_panel(f, rows[0], state, "Lifecycle");
        render_facts_panel(f, rows[1], state);
        render_body_preview_panel(f, rows[2], state);
    }
}

fn render_timeline_panel(f: &mut Frame, area: Rect, state: &TuiState, title: &str) {
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

    let title = if state.filter_query.trim().is_empty() {
        title.to_string()
    } else {
        format!("{title} [filter: {}]", state.filter_query)
    };
    let block = Block::default().borders(Borders::ALL).title(title);
    let paragraph = Paragraph::new(lines).block(block);
    f.render_widget(paragraph, area);
}

fn render_facts_panel(f: &mut Frame, area: Rect, state: &TuiState) {
    let mut lines = vec![
        fact_line(
            "Status",
            &state.status_label,
            if state.final_status_is_error {
                Color::Red
            } else {
                Color::Green
            },
        ),
        fact_line(
            "Protocol",
            value_or_dash(&state.protocol_label),
            Color::Blue,
        ),
        fact_line(
            "Remote",
            value_or_dash(&state.remote_label),
            Color::DarkGray,
        ),
        fact_line(
            "Duration",
            state
                .total_duration_ms
                .map(|ms| format!("{ms:.1}ms"))
                .unwrap_or_else(|| "running".into()),
            Color::Yellow,
        ),
        fact_line(
            "Body",
            human_bytes(state.body_bytes_received as u64),
            Color::Cyan,
        ),
        fact_line(
            "Redirects",
            state.redirects.len().to_string(),
            Color::Yellow,
        ),
        fact_line("Retries", state.retries.len().to_string(), Color::Yellow),
    ];

    if state.body_is_sse {
        lines.push(Line::raw(""));
        lines.push(fact_line(
            "SSE events",
            state.sse_event_count.to_string(),
            Color::Cyan,
        ));
    }

    if let Some(last) = state.event_lines.back() {
        lines.push(Line::raw(""));
        lines.push(Line::styled("Latest", Style::default().fg(Color::DarkGray)));
        lines.push(Line::styled(
            display_event_line(last),
            Style::default().fg(last.color),
        ));
    }

    let block = Block::default().borders(Borders::ALL).title("Overview");
    f.render_widget(Paragraph::new(lines).block(block), area);
}

fn render_body_preview_panel(f: &mut Frame, area: Rect, state: &TuiState) {
    let title = if state.body_is_sse {
        "SSE tail"
    } else {
        "Body preview"
    };
    let visible = area.height.saturating_sub(2) as usize;
    let width = area.width.saturating_sub(2) as usize;
    let mut lines: Vec<Line> = state
        .body_lines
        .iter()
        .rev()
        .filter(|line| text_matches_filter(line, &state.filter_query))
        .take(visible.max(1))
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .map(|line| Line::raw(truncate_chars(line, width.max(1))))
        .collect();
    if lines.is_empty() {
        lines.push(Line::styled(
            if state.response_headers.is_empty() {
                "waiting for response body"
            } else if state.body_done {
                "empty body"
            } else {
                "body stream open"
            },
            Style::default().fg(Color::DarkGray),
        ));
    }
    let block = Block::default().borders(Borders::ALL).title(title);
    f.render_widget(Paragraph::new(lines).block(block), area);
}

fn render_metrics_strip(f: &mut Frame, area: Rect, state: &TuiState) {
    let mut spans = vec![
        Span::styled(" body ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            human_bytes(state.body_bytes_received as u64),
            Style::default().fg(Color::Cyan),
        ),
        Span::styled("  duration ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            state
                .total_duration_ms
                .map(|ms| format!("{ms:.1}ms"))
                .unwrap_or_else(|| "running".to_string()),
            Style::default().fg(Color::Yellow),
        ),
    ];
    if let Some(transfer_start) = state.transfer_start_at {
        let elapsed = state
            .transfer_end_at
            .map(|end| end.duration_since(transfer_start).as_secs_f64())
            .unwrap_or_else(|| transfer_start.elapsed().as_secs_f64());
        if elapsed > 0.0 && state.body_bytes_received > 0 {
            spans.extend([
                Span::styled("  speed ", Style::default().fg(Color::DarkGray)),
                Span::styled(
                    human_speed(state.body_bytes_received as f64 / elapsed),
                    Style::default().fg(Color::Cyan),
                ),
            ]);
        }
    }
    if state.body_is_sse {
        spans.extend([
            Span::styled("  SSE ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                format!("{} events", state.sse_event_count),
                Style::default().fg(Color::Green),
            ),
        ]);
    }
    let block = Block::default().borders(Borders::ALL).title("Metrics");
    f.render_widget(Paragraph::new(Line::from(spans)).block(block), area);
}

fn render_trace_page(f: &mut Frame, area: Rect, state: &TuiState) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(6),
            Constraint::Length(6),
            Constraint::Length(3),
        ])
        .split(area);
    let columns = if rows[1].width >= 92 {
        Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(38), Constraint::Percentage(62)])
            .split(rows[1])
    } else {
        Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
            .split(rows[1])
    };

    render_trace_waterfall(f, rows[0], state);
    render_selected_span_panel(f, columns[0], state);
    render_redirect_chain_panel(f, columns[1], state);
    render_trace_totals_panel(f, rows[2], state);
}

fn render_trace_waterfall(f: &mut Frame, area: Rect, state: &TuiState) {
    let max_ms = state
        .phases
        .iter()
        .map(|p| p.duration_ms)
        .fold(1.0, f64::max);
    let bar_width = area.width.saturating_sub(38).max(8) as usize;
    let lines: Vec<Line> = state
        .phases
        .iter()
        .enumerate()
        .map(|(idx, p)| {
            let filled = ((p.duration_ms / max_ms) * bar_width as f64).round() as usize;
            let filled = filled
                .min(bar_width)
                .max(if p.duration_ms > 0.0 { 1 } else { 0 });
            let selected = idx == state.focus_index.min(state.phases.len().saturating_sub(1));
            Line::from(vec![
                Span::styled(
                    if selected { "› " } else { "  " },
                    Style::default().fg(Color::Cyan),
                ),
                Span::styled(
                    format!("{:<18}", truncate_chars(&p.label, 18)),
                    Style::default().fg(if selected { Color::Cyan } else { p.color }),
                ),
                Span::styled(
                    " ".to_string() + &"━".repeat(filled),
                    Style::default().fg(p.color),
                ),
                Span::styled(
                    format!(" {:>8.1}ms", p.duration_ms),
                    Style::default().fg(Color::Yellow),
                ),
                Span::styled(
                    format!(" +{:>8.1}ms", p.cumulative_ms),
                    Style::default().fg(Color::DarkGray),
                ),
            ])
        })
        .collect();
    let block = Block::default().borders(Borders::ALL).title("Trace");
    f.render_widget(Paragraph::new(lines).block(block), area);
}

fn render_selected_span_panel(f: &mut Frame, area: Rect, state: &TuiState) {
    let selected = state
        .phases
        .get(state.focus_index.min(state.phases.len().saturating_sub(1)));
    let lines = if let Some(phase) = selected {
        vec![
            fact_line("Selected", truncate_chars(&phase.label, 32), phase.color),
            fact_line(
                "Duration",
                format!("{:.1}ms", phase.duration_ms),
                Color::Yellow,
            ),
            fact_line(
                "Cumulative",
                format!("{:.1}ms", phase.cumulative_ms),
                Color::DarkGray,
            ),
            fact_line("Note", phase_note(&phase.label), Color::DarkGray),
        ]
    } else {
        vec![Line::styled(
            "waiting for spans",
            Style::default().fg(Color::DarkGray),
        )]
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title("Selected span");
    f.render_widget(Paragraph::new(lines).block(block), area);
}

fn render_redirect_chain_panel(f: &mut Frame, area: Rect, state: &TuiState) {
    let mut lines: Vec<Line> = state
        .redirects
        .iter()
        .map(|(status, from, to)| {
            Line::from(vec![
                Span::styled(format!("{status} "), Style::default().fg(Color::Yellow)),
                Span::styled(
                    truncate_chars(from, 22),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::raw(" -> "),
                Span::styled(
                    truncate_chars(to, area.width.saturating_sub(30) as usize),
                    Style::default().fg(Color::Cyan),
                ),
            ])
        })
        .collect();
    if lines.is_empty() {
        lines.push(Line::styled(
            "no redirects observed",
            Style::default().fg(Color::DarkGray),
        ));
    }
    let block = Block::default()
        .borders(Borders::ALL)
        .title("Redirect chain");
    f.render_widget(Paragraph::new(lines).block(block), area);
}

fn render_trace_totals_panel(f: &mut Frame, area: Rect, state: &TuiState) {
    let slowest = state
        .phases
        .iter()
        .max_by(|a, b| {
            a.duration_ms
                .partial_cmp(&b.duration_ms)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .map(|phase| {
            format!(
                "{} {:.1}ms",
                truncate_chars(&phase.label, 18),
                phase.duration_ms
            )
        })
        .unwrap_or_else(|| "-".into());
    let setup_ms: f64 = state
        .phases
        .iter()
        .filter(|phase| {
            phase.label.starts_with("DNS")
                || phase.label.starts_with("TCP")
                || phase.label.starts_with("TLS")
                || phase.label.starts_with("POOL")
        })
        .map(|phase| phase.duration_ms)
        .sum();
    let transfer_ms: f64 = state
        .phases
        .iter()
        .filter(|phase| phase.label.starts_with("DOWN") || phase.label.starts_with("UP"))
        .map(|phase| phase.duration_ms)
        .sum();
    let line = Line::from(vec![
        Span::styled("slowest ", Style::default().fg(Color::DarkGray)),
        Span::styled(slowest, Style::default().fg(Color::Magenta)),
        Span::styled("   setup ", Style::default().fg(Color::DarkGray)),
        Span::styled(format!("{setup_ms:.1}ms"), Style::default().fg(Color::Blue)),
        Span::styled("   transfer ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            format!("{transfer_ms:.1}ms"),
            Style::default().fg(Color::Green),
        ),
    ]);
    let block = Block::default().borders(Borders::ALL).title("Totals");
    f.render_widget(Paragraph::new(line).block(block), area);
}

fn render_headers_page(f: &mut Frame, area: Rect, state: &mut TuiState) {
    let show_trailers = state.headers_show_trailers();
    if area.width >= 120 && show_trailers {
        let columns = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Percentage(34),
                Constraint::Percentage(34),
                Constraint::Percentage(32),
            ])
            .split(area);
        render_headers_panel(
            f,
            columns[0],
            focused_title(
                "Request Headers",
                state.active_page == 2 && state.focus_index == 0,
            ),
            &state.request_headers,
            state.request_header_scroll,
            &state.filter_query,
        );
        render_headers_panel(
            f,
            columns[1],
            focused_title(
                "Response Headers",
                state.active_page == 2 && state.focus_index == 1,
            ),
            &state.response_headers,
            state.response_header_scroll,
            &state.filter_query,
        );
        render_trailers_panel(
            f,
            columns[2],
            focused_title("Trailers", state.active_page == 2 && state.focus_index == 2),
            state,
        );
    } else if show_trailers {
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Percentage(34),
                Constraint::Percentage(34),
                Constraint::Percentage(32),
            ])
            .split(area);
        render_headers_panel(
            f,
            rows[0],
            focused_title(
                "Request Headers",
                state.active_page == 2 && state.focus_index == 0,
            ),
            &state.request_headers,
            state.request_header_scroll,
            &state.filter_query,
        );
        render_headers_panel(
            f,
            rows[1],
            focused_title(
                "Response Headers",
                state.active_page == 2 && state.focus_index == 1,
            ),
            &state.response_headers,
            state.response_header_scroll,
            &state.filter_query,
        );
        render_trailers_panel(
            f,
            rows[2],
            focused_title("Trailers", state.active_page == 2 && state.focus_index == 2),
            state,
        );
    } else if area.width >= 96 {
        let columns = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
            .split(area);
        render_headers_panel(
            f,
            columns[0],
            focused_title(
                "Request Headers",
                state.active_page == 2 && state.focus_index == 0,
            ),
            &state.request_headers,
            state.request_header_scroll,
            &state.filter_query,
        );
        render_headers_panel(
            f,
            columns[1],
            focused_title(
                "Response Headers",
                state.active_page == 2 && state.focus_index == 1,
            ),
            &state.response_headers,
            state.response_header_scroll,
            &state.filter_query,
        );
    } else {
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
            .split(area);
        render_headers_panel(
            f,
            rows[0],
            focused_title(
                "Request Headers",
                state.active_page == 2 && state.focus_index == 0,
            ),
            &state.request_headers,
            state.request_header_scroll,
            &state.filter_query,
        );
        render_headers_panel(
            f,
            rows[1],
            focused_title(
                "Response Headers",
                state.active_page == 2 && state.focus_index == 1,
            ),
            &state.response_headers,
            state.response_header_scroll,
            &state.filter_query,
        );
    }
}

fn render_headers_panel(
    f: &mut Frame,
    area: Rect,
    title: String,
    headers: &[(String, String)],
    scroll: usize,
    filter_query: &str,
) {
    let lines: Vec<Line> = headers
        .iter()
        .filter(|(k, v)| header_matches_filter(k, v, filter_query))
        .map(|(k, v)| {
            Line::from(vec![
                Span::styled(format!("{k}: "), Style::default().fg(Color::Cyan)),
                Span::raw(v.as_str()),
            ])
        })
        .collect();

    let block = Block::default().borders(Borders::ALL).title(title);
    let max_scroll = lines
        .len()
        .saturating_sub(area.height.saturating_sub(2) as usize);
    let scroll = scroll.min(max_scroll).min(u16::MAX as usize) as u16;
    let paragraph = Paragraph::new(lines).block(block).scroll((scroll, 0));
    f.render_widget(paragraph, area);
}

fn render_trailers_panel(f: &mut Frame, area: Rect, title: String, state: &mut TuiState) {
    let mut lines: Vec<Line> = state
        .trailers
        .iter()
        .filter(|(k, v)| header_matches_filter(k, v, &state.filter_query))
        .map(|(k, v)| {
            Line::from(vec![
                Span::styled(format!("{k}: "), Style::default().fg(Color::Cyan)),
                Span::raw(v.as_str()),
            ])
        })
        .collect();
    if lines.is_empty() {
        lines.extend(trailer_placeholder_lines(state));
    }
    let block = Block::default().borders(Borders::ALL).title(title);
    let max_scroll = lines
        .len()
        .saturating_sub(area.height.saturating_sub(2) as usize);
    let scroll = state.trailer_scroll.min(max_scroll);
    state.trailer_scroll = scroll;
    f.render_widget(
        Paragraph::new(lines)
            .block(block)
            .scroll((scroll.min(u16::MAX as usize) as u16, 0)),
        area,
    );
}

fn render_body_page(f: &mut Frame, area: Rect, state: &mut TuiState) {
    if area.width >= 100 && area.height >= 18 {
        let columns = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(68), Constraint::Percentage(32)])
            .split(area);
        render_body(f, columns[0], state);
        render_metrics_panel(f, columns[1], state);
    } else {
        render_body(f, area, state);
    }
}

fn render_events_page(f: &mut Frame, area: Rect, state: &mut TuiState) {
    let title = if state.filter_query.trim().is_empty() {
        "Events".to_string()
    } else {
        format!("Events [filter: {}]", state.filter_query)
    };
    let block = Block::default().borders(Borders::ALL).title(title);
    let inner = block.inner(area);
    f.render_widget(block, area);

    let visible = inner.height as usize;
    let filtered_events: Vec<&EventLine> = state
        .event_lines
        .iter()
        .filter(|line| text_matches_filter(&line.text, &state.filter_query))
        .collect();
    let max_scroll = filtered_events.len().saturating_sub(visible);
    let scroll = state.event_scroll.min(max_scroll);
    state.event_scroll = scroll;
    let lines: Vec<Line> = filtered_events
        .into_iter()
        .skip(scroll)
        .take(visible)
        .map(|line| Line::styled(display_event_line(line), Style::default().fg(line.color)))
        .collect();
    f.render_widget(Paragraph::new(lines), inner);
}

fn render_summary_page(f: &mut Frame, area: Rect, state: &TuiState) {
    let mut lines = vec![
        Line::styled(
            if state.final_status_is_error {
                "failure"
            } else if state.done {
                "success"
            } else {
                "running"
            },
            Style::default()
                .fg(if state.final_status_is_error {
                    Color::Red
                } else {
                    Color::Green
                })
                .add_modifier(Modifier::BOLD),
        ),
        Line::raw(""),
        fact_line(
            "Request",
            format!("{} {}", state.method_label, state.target_label),
            Color::Cyan,
        ),
        fact_line("Status", &state.status_label, Color::White),
        fact_line(
            "Protocol",
            value_or_dash(&state.protocol_label),
            Color::Blue,
        ),
        fact_line(
            "Remote",
            value_or_dash(&state.remote_label),
            Color::DarkGray,
        ),
        fact_line(
            "Total",
            state
                .total_duration_ms
                .map(|ms| format!("{ms:.1}ms"))
                .unwrap_or_else(|| "-".into()),
            Color::Yellow,
        ),
        fact_line(
            "Body",
            human_bytes(state.body_bytes_received as u64),
            Color::Cyan,
        ),
        fact_line(
            "Request headers",
            state.request_headers.len().to_string(),
            Color::White,
        ),
        fact_line(
            "Response headers",
            state.response_headers.len().to_string(),
            Color::White,
        ),
        fact_line(
            "Trailers",
            trailer_summary(state),
            if state.trailers.is_empty() {
                Color::DarkGray
            } else {
                Color::Cyan
            },
        ),
        fact_line("Body handling", body_mode_label(state), Color::DarkGray),
    ];

    if state.final_status_is_error {
        lines.push(Line::raw(""));
        lines.push(Line::styled("Failure", Style::default().fg(Color::Red)));
        lines.push(fact_line("Phase/error", &state.status_line, Color::Red));
        lines.push(fact_line(
            "Retries",
            if state.retries.is_empty() {
                "none".to_string()
            } else {
                format!("{} attempts", state.retries.len())
            },
            Color::Yellow,
        ));
        lines.push(fact_line(
            "Output",
            if state.body_bytes_received == 0 {
                "no body received"
            } else {
                "partial body received"
            },
            Color::DarkGray,
        ));
    }

    if !state.redirects.is_empty() {
        lines.push(Line::raw(""));
        lines.push(Line::styled(
            "Redirect Chain",
            Style::default().fg(Color::Yellow),
        ));
        for (idx, (status, from, to)) in state.redirects.iter().enumerate() {
            lines.push(Line::raw(format!(
                "  {}. {status} {} -> {}",
                idx + 1,
                truncate_chars(from, 34),
                truncate_chars(to, 44)
            )));
        }
    }

    if !state.retries.is_empty() {
        lines.push(Line::raw(""));
        lines.push(Line::styled("Retries", Style::default().fg(Color::Yellow)));
        for retry in &state.retries {
            lines.push(Line::raw(format!("  {retry}")));
        }
    }

    lines.push(Line::raw(""));
    lines.push(Line::styled("Trailers", Style::default().fg(Color::Cyan)));
    if state.trailers.is_empty() {
        let color = if declared_trailer_names(state).is_empty() {
            Color::DarkGray
        } else {
            Color::Yellow
        };
        for line in trailer_text_lines(state) {
            lines.push(Line::styled(
                format!("  {line}"),
                Style::default().fg(color),
            ));
        }
    } else {
        for (name, value) in &state.trailers {
            lines.push(Line::raw(format!("  {name}: {value}")));
        }
    }

    let block = Block::default().borders(Borders::ALL).title("Summary");
    f.render_widget(Paragraph::new(lines).block(block), area);
}

fn render_error_page(f: &mut Frame, area: Rect, error: &str) {
    let lines = vec![
        Line::styled(
            "request failed",
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        ),
        Line::raw(""),
        Line::styled(
            "The request did not complete. Press q to close.",
            Style::default().fg(Color::DarkGray),
        ),
        Line::raw(""),
        Line::styled(
            truncate_chars(error, area.width.saturating_sub(4) as usize),
            Style::default().fg(Color::Yellow),
        ),
    ];
    let block = Block::default().borders(Borders::ALL).title("Error");
    f.render_widget(Paragraph::new(lines).block(block), area);
}

fn render_metrics_panel(f: &mut Frame, area: Rect, state: &TuiState) {
    let mut lines: Vec<Line> = Vec::new();

    // ── Transfer stats ──
    lines.push(Line::styled(
        "── Transfer ──",
        Style::default().fg(Color::Cyan),
    ));
    lines.push(Line::from(vec![
        Span::raw("  Body: "),
        Span::styled(
            human_bytes(state.body_bytes_received as u64),
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
                Span::styled(human_speed(bps), Style::default().fg(Color::Yellow)),
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

    let block = Block::default().borders(Borders::ALL).title("Body Metrics");
    let paragraph = Paragraph::new(lines).block(block);
    f.render_widget(paragraph, area);
}

fn phase_note(label: &str) -> &'static str {
    if label.starts_with("WAIT") {
        "time-to-first-byte"
    } else if label.starts_with("DNS") || label.starts_with("TCP") || label.starts_with("TLS") {
        "transport setup"
    } else if label.starts_with("DOWN") || label.starts_with("UP") {
        "body transfer"
    } else if label.starts_with("TRAILERS") {
        "after-body metadata"
    } else if label.starts_with("FAIL") {
        "terminal error"
    } else {
        "-"
    }
}

pub(super) fn format_event_timestamp(time: Time) -> String {
    format!(
        "{:02}:{:02}:{:02}.{:03}",
        time.hour(),
        time.minute(),
        time.second(),
        time.millisecond()
    )
}

pub(super) fn event_timestamp() -> String {
    let now = OffsetDateTime::now_local().unwrap_or_else(|_| OffsetDateTime::now_utc());
    format_event_timestamp(now.time())
}

fn display_event_line(line: &EventLine) -> String {
    format!("{} {}", line.timestamp, line.text)
}

pub(super) fn declared_trailer_names(state: &TuiState) -> Vec<String> {
    state
        .response_headers
        .iter()
        .filter(|(name, _)| name.eq_ignore_ascii_case("trailer"))
        .flat_map(|(_, value)| value.split(','))
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

pub(super) fn trailers_event_text(trailers: &[(String, String)]) -> String {
    if trailers.is_empty() {
        return "trailers received: none".to_string();
    }

    let mut preview = trailers
        .iter()
        .take(2)
        .map(|(name, value)| format!("{name}: {}", truncate_chars(value, 48)))
        .collect::<Vec<_>>()
        .join(", ");
    if trailers.len() > 2 {
        preview.push_str(&format!(" (+{} more)", trailers.len() - 2));
    }
    format!("trailers received: {preview}")
}

fn trailer_placeholder_lines(state: &TuiState) -> Vec<Line<'static>> {
    let declared = declared_trailer_names(state);
    let color = if state.trailers_observable || state.body_done || declared.is_empty() {
        Color::DarkGray
    } else {
        Color::Yellow
    };
    trailer_text_lines(state)
        .into_iter()
        .map(|line| Line::styled(line, Style::default().fg(color)))
        .collect()
}

pub(super) fn trailer_text_lines(state: &TuiState) -> Vec<String> {
    if state.trailers_observable || state.body_done {
        return vec!["none received".to_string()];
    }
    let declared = declared_trailer_names(state);
    if declared.is_empty() {
        vec!["no trailers declared".to_string()]
    } else {
        vec![
            "declared, waiting after body".to_string(),
            format!("expected: {}", declared.join(", ")),
        ]
    }
}

pub(super) fn trailer_summary(state: &TuiState) -> String {
    if !state.trailers.is_empty() {
        format!("{} received", state.trailers.len())
    } else if state.trailers_observable || state.body_done {
        "none received".to_string()
    } else if !declared_trailer_names(state).is_empty() {
        "declared, waiting".to_string()
    } else {
        "no trailers declared".to_string()
    }
}

fn body_mode_label(state: &TuiState) -> String {
    let mode = if state.body_is_sse {
        "SSE"
    } else if state
        .body_lines
        .iter()
        .any(|line| line.trim_start().starts_with('{'))
    {
        "JSON/text"
    } else if state.content_type_label.is_empty()
        || state.content_type_label.starts_with("text/")
        || state.content_type_label.contains("json")
        || state.content_type_label.contains("xml")
    {
        "text"
    } else {
        "binary or mixed"
    };
    let state_label = if state.body_done { "done" } else { "streaming" };
    let cap = if state.body_cap_bytes > 64 * 1024 {
        ", truncated"
    } else {
        ""
    };
    format!("{mode}, {state_label}{cap}")
}

fn fact_line(label: &str, value: impl Into<String>, color: Color) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{label:<16}"), Style::default().fg(Color::DarkGray)),
        Span::styled(value.into(), Style::default().fg(color)),
    ])
}

fn value_or_dash(value: &str) -> &str {
    if value.is_empty() { "-" } else { value }
}

fn focused_title(title: &str, focused: bool) -> String {
    if focused {
        format!("{title} [focus]")
    } else {
        title.to_string()
    }
}

pub(super) fn http_focus_label(state: &TuiState) -> &'static str {
    match state.active_page {
        0 => {
            if state.focus_index == 1 {
                "facts"
            } else {
                "timeline"
            }
        }
        2 => match state.focus_index {
            1 => "response",
            2 if state.headers_show_trailers() => "trailers",
            _ => "request",
        },
        3 => {
            if state.focus_index == 1 {
                "metrics"
            } else {
                "body"
            }
        }
        _ => "main",
    }
}

fn text_matches_filter(text: &str, filter_query: &str) -> bool {
    let query = filter_query.trim().to_ascii_lowercase();
    query.is_empty() || text.to_ascii_lowercase().contains(&query)
}

fn header_matches_filter(name: &str, value: &str, filter_query: &str) -> bool {
    let query = filter_query.trim().to_ascii_lowercase();
    query.is_empty()
        || name.to_ascii_lowercase().contains(&query)
        || value.to_ascii_lowercase().contains(&query)
}

pub(super) fn copy_visible_text(state: &mut TuiState) {
    let text = visible_page_text(state);
    match copy_to_clipboard(&text) {
        Ok(()) => state.log_event("copied visible page to clipboard", Color::Green),
        Err(err) => state.log_event(format!("copy failed: {err}"), Color::Red),
    }
}

fn visible_page_text(state: &TuiState) -> String {
    match state.active_page {
        0 => [
            format!("status: {}", state.status_label),
            format!("protocol: {}", value_or_dash(&state.protocol_label)),
            format!("remote: {}", value_or_dash(&state.remote_label)),
            format!(
                "duration: {}",
                state
                    .total_duration_ms
                    .map(|ms| format!("{ms:.1}ms"))
                    .unwrap_or_else(|| "running".into())
            ),
            format!("body: {}", human_bytes(state.body_bytes_received as u64)),
            format!("redirects: {}", state.redirects.len()),
            format!("retries: {}", state.retries.len()),
        ]
        .join("\n"),
        1 => state
            .phases
            .iter()
            .map(|p| {
                format!(
                    "{} {:.1}ms +{:.1}ms",
                    p.label, p.duration_ms, p.cumulative_ms
                )
            })
            .collect::<Vec<_>>()
            .join("\n"),
        2 => {
            let mut lines = Vec::new();
            lines.push("[request headers]".to_string());
            lines.extend(headers_as_text(&state.request_headers, &state.filter_query));
            lines.push(String::new());
            lines.push("[response headers]".to_string());
            lines.extend(headers_as_text(
                &state.response_headers,
                &state.filter_query,
            ));
            lines.push(String::new());
            lines.push("[trailers]".to_string());
            if state.trailers.is_empty() {
                lines.extend(trailer_text_lines(state));
            } else {
                lines.extend(headers_as_text(&state.trailers, &state.filter_query));
            }
            lines.join("\n")
        }
        3 => state
            .body_lines
            .iter()
            .filter(|line| text_matches_filter(line, &state.filter_query))
            .cloned()
            .collect::<Vec<_>>()
            .join("\n"),
        4 => state
            .event_lines
            .iter()
            .filter(|line| text_matches_filter(&line.text, &state.filter_query))
            .map(display_event_line)
            .collect::<Vec<_>>()
            .join("\n"),
        5 => {
            let mut lines = vec![
                format!(
                    "result: {}",
                    if state.final_status_is_error {
                        "failure"
                    } else if state.done {
                        "success"
                    } else {
                        "running"
                    }
                ),
                format!("request: {} {}", state.method_label, state.target_label),
                format!("status: {}", state.status_label),
                format!("protocol: {}", value_or_dash(&state.protocol_label)),
                format!("remote: {}", value_or_dash(&state.remote_label)),
                format!(
                    "total: {}",
                    state
                        .total_duration_ms
                        .map(|ms| format!("{ms:.1}ms"))
                        .unwrap_or_else(|| "-".into())
                ),
                format!("body: {}", human_bytes(state.body_bytes_received as u64)),
                format!("trailers: {}", trailer_summary(state)),
            ];
            if !state.redirects.is_empty() {
                lines.push("redirects:".to_string());
                for (status, from, to) in &state.redirects {
                    lines.push(format!("  {status} {from} -> {to}"));
                }
            }
            if !state.retries.is_empty() {
                lines.push("retries:".to_string());
                lines.extend(state.retries.iter().map(|retry| format!("  {retry}")));
            }
            lines.join("\n")
        }
        _ => String::new(),
    }
}

fn headers_as_text(headers: &[(String, String)], filter_query: &str) -> Vec<String> {
    headers
        .iter()
        .filter(|(name, value)| header_matches_filter(name, value, filter_query))
        .map(|(name, value)| format!("{name}: {value}"))
        .collect()
}

// human_speed imported from crate::util

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
        .filter(|logical| text_matches_filter(logical, &state.filter_query))
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

fn render_footer(f: &mut Frame, area: Rect, state: &TuiState) {
    let quit_hint = if state.done { "q close" } else { "q cancel" };
    let hint = if state.editing_filter {
        format!("filter: {}_   Enter apply   Esc close", state.filter_query)
    } else {
        match state.active_page {
            0 => format!(
                "1-6 pages   Tab focus:{}   j/k scroll   / filter   y copy   {quit_hint}   ? help",
                http_focus_label(state)
            ),
            1 => format!(
                "1-6 pages   ←/→ span focus   h/l pages   j/k span   / filter   y copy   {quit_hint}   ? help"
            ),
            2 => format!(
                "1-6 pages   Tab column:{}   / filter   y copy   {quit_hint}   ? help",
                http_focus_label(state)
            ),
            3 => format!(
                "1-6 pages   j/k scroll body   Space autoscroll:{}   / filter   y copy   {quit_hint}   ? help",
                if state.body_auto_scroll { "on" } else { "off" }
            ),
            4 => format!(
                "1-6 pages   j/k scroll   g/G top/bottom   / filter   y copy visible   {quit_hint}   ? help"
            ),
            5 => format!("1-6 pages   y copy summary   {quit_hint}   ? help"),
            _ => format!("1-6 pages   {quit_hint}   ? help"),
        }
    };
    let paragraph = Paragraph::new(Line::styled(hint, Style::default().fg(Color::DarkGray)));
    f.render_widget(paragraph, area);
}

fn render_help(f: &mut Frame, area: Rect) {
    let help_text = vec![
        Line::from(Span::styled(
            "Keyboard Shortcuts",
            Style::default().fg(Color::Yellow),
        )),
        Line::raw(""),
        Line::raw("  q        Quit"),
        Line::raw("  Ctrl+C   Quit"),
        Line::raw("  Tab/←→   Move focus or selection"),
        Line::raw("  h/l      Previous / next page"),
        Line::raw("  1-6      Jump to page"),
        Line::raw("  /        Filter visible text"),
        Line::raw("  y        Copy visible page"),
        Line::raw("  Space    Toggle body autoscroll"),
        Line::raw("  ?        Toggle this help"),
        Line::raw("  Esc      Close help"),
        Line::raw("  j/↓      Scroll active page down"),
        Line::raw("  k/↑      Scroll active page up"),
        Line::raw("  PgDn     Scroll active page down"),
        Line::raw("  PgUp     Scroll active page up"),
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

// human_bytes imported from crate::util

// duration_ms imported from crate::util

// find_split_point imported from crate::util

// truncate_chars imported from crate::util

// redact_headers imported from crate::util
