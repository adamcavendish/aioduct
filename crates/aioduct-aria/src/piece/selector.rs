use super::storage::PieceStorage;

/// Select the next piece to download using aria2's "largest gap midpoint" strategy.
///
/// Finds the longest contiguous run of available (not complete, not in-flight) pieces,
/// then returns the midpoint of that run. This naturally distributes workers across
/// the file, maximizing parallelism.
pub fn select_piece(storage: &PieceStorage) -> Option<u32> {
    let total = storage.total_pieces();
    if total == 0 {
        return None;
    }

    let mut best_start = 0u32;
    let mut best_len = 0u32;

    let mut run_start = None;
    let mut run_len = 0u32;

    for i in 0..total {
        if storage.is_available(i) {
            if run_start.is_none() {
                run_start = Some(i);
                run_len = 0;
            }
            run_len += 1;
        } else {
            if run_len > best_len {
                best_start = run_start.unwrap_or(0);
                best_len = run_len;
            }
            run_start = None;
            run_len = 0;
        }
    }

    if run_len > best_len {
        best_start = run_start.unwrap_or(0);
        best_len = run_len;
    }

    if best_len == 0 {
        return None;
    }

    Some(best_start + best_len / 2)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selects_midpoint_of_full_range() {
        let s = PieceStorage::new(1024 * 8, 1024);
        assert_eq!(select_piece(&s), Some(4)); // midpoint of 0..8
    }

    #[test]
    fn selects_midpoint_of_largest_gap() {
        let mut s = PieceStorage::new(1024 * 10, 1024);
        // Mark pieces 3 and 4 complete → gaps are [0..3] (len 3) and [5..10] (len 5)
        s.mark_complete(3);
        s.mark_complete(4);
        // Largest gap is [5..9], len 5, midpoint = 5 + 5/2 = 7
        assert_eq!(select_piece(&s), Some(7));
    }

    #[test]
    fn skips_in_flight() {
        let mut s = PieceStorage::new(1024 * 4, 1024);
        s.mark_in_flight(0);
        s.mark_in_flight(1);
        // Available: [2, 3], midpoint = 2 + 2/2 = 3
        assert_eq!(select_piece(&s), Some(3));
    }

    #[test]
    fn returns_none_when_all_complete() {
        let mut s = PieceStorage::new(1024 * 4, 1024);
        for i in 0..4 {
            s.mark_complete(i);
        }
        assert_eq!(select_piece(&s), None);
    }

    #[test]
    fn returns_none_when_all_in_flight_or_complete() {
        let mut s = PieceStorage::new(1024 * 4, 1024);
        s.mark_complete(0);
        s.mark_complete(1);
        s.mark_in_flight(2);
        s.mark_in_flight(3);
        assert_eq!(select_piece(&s), None);
    }

    #[test]
    fn single_available_piece() {
        let mut s = PieceStorage::new(1024 * 4, 1024);
        s.mark_complete(0);
        s.mark_complete(1);
        s.mark_complete(3);
        // Only piece 2 available
        assert_eq!(select_piece(&s), Some(2));
    }
}
