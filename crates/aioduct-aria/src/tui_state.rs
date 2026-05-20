use std::collections::VecDeque;
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crate::file_entry::FileId;

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
    pub piece_downloaded: Arc<AtomicU64>,
    pub piece_length: u64,
    pub speed_bps: f64,
    pub retries: u32,
    pub status: WorkerStatus,
}

impl WorkerState {
    pub fn new(id: usize) -> Self {
        Self {
            id,
            file_id: None,
            file_name: String::new(),
            current_piece: None,
            piece_downloaded: Arc::new(AtomicU64::new(0)),
            piece_length: 0,
            speed_bps: 0.0,
            retries: 0,
            status: WorkerStatus::Idle,
        }
    }

    pub fn downloaded_bytes(&self) -> u64 {
        self.piece_downloaded.load(Ordering::Relaxed)
    }
}

pub type SharedWorkerStates = Arc<Mutex<Vec<WorkerState>>>;
pub type SharedEventLog = Arc<Mutex<VecDeque<String>>>;

const MAX_EVENTS: usize = 200;

pub fn new_worker_states(num_workers: usize) -> SharedWorkerStates {
    let states: Vec<WorkerState> = (0..num_workers).map(WorkerState::new).collect();
    Arc::new(Mutex::new(states))
}

pub fn new_event_log() -> SharedEventLog {
    Arc::new(Mutex::new(VecDeque::with_capacity(MAX_EVENTS)))
}

pub fn push_event(log: &SharedEventLog, msg: String) {
    let mut events = log.lock().unwrap();
    if events.len() >= MAX_EVENTS {
        events.pop_front();
    }
    events.push_back(msg);
}
