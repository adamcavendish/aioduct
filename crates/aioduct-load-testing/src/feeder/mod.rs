//! Data feeders that provide records to VU iterations.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use serde::de::DeserializeOwned;
use serde_json::Value;

/// Factory for creating per-VU feeder instances.
pub trait FeederFactory: Send + Sync + 'static {
    /// Create a feeder for a specific VU.
    fn create(&self, vu_id: usize, total_vus: usize) -> Box<dyn Feeder>;
}

/// Provides data records to VU iterations.
pub trait Feeder: Send + 'static {
    /// Get the next record. Returns None when exhausted.
    fn next_record(&mut self) -> Option<Value>;
}

/// JSONL-based feeder supporting per-VU files and shared-queue modes.
pub struct JsonlFeeder {
    records: Arc<Vec<Value>>,
    cursor: usize,
    cycle: bool,
}

impl JsonlFeeder {
    /// Create a feeder from pre-loaded records.
    pub fn new(records: Arc<Vec<Value>>, cycle: bool) -> Self {
        Self {
            records,
            cursor: 0,
            cycle,
        }
    }
}

impl Feeder for JsonlFeeder {
    fn next_record(&mut self) -> Option<Value> {
        if self.records.is_empty() {
            return None;
        }
        if self.cursor >= self.records.len() {
            if self.cycle {
                self.cursor = 0;
            } else {
                return None;
            }
        }
        let record = self.records[self.cursor].clone();
        self.cursor += 1;
        Some(record)
    }
}

// ── Filesystem-based feeders (native only) ───────────────────────────────────

#[cfg(not(target_arch = "wasm32"))]
mod fs_feeders;
#[cfg(not(target_arch = "wasm32"))]
pub use fs_feeders::{PerVuJsonlFeederFactory, SharedJsonlFeederFactory};

/// Factory for in-memory Vec-based feeders (works on all targets).
pub struct VecFeederFactory {
    records: Arc<Vec<Value>>,
    cycle: bool,
}

impl VecFeederFactory {
    /// Create a feeder factory from pre-loaded records.
    /// All VUs share the same data; each VU gets its own cursor.
    pub fn new(records: Vec<Value>, cycle: bool) -> Self {
        Self {
            records: Arc::new(records),
            cycle,
        }
    }
}

impl FeederFactory for VecFeederFactory {
    fn create(&self, _vu_id: usize, _total_vus: usize) -> Box<dyn Feeder> {
        Box::new(JsonlFeeder::new(self.records.clone(), self.cycle))
    }
}

/// Factory for shared-queue feeders backed by a global atomic cursor.
pub struct SharedQueueFeederFactory {
    records: Arc<Vec<Value>>,
    cursor: Arc<AtomicUsize>,
    cycle: bool,
}

impl SharedQueueFeederFactory {
    /// Create from pre-loaded records.
    pub fn from_records(records: Vec<Value>, cycle: bool) -> Self {
        Self {
            records: Arc::new(records),
            cursor: Arc::new(AtomicUsize::new(0)),
            cycle,
        }
    }

    /// Total records available.
    pub fn total_records(&self) -> usize {
        self.records.len()
    }
}

impl FeederFactory for SharedQueueFeederFactory {
    fn create(&self, _vu_id: usize, _total_vus: usize) -> Box<dyn Feeder> {
        Box::new(SharedQueueFeeder::new(
            self.records.clone(),
            self.cursor.clone(),
            self.cycle,
        ))
    }
}

pub(crate) struct SharedQueueFeeder {
    records: Arc<Vec<Value>>,
    cursor: Arc<AtomicUsize>,
    cycle: bool,
}

impl SharedQueueFeeder {
    pub(crate) fn new(records: Arc<Vec<Value>>, cursor: Arc<AtomicUsize>, cycle: bool) -> Self {
        Self {
            records,
            cursor,
            cycle,
        }
    }
}

impl Feeder for SharedQueueFeeder {
    fn next_record(&mut self) -> Option<Value> {
        if self.records.is_empty() {
            return None;
        }
        let idx = self.cursor.fetch_add(1, Ordering::Relaxed);
        if idx >= self.records.len() {
            if self.cycle {
                let wrapped = idx % self.records.len();
                Some(self.records[wrapped].clone())
            } else {
                None
            }
        } else {
            Some(self.records[idx].clone())
        }
    }
}

/// Helper to deserialize a Value into a concrete type.
pub fn deserialize_record<T: DeserializeOwned>(value: &Value) -> crate::error::Result<T> {
    serde_json::from_value(value.clone()).map_err(Into::into)
}
