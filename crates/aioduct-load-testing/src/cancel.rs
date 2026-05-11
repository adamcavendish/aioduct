//! Runtime-agnostic cancellation token.
//!
//! Supports both sync polling (`is_cancelled()`) and async waiting (`cancelled().await`).

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};

use pin_project_lite::pin_project;

/// A cancellation signal that can be shared across tasks and runtimes.
///
/// Cheap to clone (Arc internally). Supports hierarchical cancellation via
/// [`child_token()`](CancellationToken::child_token).
#[derive(Clone)]
pub struct CancellationToken {
    inner: Arc<Inner>,
}

struct Inner {
    cancelled: AtomicBool,
    wakers: Mutex<Vec<Waker>>,
    parent: Option<CancellationToken>,
}

impl CancellationToken {
    /// Create a new token in the non-cancelled state.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Inner {
                cancelled: AtomicBool::new(false),
                wakers: Mutex::new(Vec::new()),
                parent: None,
            }),
        }
    }

    /// Trigger cancellation. All current and future waiters will be woken/resolve.
    pub fn cancel(&self) {
        if self
            .inner
            .cancelled
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            let wakers = std::mem::take(&mut *self.inner.wakers.lock().unwrap());
            for w in wakers {
                w.wake();
            }
        }
    }

    /// Check synchronously whether cancellation has been requested.
    pub fn is_cancelled(&self) -> bool {
        self.inner.cancelled.load(Ordering::Acquire)
            || self.inner.parent.as_ref().is_some_and(|p| p.is_cancelled())
    }

    /// Returns a future that resolves when this token (or a parent) is cancelled.
    pub fn cancelled(&self) -> CancelledFuture {
        CancelledFuture {
            token: self.clone(),
        }
    }

    /// Create a child token. Cancelled when either parent or child is cancelled directly.
    pub fn child_token(&self) -> CancellationToken {
        CancellationToken {
            inner: Arc::new(Inner {
                cancelled: AtomicBool::new(false),
                wakers: Mutex::new(Vec::new()),
                parent: Some(self.clone()),
            }),
        }
    }
}

impl Default for CancellationToken {
    fn default() -> Self {
        Self::new()
    }
}

pin_project! {
    /// Future that resolves when the associated [`CancellationToken`] is cancelled.
    pub struct CancelledFuture {
        token: CancellationToken,
    }
}

impl Future for CancelledFuture {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        if self.token.is_cancelled() {
            return Poll::Ready(());
        }
        let mut wakers = self.token.inner.wakers.lock().unwrap();
        // Double-check after acquiring the lock.
        if self.token.is_cancelled() {
            return Poll::Ready(());
        }
        wakers.push(cx.waker().clone());
        Poll::Pending
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_cancel() {
        let token = CancellationToken::new();
        assert!(!token.is_cancelled());
        token.cancel();
        assert!(token.is_cancelled());
    }

    #[test]
    fn child_inherits_parent_cancel() {
        let parent = CancellationToken::new();
        let child = parent.child_token();
        assert!(!child.is_cancelled());
        parent.cancel();
        assert!(child.is_cancelled());
    }

    #[test]
    fn child_cancel_does_not_affect_parent() {
        let parent = CancellationToken::new();
        let child = parent.child_token();
        child.cancel();
        assert!(child.is_cancelled());
        assert!(!parent.is_cancelled());
    }
}
