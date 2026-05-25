use std::sync::{Arc, Mutex};

use tokio::sync::Notify;

use super::disk_writer::DiskWriter;
use super::file_entry::{FileEntry, FileId, FileStatus};
use super::piece_grid::{PieceState, collect_piece_states};
use super::segment_man::PieceAssignment;

pub struct WorkAssignment {
    pub file_id: FileId,
    pub piece: PieceAssignment,
    pub url: String,
    pub disk_writer: Arc<DiskWriter>,
}

#[derive(Clone)]
pub struct FileSnapshot {
    pub id: FileId,
    pub filename: String,
    pub total_size: u64,
    pub piece_length: u32,
    pub total_pieces: u32,
    pub completed_pieces: u32,
    pub remaining_pieces: u32,
    pub active_workers: u32,
    pub status: FileStatus,
}

pub struct GlobalScheduler {
    inner: Mutex<SchedulerInner>,
    work_available: Notify,
}

struct SchedulerInner {
    files: Vec<FileEntry>,
    file_status: Vec<FileStatus>,
    worker_file: Vec<Option<FileId>>,
    file_worker_count: Vec<u32>,
    total_workers: usize,
}

const STICKINESS_BONUS: f64 = 1.2;
const SMALL_FILE_THRESHOLD: u64 = 2 * 1024 * 1024;

impl GlobalScheduler {
    pub fn new(total_workers: usize) -> Self {
        Self {
            inner: Mutex::new(SchedulerInner {
                files: Vec::new(),
                file_status: Vec::new(),
                worker_file: vec![None; total_workers],
                file_worker_count: Vec::new(),
                total_workers,
            }),
            work_available: Notify::new(),
        }
    }

    pub fn add_file(&self, entry: FileEntry) {
        let mut inner = self.inner.lock().unwrap();
        let id = entry.id as usize;
        while inner.file_status.len() <= id {
            inner.file_status.push(FileStatus::Pending);
            inner.file_worker_count.push(0);
        }
        inner.file_status[id] = FileStatus::Active;
        inner.file_worker_count[id] = 0;
        inner.files.push(entry);
        drop(inner);
        self.work_available.notify_waiters();
    }

    pub fn next_work(&self, worker_id: usize) -> Option<WorkAssignment> {
        let mut inner = self.inner.lock().unwrap();

        // Read previous file before releasing
        let prev_file_id = inner.worker_file[worker_id];

        // Release previous assignment
        if let Some(prev_file) = inner.worker_file[worker_id].take()
            && (prev_file as usize) < inner.file_worker_count.len()
        {
            inner.file_worker_count[prev_file as usize] =
                inner.file_worker_count[prev_file as usize].saturating_sub(1);
        }

        if inner.all_done() {
            return None;
        }

        // Score each active file
        let mut best_file_idx: Option<usize> = None;
        let mut best_score: f64 = f64::MIN;

        for (idx, file) in inner.files.iter().enumerate() {
            let fid = file.id as usize;
            if inner.file_status[fid] != FileStatus::Active {
                continue;
            }

            let remaining = file.segment_man.snapshot_storage(|s| s.remaining_pieces());
            if remaining == 0 {
                continue;
            }

            let current_workers = inner.file_worker_count[fid];

            // Cap workers for small files
            let max_workers = if file.total_size < SMALL_FILE_THRESHOLD {
                remaining.min(2)
            } else {
                remaining.min(inner.total_workers as u32)
            };

            if current_workers >= max_workers {
                continue;
            }

            // Starvation avoidance: unstarted files get priority; tie-break by smallest first
            let score = if current_workers == 0 {
                1e18 - file.total_size as f64
            } else {
                let remaining_bytes = remaining as f64 * file.piece_length as f64;
                let mut s = remaining_bytes / (current_workers as f64 + 1.0);
                // Stickiness bonus
                if prev_file_id == Some(file.id) {
                    s *= STICKINESS_BONUS;
                }
                s
            };

            if score > best_score {
                best_score = score;
                best_file_idx = Some(idx);
            }
        }

        let file_idx = best_file_idx?;
        let file = &inner.files[file_idx];
        let fid = file.id;

        let piece = file.segment_man.next_piece(worker_id)?;
        let url = file.url.clone();
        let disk_writer = Arc::clone(&file.disk_writer);

        inner.worker_file[worker_id] = Some(fid);
        inner.file_worker_count[fid as usize] += 1;

        // Activate file if it was pending
        if inner.file_status[fid as usize] == FileStatus::Pending {
            inner.file_status[fid as usize] = FileStatus::Active;
        }

        Some(WorkAssignment {
            file_id: fid,
            piece,
            url,
            disk_writer,
        })
    }

