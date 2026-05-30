use super::*;

pub(super) fn copy_visible_text(target: &PieceGridTarget, app: &AppState, events: &SharedEventLog) {
    let text = match app.active_tab {
        4 => filtered_event_text(app, events),
        5 => summary_text(target, events),
        _ => format!(
            "{}\nchecksum: {}\noutput: {}\nurl: {}",
            target.filename,
            crate::download::checksum::read_status(&target.checksum_status),
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
        crate::download::checksum::read_status(&target.checksum_status),
        target.output.display(),
        target.url
    )];
    if let Some(last) = log.back().map(DownloadEvent::display_line) {
        lines.push(format!("latest event: {last}"));
    }
    lines.join("\n")
}

pub(super) fn open_output_dir(target: &PieceGridTarget, events: &SharedEventLog) {
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
