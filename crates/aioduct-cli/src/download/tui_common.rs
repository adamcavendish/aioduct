use crossterm::event::KeyCode;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use super::tui_state::{
    DownloadEvent, EventCategory, EventSeverity, SharedEventLog, WorkerStatus, push_typed_event,
};

pub(crate) const DOWNLOAD_TABS: [&str; 6] =
    ["Queue", "File", "Pieces", "Workers", "Events", "Summary"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HorizontalKeyAction {
    PrevPage,
    NextPage,
    PrevPiece,
    NextPiece,
}

pub(crate) fn horizontal_key_action(
    code: KeyCode,
    active_tab: usize,
) -> Option<HorizontalKeyAction> {
    const PIECES_TAB: usize = 2;

    match code {
        KeyCode::Left => Some(if active_tab == PIECES_TAB {
            HorizontalKeyAction::PrevPiece
        } else {
            HorizontalKeyAction::PrevPage
        }),
        KeyCode::Right => Some(if active_tab == PIECES_TAB {
            HorizontalKeyAction::NextPiece
        } else {
            HorizontalKeyAction::NextPage
        }),
        KeyCode::Char('h') => Some(HorizontalKeyAction::PrevPage),
        KeyCode::Char('l') => Some(HorizontalKeyAction::NextPage),
        _ => None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EventFilter {
    All,
    Failures,
    Retries,
    Worker,
    SelectedFile,
}

impl EventFilter {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Failures => "failures",
            Self::Retries => "retries",
            Self::Worker => "worker",
            Self::SelectedFile => "selected file",
        }
    }

    pub(crate) fn matches(
        self,
        event: &DownloadEvent,
        selected_file_id: Option<super::file_entry::FileId>,
        selected_worker_id: Option<usize>,
    ) -> bool {
        match self {
            Self::All => true,
            Self::Failures => event.severity == EventSeverity::Error,
            Self::Retries => event.severity == EventSeverity::Retry,
            Self::Worker => selected_worker_id
                .map(|id| event.worker_id == Some(id))
                .unwrap_or_else(|| event.worker_id.is_some()),
            Self::SelectedFile => selected_file_id
                .map(|id| event.file_id == Some(id))
                .unwrap_or(true),
        }
    }

    pub(crate) fn next(self) -> Self {
        match self {
            Self::All => Self::Failures,
            Self::Failures => Self::Retries,
            Self::Retries => Self::Worker,
            Self::Worker => Self::SelectedFile,
            Self::SelectedFile => Self::All,
        }
    }
}

pub(crate) fn percent(done: u64, total: u64) -> f64 {
    if total > 0 {
        done as f64 / total as f64 * 100.0
    } else {
        0.0
    }
}

pub(crate) fn detail_line(label: &str, value: impl Into<String>, color: Color) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{label:<12}"), Style::default().fg(Color::DarkGray)),
        Span::styled(value.into(), Style::default().fg(color)),
    ])
}

pub(crate) fn legend_line() -> Line<'static> {
    Line::from(vec![
        Span::styled("legend ", Style::default().fg(Color::DarkGray)),
        Span::styled("complete ", Style::default().fg(Color::Green)),
        Span::styled("active ", Style::default().fg(Color::Yellow)),
        Span::styled("retry ", Style::default().fg(Color::Red)),
        Span::styled("pending ", Style::default().fg(Color::DarkGray)),
        Span::styled("selected", Style::default().fg(Color::Cyan)),
    ])
}

pub(crate) fn worker_status_color(status: WorkerStatus) -> Color {
    match status {
        WorkerStatus::Idle => Color::DarkGray,
        WorkerStatus::Downloading => Color::Green,
        WorkerStatus::Retrying => Color::Yellow,
        WorkerStatus::Done => Color::DarkGray,
    }
}

pub(crate) fn format_speed(bps: f64) -> String {
    crate::util::human_speed(bps)
}

pub(crate) fn format_speed_compact(bps: f64) -> String {
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

pub(crate) fn format_size_compact(bytes: u64) -> String {
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

pub(crate) fn format_size_iec(bytes: u64) -> String {
    crate::util::human_bytes(bytes)
}

pub(crate) fn format_eta(secs: f64) -> String {
    let s = secs as u64;
    if s >= 3600 {
        format!("{}h{:02}m", s / 3600, (s % 3600) / 60)
    } else if s >= 60 {
        format!("{}m{:02}s", s / 60, s % 60)
    } else {
        format!("{s}s")
    }
}

pub(crate) fn truncate_str(s: &str, max: usize) -> &str {
    crate::util::truncate_str(s, max)
}

pub(crate) fn truncate_chars(value: &str, max_chars: usize) -> String {
    crate::util::truncate_chars(value, max_chars)
}

pub(crate) fn draw_download_help_overlay(f: &mut Frame, area: Rect, title: &'static str) {
    let width = 42u16;
    let height = 15u16;
    let x = area.width.saturating_sub(width) / 2;
    let y = area.height.saturating_sub(height) / 2;
    let popup_area = Rect::new(x, y, width.min(area.width), height.min(area.height));

    f.render_widget(Clear, popup_area);

    let lines = vec![
        Line::raw(""),
        key_line("1-6", "←→", "Pages / pieces"),
        key_line("Tab", "Enter", "Focus / detail"),
        key_line("↑↓", "j k", "Scroll"),
        key_line("PgUp", "PgDn", "Page scroll"),
        key_line("Home", "End", "Top / Bottom"),
        key_line("/", "Enter", "Edit text filter"),
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
            Span::raw("              Cancel / close"),
        ]),
        Line::from(vec![
            Span::styled("Ctrl+C", Style::default().fg(Color::Cyan)),
            Span::raw("          Force cancel"),
        ]),
        Line::from(vec![
            Span::styled("  y", Style::default().fg(Color::Cyan)),
            Span::raw("              Copy visible"),
        ]),
    ];

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan).bg(Color::Black))
        .style(Style::default().bg(Color::Black))
        .title(title);
    f.render_widget(Paragraph::new(lines).block(block), popup_area);
}

