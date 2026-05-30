use super::*;

pub(super) fn copy_visible_text(app: &AppState, files: &[FileSnapshot], events: &SharedEventLog) {
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

pub(super) fn open_selected_output(
    app: &AppState,
    files: &[FileSnapshot],
    events: &SharedEventLog,
) {
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

pub(super) fn integrity_summary(files: &[FileSnapshot]) -> String {
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
