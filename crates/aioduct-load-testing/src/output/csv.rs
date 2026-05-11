//! CSV per-request output.

use std::path::PathBuf;
use std::sync::Mutex;

use crate::metrics::{RequestRecord, Sample, TestSummary};
use crate::output::Output;

/// Writes one CSV row per request. Header is auto-generated from the first record.
pub struct CsvOutput {
    writer: Mutex<Option<csv::Writer<std::fs::File>>>,
    path: PathBuf,
    header_written: Mutex<bool>,
}

impl CsvOutput {
    /// Create a new CSV output writing to the given path.
    pub fn new(path: impl Into<PathBuf>) -> crate::error::Result<Self> {
        let path = path.into();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = std::fs::File::create(&path)?;
        let writer = csv::Writer::from_writer(file);
        Ok(Self {
            writer: Mutex::new(Some(writer)),
            path,
            header_written: Mutex::new(false),
        })
    }

    /// The path this output writes to.
    pub fn path(&self) -> &std::path::Path {
        &self.path
    }
}

impl Output for CsvOutput {
    fn record(&self, _sample: &Sample) {}

    fn request_done(&self, record: &RequestRecord) {
        if let Ok(mut guard) = self.writer.lock()
            && let Some(ref mut writer) = *guard
        {
            let mut header_written = self.header_written.lock().unwrap();
            if !*header_written {
                let _ = writer.write_record([
                    "timestamp",
                    "traceparent",
                    "vu",
                    "iteration",
                    "status_code",
                    "success",
                    "latency_ms",
                    "error_category",
                    "error_msg",
                ]);
                *header_written = true;
            }
            let _ = writer.write_record([
                &record.timestamp,
                &record.traceparent,
                &record.vu.to_string(),
                &record.iteration.to_string(),
                &record.status_code.to_string(),
                &record.success.to_string(),
                &format!("{:.2}", record.latency_ms),
                &record.error_category,
                &record.error_msg,
            ]);
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