pub(crate) fn draw_cancel_overlay(f: &mut Frame, area: Rect, subject: &str) {
    let width = 48u16;
    let height = 8u16;
    let x = area.width.saturating_sub(width) / 2;
    let y = area.height.saturating_sub(height) / 2;
    let popup_area = Rect::new(x, y, width.min(area.width), height.min(area.height));

    f.render_widget(Clear, popup_area);

    let lines = vec![
        Line::raw(""),
        Line::styled(
            format!(" Cancel active {subject}?"),
            Style::default().fg(Color::Yellow),
        ),
        Line::raw(""),
        Line::from(vec![
            Span::styled("  Enter", Style::default().fg(Color::Green)),
            Span::raw("  keep downloading"),
        ]),
        Line::from(vec![
            Span::styled("  Esc", Style::default().fg(Color::Cyan)),
            Span::raw("    close dialog"),
        ]),
        Line::from(vec![
            Span::styled("  q", Style::default().fg(Color::Red)),
            Span::raw("      cancel now"),
        ]),
    ];

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Yellow).bg(Color::Black))
        .style(Style::default().bg(Color::Black))
        .title("Cancel");
    f.render_widget(Paragraph::new(lines).block(block), popup_area);
}

pub(crate) fn open_path(path: &std::path::Path) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        open_with_command("open", path)
    }

    #[cfg(target_os = "linux")]
    {
        open_with_command("xdg-open", path)
    }

    #[cfg(target_os = "windows")]
    {
        let path = path.display().to_string();
        std::process::Command::new("cmd")
            .args(["/C", "start", "", &path])
            .status()
            .map_err(|e| format!("open failed: {e}"))
            .and_then(|status| {
                if status.success() {
                    Ok(())
                } else {
                    Err(format!("open exited with status {status}"))
                }
            })
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        let _ = path;
        Err("open command is not available on this platform".to_string())
    }
}

pub(crate) fn push_ui_event(
    events: &SharedEventLog,
    severity: EventSeverity,
    message: impl Into<String>,
) {
    push_typed_event(
        events,
        DownloadEvent::new(severity, EventCategory::Ui, message),
    );
}

fn open_with_command(command: &str, path: &std::path::Path) -> Result<(), String> {
    let status = std::process::Command::new(command)
        .arg(path)
        .status()
        .map_err(|e| format!("{command} failed to start: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{command} exited with status {status}"))
    }
}

fn key_line(left: &'static str, right: &'static str, label: &'static str) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("  {left}"), Style::default().fg(Color::Cyan)),
        Span::styled(" / ", Style::default().fg(Color::DarkGray)),
        Span::styled(right, Style::default().fg(Color::Cyan)),
        Span::raw(format!("    {label}")),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn download_tabs_match_flight_deck_pages() {
        assert_eq!(
            DOWNLOAD_TABS,
            ["Queue", "File", "Pieces", "Workers", "Events", "Summary"]
        );
    }

    #[test]
    fn horizontal_keys_switch_pages_except_on_pieces() {
        assert_eq!(
            horizontal_key_action(KeyCode::Left, 0),
            Some(HorizontalKeyAction::PrevPage)
        );
        assert_eq!(
            horizontal_key_action(KeyCode::Right, 0),
            Some(HorizontalKeyAction::NextPage)
        );
        assert_eq!(
            horizontal_key_action(KeyCode::Left, 2),
            Some(HorizontalKeyAction::PrevPiece)
        );
        assert_eq!(
            horizontal_key_action(KeyCode::Right, 2),
            Some(HorizontalKeyAction::NextPiece)
        );
    }

    #[test]
    fn vim_horizontal_keys_keep_page_navigation() {
        assert_eq!(
            horizontal_key_action(KeyCode::Char('h'), 2),
            Some(HorizontalKeyAction::PrevPage)
        );
        assert_eq!(
            horizontal_key_action(KeyCode::Char('l'), 2),
            Some(HorizontalKeyAction::NextPage)
        );
    }

    #[test]
    fn percent_handles_empty_totals() {
        assert_eq!(percent(10, 0), 0.0);
        assert_eq!(percent(25, 100), 25.0);
    }

    #[test]
    fn truncate_helpers_preserve_char_boundaries() {
        assert_eq!(truncate_str("abcdef", 3), "abc");
        assert_eq!(truncate_str("éclair", 2), "é");
        assert_eq!(truncate_chars("abcdef", 4), "abc…");
    }

    #[test]
    fn compact_formatters_are_stable() {
        assert_eq!(format_size_compact(1536), "2K");
        assert_eq!(format_speed_compact(1536.0), "2K/s");
        assert_eq!(format_eta(65.0), "1m05s");
    }

    #[test]
    fn iec_size_formatter_matches_draft_labels() {
        assert_eq!(format_size_iec(64 * 1024), "64 KiB");
        assert_eq!(format_size_iec(320 * 1024), "320 KiB");
        assert_eq!(format_size_iec(4 * 1024 * 1024), "4 MiB");
        assert_eq!(format_size_iec(3_276_800), "3.1 MiB");
    }
}
