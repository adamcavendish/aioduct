use std::collections::HashSet;

use super::bitfield::BitfieldMan;

pub struct PieceStorage {
    bitfield: BitfieldMan,
    in_flight: HashSet<u32>,
}

impl PieceStorage {
    pub fn new(total_length: u64, piece_length: u32) -> Self {
        Self {
            bitfield: BitfieldMan::new(total_length, piece_length),
            in_flight: HashSet::new(),
        }
    }

    pub fn from_bitfield(bitfield: BitfieldMan) -> Self {
        Self {
            bitfield,
            in_flight: HashSet::new(),
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
    }

    pub fn mark_in_flight(&mut self, index: u32) {
        self.in_flight.insert(index);
    }

    pub fn release(&mut self, index: u32) {
        self.in_flight.remove(&index);
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
    fn all_complete() {
        let mut s = PieceStorage::new(1024, 256);
        for i in 0..4 {
            s.mark_complete(i);
        }
        assert!(s.all_complete());
    }
}
