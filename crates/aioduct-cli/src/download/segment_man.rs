use std::sync::Mutex;

use tokio_util::sync::CancellationToken;

use super::endgame::EndGameTracker;
use super::piece::selector;
use super::piece::storage::PieceStorage;

pub struct PieceAssignment {
    pub index: u32,
    pub offset: u64,
    pub length: u64,
}

pub struct SegmentMan {
    storage: Mutex<PieceStorage>,
    endgame: EndGameTracker,
}

impl SegmentMan {
    pub fn new(storage: PieceStorage, split_count: u32) -> Self {
        let endgame = EndGameTracker::new(split_count);
        Self {
            storage: Mutex::new(storage),
            endgame,
        }
    }

    pub fn next_piece(&self, _worker_id: usize) -> Option<PieceAssignment> {
        let mut storage = self.storage.lock().unwrap();

        if storage.all_complete() {
            return None;
        }

        self.endgame.check_activate(storage.remaining_pieces());

        if let Some(index) = selector::select_piece(&storage) {
            let (offset, length) = storage.piece_range(index);
            storage.mark_in_flight(index);
            return Some(PieceAssignment {
                index,
                offset,
                length,
            });
        }

        // In end-game mode, we can pick an in-flight piece to duplicate
        if self.endgame.is_active() {
            let total = storage.total_pieces();
            for i in 0..total {
                if !storage.is_complete(i) {
                    let (offset, length) = storage.piece_range(i);
                    return Some(PieceAssignment {
                        index: i,
                        offset,
                        length,
                    });
                }
            }
        }

        None
    }

    pub fn complete_piece(&self, index: u32) {
        let mut storage = self.storage.lock().unwrap();
        storage.mark_complete(index);
        self.endgame.piece_completed(index);
    }

    pub fn fail_piece(&self, index: u32) {
        let mut storage = self.storage.lock().unwrap();
        storage.release(index);
    }

    pub fn record_piece_retry(&self, index: u32, error: impl Into<String>) {
        let mut storage = self.storage.lock().unwrap();
        storage.record_retry(index, error);
    }

    pub fn mark_piece_failed(&self, index: u32, error: impl Into<String>) {
        let mut storage = self.storage.lock().unwrap();
        storage.mark_failed(index, error);
    }

    pub fn is_complete(&self) -> bool {
        self.storage.lock().unwrap().all_complete()
    }

    pub fn is_endgame(&self) -> bool {
        self.endgame.is_active()
    }

    pub fn register_endgame_worker(&self, piece_index: u32) -> CancellationToken {
        self.endgame.register_worker(piece_index)
    }

    pub fn snapshot_storage<F, T>(&self, f: F) -> T
    where
        F: FnOnce(&PieceStorage) -> T,
    {
        let storage = self.storage.lock().unwrap();
        f(&storage)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assigns_pieces_until_done() {
        let storage = PieceStorage::new(4096, 1024);
        let sm = SegmentMan::new(storage, 2);

        let mut assigned = Vec::new();
        for _ in 0..4 {
            let a = sm.next_piece(0).unwrap();
            assigned.push(a.index);
            sm.complete_piece(a.index);
        }

        assert_eq!(assigned.len(), 4);
        assert!(sm.is_complete());
        assert!(sm.next_piece(0).is_none());
    }

    #[test]
    fn failed_piece_returns_to_pool() {
        let storage = PieceStorage::new(2048, 1024);
        let sm = SegmentMan::new(storage, 2);

        let a = sm.next_piece(0).unwrap();
        sm.fail_piece(a.index);

        // Should be able to get that piece again
        let b = sm.next_piece(0).unwrap();
        assert!(!sm.is_complete());
        sm.complete_piece(b.index);
    }

    #[test]
    fn endgame_allows_duplicate_assignment() {
        let storage = PieceStorage::new(2048, 1024);
        let sm = SegmentMan::new(storage, 4); // threshold = 4, so endgame activates immediately

        // Assign both pieces as in-flight
        let _a = sm.next_piece(0).unwrap();
        let _b = sm.next_piece(1).unwrap();

        // In endgame, we should still get a piece (duplicate)
        let c = sm.next_piece(2);
        assert!(c.is_some());
    }
}
