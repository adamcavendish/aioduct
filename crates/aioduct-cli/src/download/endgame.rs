use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use tokio_util::sync::CancellationToken;

pub struct EndGameTracker {
    threshold: u32,
    active: AtomicBool,
    piece_tokens: Mutex<HashMap<u32, CancellationToken>>,
}

impl EndGameTracker {
    pub fn new(threshold: u32) -> Self {
        Self {
            threshold,
            active: AtomicBool::new(false),
            piece_tokens: Mutex::new(HashMap::new()),
        }
    }

    pub fn check_activate(&self, remaining: u32) {
        if remaining <= self.threshold && !self.active.load(Ordering::Relaxed) {
            self.active.store(true, Ordering::Release);
        }
    }

    pub fn is_active(&self) -> bool {
        self.active.load(Ordering::Acquire)
    }

    pub fn register_worker(&self, piece_index: u32) -> CancellationToken {
        let mut tokens = self.piece_tokens.lock().unwrap();
        let token = tokens.entry(piece_index).or_default().clone();
        token.child_token()
    }

    pub fn piece_completed(&self, piece_index: u32) {
        let mut tokens = self.piece_tokens.lock().unwrap();
        if let Some(token) = tokens.remove(&piece_index) {
            token.cancel();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn activates_at_threshold() {
        let eg = EndGameTracker::new(4);
        assert!(!eg.is_active());
        eg.check_activate(5);
        assert!(!eg.is_active());
        eg.check_activate(4);
        assert!(eg.is_active());
    }

    #[test]
    fn register_and_cancel() {
        let eg = EndGameTracker::new(4);
        eg.check_activate(2);

        let child = eg.register_worker(5);
        assert!(!child.is_cancelled());

        eg.piece_completed(5);
        assert!(child.is_cancelled());
    }

    #[test]
    fn multiple_workers_same_piece() {
        let eg = EndGameTracker::new(4);
        eg.check_activate(1);

        let t1 = eg.register_worker(0);
        let t2 = eg.register_worker(0);

        assert!(!t1.is_cancelled());
        assert!(!t2.is_cancelled());

        eg.piece_completed(0);
        assert!(t1.is_cancelled());
        assert!(t2.is_cancelled());
    }
}
