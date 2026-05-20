pub struct BitfieldMan {
    bits: Vec<u8>,
    total_pieces: u32,
    piece_length: u32,
    total_length: u64,
    completed_count: u32,
}

impl BitfieldMan {
    pub fn new(total_length: u64, piece_length: u32) -> Self {
        let total_pieces = total_length.div_ceil(piece_length as u64) as u32;
        let byte_count = (total_pieces as usize).div_ceil(8);
        Self {
            bits: vec![0u8; byte_count],
            total_pieces,
            piece_length,
            total_length,
            completed_count: 0,
        }
    }

    pub fn total_pieces(&self) -> u32 {
        self.total_pieces
    }

    pub fn piece_length(&self) -> u32 {
        self.piece_length
    }

    pub fn total_length(&self) -> u64 {
        self.total_length
    }

    pub fn completed_count(&self) -> u32 {
        self.completed_count
    }

    pub fn remaining_count(&self) -> u32 {
        self.total_pieces - self.completed_count
    }

    pub fn is_all_set(&self) -> bool {
        self.completed_count == self.total_pieces
    }

    pub fn set_bit(&mut self, index: u32) {
        debug_assert!(index < self.total_pieces);
        let byte_idx = (index / 8) as usize;
        let bit_idx = 7 - (index % 8);
        if self.bits[byte_idx] & (1 << bit_idx) == 0 {
            self.bits[byte_idx] |= 1 << bit_idx;
            self.completed_count += 1;
        }
    }

    pub fn clear_bit(&mut self, index: u32) {
        debug_assert!(index < self.total_pieces);
        let byte_idx = (index / 8) as usize;
        let bit_idx = 7 - (index % 8);
        if self.bits[byte_idx] & (1 << bit_idx) != 0 {
            self.bits[byte_idx] &= !(1 << bit_idx);
            self.completed_count -= 1;
        }
    }

    pub fn is_set(&self, index: u32) -> bool {
        debug_assert!(index < self.total_pieces);
        let byte_idx = (index / 8) as usize;
        let bit_idx = 7 - (index % 8);
        self.bits[byte_idx] & (1 << bit_idx) != 0
    }

    pub fn piece_range(&self, index: u32) -> (u64, u64) {
        let offset = index as u64 * self.piece_length as u64;
        let length = if index == self.total_pieces - 1 {
            self.total_length - offset
        } else {
            self.piece_length as u64
        };
        (offset, length)
    }

    pub fn to_bytes(&self) -> &[u8] {
        &self.bits
    }

    pub fn from_bytes(bytes: &[u8], total_length: u64, piece_length: u32) -> Self {
        let total_pieces = total_length.div_ceil(piece_length as u64) as u32;
        let byte_count = (total_pieces as usize).div_ceil(8);

        let mut bits = vec![0u8; byte_count];
        let copy_len = bits.len().min(bytes.len());
        bits[..copy_len].copy_from_slice(&bytes[..copy_len]);

        let mut completed_count = 0u32;
        for i in 0..total_pieces {
            let byte_idx = (i / 8) as usize;
            let bit_idx = 7 - (i % 8);
            if bits[byte_idx] & (1 << bit_idx) != 0 {
                completed_count += 1;
            }
        }

        Self {
            bits,
            total_pieces,
            piece_length,
            total_length,
            completed_count,
        }
    }

    pub fn to_hex(&self) -> String {
        self.bits.iter().map(|b| format!("{b:02x}")).collect()
    }

    pub fn from_hex(hex: &str, total_length: u64, piece_length: u32) -> Option<Self> {
        if !hex.len().is_multiple_of(2) {
            return None;
        }
        let bytes: Option<Vec<u8>> = (0..hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).ok())
            .collect();
        Some(Self::from_bytes(&bytes?, total_length, piece_length))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_has_correct_pieces() {
        let bf = BitfieldMan::new(1024 * 1024, 256 * 1024);
        assert_eq!(bf.total_pieces(), 4);
        assert_eq!(bf.completed_count(), 0);
        assert_eq!(bf.remaining_count(), 4);
    }

    #[test]
    fn new_with_remainder() {
        let bf = BitfieldMan::new(1000, 300);
        assert_eq!(bf.total_pieces(), 4); // ceil(1000/300)
    }

    #[test]
    fn set_and_check() {
        let mut bf = BitfieldMan::new(1024, 256);
        assert!(!bf.is_set(0));
        bf.set_bit(0);
        assert!(bf.is_set(0));
        assert_eq!(bf.completed_count(), 1);
    }

    #[test]
    fn double_set_no_double_count() {
        let mut bf = BitfieldMan::new(1024, 256);
        bf.set_bit(2);
        bf.set_bit(2);
        assert_eq!(bf.completed_count(), 1);
    }

    #[test]
    fn clear_bit() {
        let mut bf = BitfieldMan::new(1024, 256);
        bf.set_bit(1);
        assert_eq!(bf.completed_count(), 1);
        bf.clear_bit(1);
        assert_eq!(bf.completed_count(), 0);
        assert!(!bf.is_set(1));
    }

    #[test]
    fn is_all_set() {
        let mut bf = BitfieldMan::new(1024, 256);
        for i in 0..4 {
            bf.set_bit(i);
        }
        assert!(bf.is_all_set());
    }

    #[test]
    fn piece_range_normal() {
        let bf = BitfieldMan::new(1024, 256);
        assert_eq!(bf.piece_range(0), (0, 256));
        assert_eq!(bf.piece_range(1), (256, 256));
        assert_eq!(bf.piece_range(3), (768, 256));
    }

    #[test]
    fn piece_range_last_smaller() {
        let bf = BitfieldMan::new(1000, 300);
        assert_eq!(bf.piece_range(0), (0, 300));
        assert_eq!(bf.piece_range(3), (900, 100));
    }

    #[test]
    fn hex_roundtrip() {
        let mut bf = BitfieldMan::new(1024, 256);
        bf.set_bit(0);
        bf.set_bit(2);
        let hex = bf.to_hex();
        let bf2 = BitfieldMan::from_hex(&hex, 1024, 256).unwrap();
        assert!(bf2.is_set(0));
        assert!(!bf2.is_set(1));
        assert!(bf2.is_set(2));
        assert!(!bf2.is_set(3));
        assert_eq!(bf2.completed_count(), 2);
    }

    #[test]
    fn bytes_roundtrip() {
        let mut bf = BitfieldMan::new(2048, 256);
        bf.set_bit(0);
        bf.set_bit(3);
        bf.set_bit(7);
        let bytes = bf.to_bytes().to_vec();
        let bf2 = BitfieldMan::from_bytes(&bytes, 2048, 256);
        assert!(bf2.is_set(0));
        assert!(bf2.is_set(3));
        assert!(bf2.is_set(7));
        assert_eq!(bf2.completed_count(), 3);
    }
}