    pub fn complete_piece(&self, file_id: FileId, piece_index: u32) {
        let inner = self.inner.lock().unwrap();
        if let Some(file) = inner.files.iter().find(|f| f.id == file_id) {
            file.segment_man.complete_piece(piece_index);
            if file.segment_man.is_complete() {
                drop(inner);
                self.mark_file_complete(file_id);
            }
        }
    }

    pub fn fail_piece(&self, file_id: FileId, piece_index: u32) {
        let inner = self.inner.lock().unwrap();
        if let Some(file) = inner.files.iter().find(|f| f.id == file_id) {
            file.segment_man.fail_piece(piece_index);
        }
        drop(inner);
        self.work_available.notify_waiters();
    }

    pub fn is_piece_complete(&self, file_id: FileId, piece_index: u32) -> bool {
        let inner = self.inner.lock().unwrap();
        inner
            .files
            .iter()
            .find(|f| f.id == file_id)
            .is_some_and(|file| {
                file.segment_man
                    .snapshot_storage(|s| s.is_complete(piece_index))
            })
    }

    pub fn mark_file_complete(&self, file_id: FileId) {
        let mut inner = self.inner.lock().unwrap();
        if (file_id as usize) < inner.file_status.len() {
            inner.file_status[file_id as usize] = FileStatus::Complete;
        }
        drop(inner);
        self.work_available.notify_waiters();
    }

    pub fn mark_file_failed(&self, file_id: FileId) {
        let mut inner = self.inner.lock().unwrap();
        if (file_id as usize) < inner.file_status.len() {
            inner.file_status[file_id as usize] = FileStatus::Failed;
        }
        drop(inner);
        self.work_available.notify_waiters();
    }

    pub fn all_complete(&self) -> bool {
        self.inner.lock().unwrap().all_done()
    }

    pub fn notify_workers(&self) {
        self.work_available.notify_waiters();
    }

    pub fn work_available(&self) -> &Notify {
        &self.work_available
    }

    pub fn snapshot_files(&self) -> Vec<FileSnapshot> {
        let inner = self.inner.lock().unwrap();
        inner
            .files
            .iter()
            .map(|file| {
                let fid = file.id as usize;
                let (total_pieces, completed_pieces, remaining_pieces) =
                    file.segment_man.snapshot_storage(|s| {
                        (s.total_pieces(), s.completed_count(), s.remaining_pieces())
                    });
                FileSnapshot {
                    id: file.id,
                    filename: file.filename.clone(),
                    total_size: file.total_size,
                    piece_length: file.piece_length,
                    total_pieces,
                    completed_pieces,
                    remaining_pieces,
                    active_workers: inner.file_worker_count.get(fid).copied().unwrap_or(0),
                    status: inner
                        .file_status
                        .get(fid)
                        .copied()
                        .unwrap_or(FileStatus::Pending),
                }
            })
            .collect()
    }

    pub fn total_files(&self) -> usize {
        self.inner.lock().unwrap().files.len()
    }

    pub fn snapshot_file_pieces(&self, file_id: FileId) -> Option<(Vec<PieceState>, u32)> {
        let inner = self.inner.lock().unwrap();
        let file = inner.files.iter().find(|f| f.id == file_id)?;
        let (pieces, piece_length) = file
            .segment_man
            .snapshot_storage(|s| (collect_piece_states(s), s.piece_length()));
        Some((pieces, piece_length))
    }

    pub fn completed_files(&self) -> usize {
        let inner = self.inner.lock().unwrap();
        inner
            .file_status
            .iter()
            .filter(|s| **s == FileStatus::Complete)
            .count()
    }

    pub fn for_each_active_file<F>(&self, mut f: F)
    where
        F: FnMut(&FileEntry),
    {
        let inner = self.inner.lock().unwrap();
        for file in &inner.files {
            let fid = file.id as usize;
            if inner.file_status.get(fid).copied() == Some(FileStatus::Active) {
                f(file);
            }
        }
    }
}

