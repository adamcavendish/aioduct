//! Filesystem-based feeder factories (native targets only).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;

use serde_json::Value;

use super::{Feeder, FeederFactory, JsonlFeeder, SharedQueueFeeder};

/// Factory for per-VU JSONL feeders (each VU gets its own file from a directory).
pub struct PerVuJsonlFeederFactory {
    files: Vec<Arc<Vec<Value>>>,
    cycle: bool,
}

impl PerVuJsonlFeederFactory {
    /// Load per-VU data from a directory containing JSONL files.
    /// Reads `_manifest.txt` for file list, or globs `*.jsonl`.
    pub fn from_dir(dir: impl AsRef<Path>, cycle: bool) -> crate::error::Result<Self> {
        let dir = dir.as_ref();
        let manifest_path = dir.join("_manifest.txt");
        let paths = if manifest_path.exists() {
            let content = std::fs::read_to_string(&manifest_path)?;
            content
                .lines()
                .filter(|l| !l.trim().is_empty())
                .map(|l| {
                    let p = PathBuf::from(l.trim());
                    if p.is_absolute() { p } else { dir.join(p) }
                })
                .collect()
        } else {
            let mut paths: Vec<PathBuf> = std::fs::read_dir(dir)?
                .filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|ext| ext == "jsonl"))
                .collect();
            paths.sort();
            paths
        };

        let files: Vec<Arc<Vec<Value>>> = paths
            .iter()
            .map(|p| {
                let content = std::fs::read_to_string(p)?;
                let records: Vec<Value> = content
                    .lines()
                    .filter(|l| !l.trim().is_empty())
                    .filter_map(|l| serde_json::from_str(l).ok())
                    .collect();
                Ok(Arc::new(records))
            })
            .collect::<crate::error::Result<_>>()?;

        Ok(Self { files, cycle })
    }

    /// Number of data files loaded.
    pub fn file_count(&self) -> usize {
        self.files.len()
    }

    /// Total records across all files.
    pub fn total_records(&self) -> usize {
        self.files.iter().map(|f| f.len()).sum()
    }
}

impl FeederFactory for PerVuJsonlFeederFactory {
    fn create(&self, vu_id: usize, _total_vus: usize) -> Box<dyn Feeder> {
        let file_idx = vu_id % self.files.len();
        Box::new(JsonlFeeder::new(self.files[file_idx].clone(), self.cycle))
    }
}

/// Factory for shared-queue JSONL feeder loaded from a single file.
pub struct SharedJsonlFeederFactory {
    records: Arc<Vec<Value>>,
    cursor: Arc<AtomicUsize>,
    cycle: bool,
}

impl SharedJsonlFeederFactory {
    /// Load a single JSONL file as a shared queue.
    pub fn from_file(path: impl AsRef<Path>, cycle: bool) -> crate::error::Result<Self> {
        let content = std::fs::read_to_string(path.as_ref())?;
        let records: Vec<Value> = content
            .lines()
            .filter(|l| !l.trim().is_empty())
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect();
        Ok(Self {
            records: Arc::new(records),
            cursor: Arc::new(AtomicUsize::new(0)),
            cycle,
        })
    }

    /// Total records in the shared file.
    pub fn total_records(&self) -> usize {
        self.records.len()
    }
}

impl FeederFactory for SharedJsonlFeederFactory {
    fn create(&self, _vu_id: usize, _total_vus: usize) -> Box<dyn Feeder> {
        Box::new(SharedQueueFeeder::new(
            self.records.clone(),
            self.cursor.clone(),
            self.cycle,
        ))
    }
}
