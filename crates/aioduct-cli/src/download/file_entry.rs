use std::path::PathBuf;
use std::sync::Arc;

use super::checksum::SharedChecksumStatus;
use super::disk_writer::DiskWriter;
use super::segment_man::SegmentMan;

pub type FileId = u32;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum FileStatus {
    Pending,
    Active,
    Complete,
    Failed,
}

pub struct FileEntry {
    pub id: FileId,
    pub url: String,
    pub output: PathBuf,
    pub filename: String,
    pub total_size: u64,
    pub piece_length: u32,
    pub segment_man: Arc<SegmentMan>,
    pub disk_writer: Arc<DiskWriter>,
    pub control_path: PathBuf,
    pub supports_range: bool,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub created_at: String,
    pub resume_skipped_pieces: u32,
    pub checksum_status: SharedChecksumStatus,
}
