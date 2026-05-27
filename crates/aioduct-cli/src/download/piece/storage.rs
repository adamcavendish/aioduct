use std::collections::HashSet;

use super::bitfield::BitfieldMan;

#[derive(Debug, Clone, Default)]
pub struct PieceMetadata {
    pub retry_count: u32,
    pub failed: bool,
    pub last_error: Option<String>,
}

pub struct PieceStorage {
    bitfield: BitfieldMan,
    in_flight: HashSet<u32>,
    metadata: Vec<PieceMetadata>,
}

impl PieceStorage {
    pub fn new(total_length: u64, piece_length: u32) -> Self {
        let bitfield = BitfieldMan::new(total_length, piece_length);
        let metadata = vec![PieceMetadata::default(); bitfield.total_pieces() as usize];
        Self {
            bitfield,
            in_flight: HashSet::new(),
            metadata,
        }
    }

    pub fn from_bitfield(bitfield: BitfieldMan) -> Self {
        let metadata = vec![PieceMetadata::default(); bitfield.total_pieces() as usize];
        Self {
            bitfield,
            in_flight: HashSet::new(),
            metadata,
        }
    }

    pub fn total_pieces(&self) -> u32 {
        self.bitfield.total_pieces()
    }

    pub fn piece_length(&self) -> u32 {
        self.bitfield.piece_length()
    }

    pub fn total_length(&self) -> u64 {
        self.bitfield.total_length()
    }

    pub fn mark_complete(&mut self, index: u32) {
        self.bitfield.set_bit(index);
        self.in_flight.remove(&index);
        if let Some(meta) = self.metadata.get_mut(index as usize) {
            meta.failed = false;
            meta.last_error = None;
        }
    }

    pub fn mark_in_flight(&mut self, index: u32) {
        self.in_flight.insert(index);
    }

    pub fn release(&mut self, index: u32) {
        self.in_flight.remove(&index);
    }

    pub fn record_retry(&mut self, index: u32, error: impl Into<String>) {
        if let Some(meta) = self.metadata.get_mut(index as usize) {
            meta.retry_count = meta.retry_count.saturating_add(1);
            meta.last_error = Some(error.into());
            meta.failed = false;
        }
    }

    pub fn mark_failed(&mut self, index: u32, error: impl Into<String>) {
        self.in_flight.remove(&index);
        if let Some(meta) = self.metadata.get_mut(index as usize) {
            meta.retry_count = meta.retry_count.saturating_add(1);
            meta.last_error = Some(error.into());
            meta.failed = true;
        }
    }

    pub fn is_complete(&self, index: u32) -> bool {
        self.bitfield.is_set(index)
    }

    pub fn is_in_flight(&self, index: u32) -> bool {
        self.in_flight.contains(&index)
    }

    pub fn all_complete(&self) -> bool {
        self.bitfield.is_all_set()
    }

    pub fn is_available(&self, index: u32) -> bool {
        !self.bitfield.is_set(index) && !self.in_flight.contains(&index)
    }

    pub fn remaining_pieces(&self) -> u32 {
        self.bitfield.remaining_count()
    }

    pub fn completed_count(&self) -> u32 {
        self.bitfield.completed_count()
    }

    pub fn piece_range(&self, index: u32) -> (u64, u64) {
        self.bitfield.piece_range(index)
    }

    pub fn bitfield(&self) -> &BitfieldMan {
        &self.bitfield
    }

    pub fn metadata(&self, index: u32) -> Option<&PieceMetadata> {
        self.metadata.get(index as usize)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_lifecycle() {
        let mut s = PieceStorage::new(1024, 256);
        assert_eq!(s.total_pieces(), 4);
        assert_eq!(s.remaining_pieces(), 4);

        assert!(s.is_available(0));
        s.mark_in_flight(0);
        assert!(!s.is_available(0));
        assert!(s.is_in_flight(0));

        s.mark_complete(0);
        assert!(!s.is_in_flight(0));
        assert!(s.is_complete(0));
        assert!(!s.is_available(0));
        assert_eq!(s.remaining_pieces(), 3);
    }

    #[test]
    fn release_returns_to_available() {
        let mut s = PieceStorage::new(1024, 256);
        s.mark_in_flight(2);
        assert!(!s.is_available(2));
        s.release(2);
        assert!(s.is_available(2));
    }

    #[test]
    fn records_retry_and_failure_metadata() {
        let mut s = PieceStorage::new(1024, 256);
        s.record_retry(1, "timeout");
        let meta = s.metadata(1).unwrap();
        assert_eq!(meta.retry_count, 1);
        assert_eq!(meta.last_error.as_deref(), Some("timeout"));
        assert!(!meta.failed);

        s.mark_failed(1, "exhausted");
        let meta = s.metadata(1).unwrap();
        assert_eq!(meta.retry_count, 2);
        assert_eq!(meta.last_error.as_deref(), Some("exhausted"));
        assert!(meta.failed);
    }

    #[test]
    fn all_complete() {
        let mut s = PieceStorage::new(1024, 256);
        for i in 0..4 {
            s.mark_complete(i);
        }
        assert!(s.all_complete());
    }
}
