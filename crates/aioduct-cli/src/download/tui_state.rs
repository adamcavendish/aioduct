use std::collections::VecDeque;
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use time::{OffsetDateTime, UtcOffset};

use super::file_entry::FileId;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkerStatus {
    Idle,
    Downloading,
    Retrying,
    Done,
}

impl fmt::Display for WorkerStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Idle => write!(f, "idle"),
            Self::Downloading => write!(f, "downloading"),
            Self::Retrying => write!(f, "retrying"),
            Self::Done => write!(f, "done"),
        }
    }
}

pub struct WorkerState {
    pub id: usize,
    pub file_id: Option<FileId>,
    pub file_name: String,
    pub current_piece: Option<u32>,
    pub assignment_started_at: Option<Instant>,
    pub status_changed_at: Instant,
    pub piece_downloaded: Arc<AtomicU64>,
    pub piece_length: u64,
    pub speed_bps: f64,
    pub retries: u32,
    pub last_error: Option<String>,
    pub status: WorkerStatus,
}

impl WorkerState {
    pub fn new(id: usize) -> Self {
        let now = Instant::now();
        Self {
            id,
            file_id: None,
            file_name: String::new(),
            current_piece: None,
            assignment_started_at: None,
            status_changed_at: now,
            piece_downloaded: Arc::new(AtomicU64::new(0)),
            piece_length: 0,
            speed_bps: 0.0,
            retries: 0,
            last_error: None,
            status: WorkerStatus::Idle,
        }
    }

    pub fn downloaded_bytes(&self) -> u64 {
        self.piece_downloaded.load(Ordering::Relaxed)
    }

    pub fn assignment_age(&self) -> Option<Duration> {
        self.assignment_started_at.map(|started| started.elapsed())
    }
}

pub type SharedWorkerStates = Arc<Mutex<Vec<WorkerState>>>;
pub type SharedEventLog = Arc<Mutex<VecDeque<DownloadEvent>>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventSeverity {
    Info,
    Retry,
    Error,
}