impl SchedulerInner {
    fn all_done(&self) -> bool {
        if self.files.is_empty() {
            return false;
        }
        self.file_status
            .iter()
            .take(self.files.len())
            .all(|s| *s == FileStatus::Complete || *s == FileStatus::Failed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::download::piece::storage::PieceStorage;
    use crate::download::segment_man::SegmentMan;
    use std::path::PathBuf;

    fn make_entry(id: FileId, total_size: u64, piece_length: u32) -> FileEntry {
        let storage = PieceStorage::new(total_size, piece_length);
        let segment_man = Arc::new(SegmentMan::new(storage, 2));
        let disk_writer = Arc::new(DiskWriter::null());
        FileEntry {
            id,
            url: format!("http://example.com/file{id}"),
            output: PathBuf::from(format!("/tmp/file{id}")),
            filename: format!("file{id}"),
            total_size,
            piece_length,
            segment_man,
            disk_writer,
            control_path: PathBuf::from(format!("/tmp/file{id}.aioduct")),
            supports_range: true,
        }
    }

    #[test]
    fn starvation_avoidance_gives_every_file_a_worker() {
        let scheduler = GlobalScheduler::new(4);
        scheduler.add_file(make_entry(0, 100 * 1024 * 1024, 1024 * 1024));
        scheduler.add_file(make_entry(1, 1024 * 1024, 256 * 1024));
        scheduler.add_file(make_entry(2, 1024 * 1024, 256 * 1024));

        // First 3 workers should each get a different file (starvation avoidance)
        let a0 = scheduler.next_work(0).unwrap();
        let a1 = scheduler.next_work(1).unwrap();
        let a2 = scheduler.next_work(2).unwrap();

        let mut files: Vec<FileId> = vec![a0.file_id, a1.file_id, a2.file_id];
        files.sort();
        assert_eq!(files, vec![0, 1, 2]);
    }

    #[test]
    fn small_file_capped_at_two_workers() {
        let scheduler = GlobalScheduler::new(4);
        // 1MB file with 4 pieces of 256KB → small file, capped at 2 workers
        scheduler.add_file(make_entry(0, 1024 * 1024, 256 * 1024));

        let _a0 = scheduler.next_work(0).unwrap();
        let _a1 = scheduler.next_work(1).unwrap();
        // Third worker should get None (capped)
        let a2 = scheduler.next_work(2);
        assert!(a2.is_none());
    }

    #[test]
    fn file_completion_detected() {
        let scheduler = GlobalScheduler::new(2);
        // 2 pieces
        scheduler.add_file(make_entry(0, 2048, 1024));

        let a0 = scheduler.next_work(0).unwrap();
        scheduler.complete_piece(a0.file_id, a0.piece.index);

        let a1 = scheduler.next_work(0).unwrap();
        scheduler.complete_piece(a1.file_id, a1.piece.index);

        assert!(scheduler.all_complete());
    }

    #[test]
    fn workers_migrate_after_file_completes() {
        let scheduler = GlobalScheduler::new(2);
        scheduler.add_file(make_entry(0, 1024, 1024)); // 1 piece
        scheduler.add_file(make_entry(1, 4096, 1024)); // 4 pieces

        // Worker 0 gets file 0 (starvation), worker 1 gets file 1 (starvation)
        let a0 = scheduler.next_work(0).unwrap();
        let _a1 = scheduler.next_work(1).unwrap();
        assert_eq!(a0.file_id, 0);

        // Complete file 0
        scheduler.complete_piece(0, a0.piece.index);

        // Worker 0 should now get work from file 1
        let a0_next = scheduler.next_work(0).unwrap();
        assert_eq!(a0_next.file_id, 1);
    }

    #[test]
    fn starvation_prefers_smallest_unstarted_file() {
        let scheduler = GlobalScheduler::new(4);
        // Add files in decreasing size order
        scheduler.add_file(make_entry(0, 100 * 1024 * 1024, 1024 * 1024)); // 100MB
        scheduler.add_file(make_entry(1, 50 * 1024 * 1024, 1024 * 1024)); // 50MB
        scheduler.add_file(make_entry(2, 1024 * 1024, 256 * 1024)); // 1MB

        // First worker should get the smallest file (id=2)
        let a0 = scheduler.next_work(0).unwrap();
        assert_eq!(a0.file_id, 2);

        // Second worker gets next smallest (id=1)
        let a1 = scheduler.next_work(1).unwrap();
        assert_eq!(a1.file_id, 1);

        // Third worker gets the largest (id=0)
        let a2 = scheduler.next_work(2).unwrap();
        assert_eq!(a2.file_id, 0);
    }
}
