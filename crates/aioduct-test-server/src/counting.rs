use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Clone)]
pub struct ConnectionCounter {
    connections: Arc<AtomicUsize>,
    requests: Arc<AtomicUsize>,
}

impl ConnectionCounter {
    pub fn new() -> Self {
        Self {
            connections: Arc::new(AtomicUsize::new(0)),
            requests: Arc::new(AtomicUsize::new(0)),
        }
    }

    pub fn connections(&self) -> usize {
        self.connections.load(Ordering::SeqCst)
    }

    pub fn requests(&self) -> usize {
        self.requests.load(Ordering::SeqCst)
    }

    pub fn inc_connections(&self) -> usize {
        self.connections.fetch_add(1, Ordering::SeqCst)
    }

    pub fn inc_requests(&self) -> usize {
        self.requests.fetch_add(1, Ordering::SeqCst)
    }

    pub fn reset(&self) {
        self.connections.store(0, Ordering::SeqCst);
        self.requests.store(0, Ordering::SeqCst);
    }
}

impl Default for ConnectionCounter {
    fn default() -> Self {
        Self::new()
    }
}