impl fmt::Display for EventSeverity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Info => write!(f, "info"),
            Self::Retry => write!(f, "retry"),
            Self::Error => write!(f, "error"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventCategory {
    Assignment,
    Piece,
    Retry,
    Failure,
    Resume,
    Allocation,
    Checksum,
    Ui,
}

impl fmt::Display for EventCategory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Assignment => write!(f, "assignment"),
            Self::Piece => write!(f, "piece"),
            Self::Retry => write!(f, "retry"),
            Self::Failure => write!(f, "failure"),
            Self::Resume => write!(f, "resume"),
            Self::Allocation => write!(f, "allocation"),
            Self::Checksum => write!(f, "checksum"),
            Self::Ui => write!(f, "ui"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct DownloadEvent {
    pub timestamp_ms: u128,
    pub severity: EventSeverity,
    pub category: EventCategory,
    pub file_id: Option<FileId>,
    pub file_name: Option<String>,
    pub worker_id: Option<usize>,
    pub piece_id: Option<u32>,
    pub message: String,
}

impl DownloadEvent {
    pub fn new(
        severity: EventSeverity,
        category: EventCategory,
        message: impl Into<String>,
    ) -> Self {
        Self {
            timestamp_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis(),
            severity,
            category,
            file_id: None,
            file_name: None,
            worker_id: None,
            piece_id: None,
            message: sanitize_for_display(&message.into()),
        }
    }

    pub fn file(mut self, file_id: FileId, file_name: impl Into<String>) -> Self {
        self.file_id = Some(file_id);
        self.file_name = Some(file_name.into());
        self
    }

    pub fn worker(mut self, worker_id: usize) -> Self {
        self.worker_id = Some(worker_id);
        self
    }

    pub fn piece(mut self, piece_id: u32) -> Self {
        self.piece_id = Some(piece_id);
        self
    }

    pub fn display_line(&self) -> String {
        let mut parts = vec![format_event_timestamp(self.timestamp_ms)];
        parts.push(format!("[{}]", self.severity));
        if let Some(worker_id) = self.worker_id {
            parts.push(display_worker_id(worker_id));
        }
        if let Some(file_id) = self.file_id {
            parts.push(format!("file#{file_id}"));
        }
        if let Some(piece_id) = self.piece_id {
            parts.push(format!("piece#{piece_id}"));
        }
        parts.push(self.message.clone());
        parts.join(" ")
    }
}

pub(crate) fn display_worker_id(worker_id: usize) -> String {
    format!("W{:02}", worker_id.saturating_add(1))
}

pub(crate) fn format_event_timestamp(timestamp_ms: u128) -> String {
    let seconds = (timestamp_ms / 1000).min(i64::MAX as u128) as i64;
    let millis = timestamp_ms % 1000;
    let Ok(utc) = OffsetDateTime::from_unix_timestamp(seconds) else {
        return format_seconds_timestamp(timestamp_ms);
    };
    let local = UtcOffset::current_local_offset()
        .map(|offset| utc.to_offset(offset))
        .unwrap_or(utc);
    format!(
        "{:02}:{:02}:{:02}.{:03}",
        local.hour(),
        local.minute(),
        local.second(),
        millis
    )
}

fn format_seconds_timestamp(timestamp_ms: u128) -> String {
    let seconds = (timestamp_ms / 1000) % 86_400;
    let millis = timestamp_ms % 1000;
    let hour = seconds / 3600;
    let minute = (seconds % 3600) / 60;
    let second = seconds % 60;
    format!("{hour:02}:{minute:02}:{second:02}.{millis:03}")
}

const MAX_EVENTS: usize = 200;

pub fn new_worker_states(num_workers: usize) -> SharedWorkerStates {
    let states: Vec<WorkerState> = (0..num_workers).map(WorkerState::new).collect();
    Arc::new(Mutex::new(states))
}

pub fn new_event_log() -> SharedEventLog {
    Arc::new(Mutex::new(VecDeque::with_capacity(MAX_EVENTS)))
}

pub fn push_event(log: &SharedEventLog, msg: String) {
    push_typed_event(
        log,
        DownloadEvent::new(EventSeverity::Info, EventCategory::Piece, msg),
    );
}

pub fn push_typed_event(log: &SharedEventLog, event: DownloadEvent) {
    let mut events = log.lock().unwrap();
    if events.len() >= MAX_EVENTS {
        events.pop_front();
    }
    events.push_back(event);
}

/// Sanitize a string for terminal display: strip newlines, ANSI escapes,
/// and truncate long messages to avoid corrupting the TUI or table layout.
pub(crate) fn sanitize_for_display(msg: &str) -> String {
    let s = msg.replace('\n', " | ").replace('\r', "");
    // Strip ANSI escape sequences (e.g. from tracing spans)
    let mut cleaned = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            while let Some(&next) = chars.peek() {
                chars.next();
                if next.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            cleaned.push(c);
        }
    }
    if cleaned.len() > 500 {
        cleaned.truncate(497);
        cleaned.push_str("...");
    }
    cleaned
}

pub fn format_duration_compact(duration: Duration) -> String {
    let secs = duration.as_secs();
    if secs >= 3600 {
        format!("{}h{:02}m", secs / 3600, (secs % 3600) / 60)
    } else if secs >= 60 {
        format!("{}m{:02}s", secs / 60, secs % 60)
    } else {
        format!("{secs}s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_worker_id_is_one_based_and_padded() {
        assert_eq!(display_worker_id(0), "W01");
        assert_eq!(display_worker_id(7), "W08");
        assert_eq!(display_worker_id(23), "W24");
    }

    #[test]
    fn event_timestamp_uses_wall_clock_shape() {
        let timestamp = format_event_timestamp(1_798_578_792_096);
        assert_eq!(timestamp.len(), "14:03:12.096".len());
        assert_eq!(timestamp.as_bytes()[2], b':');
        assert_eq!(timestamp.as_bytes()[5], b':');
        assert_eq!(timestamp.as_bytes()[8], b'.');
    }

    #[test]
    fn event_display_uses_display_worker_label() {
        let event = DownloadEvent::new(EventSeverity::Info, EventCategory::Piece, "complete")
            .worker(0)
            .piece(12);
        assert!(event.display_line().contains("W01"));
        assert!(event.display_line().contains("piece#12"));
    }
}
