//! JSONL per-request output — one JSON object per line, flushed immediately.

use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::sync::Mutex;

use crate::metrics::{RequestRecord, Sample, TestSummary};
use crate::output::Output;

/// Writes one JSON object per request to a JSONL file.
/// Line-buffered — each record is flushed immediately for `tail -f` support.
pub struct JsonlOutput {
    writer: Mutex<Option<BufWriter<std::fs::File>>>,
    path: PathBuf,
}

impl JsonlOutput {
    /// Create a new JSONL output writing to the given path.
    /// The file is created/truncated on construction.
    pub fn new(path: impl Into<PathBuf>) -> crate::error::Result<Self> {
        let path = path.into();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = std::fs::File::create(&path)?;
        let writer = BufWriter::new(file);
        Ok(Self {
            writer: Mutex::new(Some(writer)),
            path,
        })
    }

    /// The path this output writes to.
    pub fn path(&self) -> &std::path::Path {
        &self.path
    }
}

impl Output for JsonlOutput {
    fn record(&self, _sample: &Sample) {}

    fn request_done(&self, record: &RequestRecord) {
        if let Ok(mut guard) = self.writer.lock()
            && let Some(ref mut writer) = *guard
            && let Ok(json) = serde_json::to_string(record)
        {
            let _ = writeln!(writer, "{json}");
            let _ = writer.flush();
        }
    }

    fn summary(&self, _summary: &TestSummary) {}

    fn flush(&self) {
        if let Ok(mut guard) = self.writer.lock()
            && let Some(ref mut writer) = *guard
        {
            let _ = writer.flush();
        }
    }
}
