//! The Scenario trait — the core user-facing abstraction.

use std::future::Future;
use std::pin::Pin;

use crate::vu::VuContext;
use aioduct::runtime::{ConnectorSend, RuntimePoll};

/// The core trait users implement to define load test behavior.
///
/// Each call to [`run`](Scenario::run) represents one VU iteration (one "request"
/// or "transaction"). The framework calls this repeatedly according to the
/// configured [`Executor`](crate::executor::Executor).
///
/// Generic over `R: RuntimePoll, C: ConnectorSend` so the same scenario works
/// across tokio/smol.
pub trait Scenario<R: RuntimePoll, C: ConnectorSend>: Send + Sync + 'static {
    /// Execute one VU iteration.
    fn run<'a>(
        &'a self,
        ctx: &'a mut VuContext<R, C>,
    ) -> Pin<Box<dyn Future<Output = crate::error::Result<()>> + Send + 'a>>;

    /// Called once when a VU starts, before the first iteration.
    fn on_start<'a>(
        &'a self,
        _ctx: &'a mut VuContext<R, C>,
    ) -> Pin<Box<dyn Future<Output = crate::error::Result<()>> + Send + 'a>> {
        Box::pin(async { Ok(()) })
    }

    /// Called once when a VU finishes (after all iterations or on cancellation).
    fn on_stop<'a>(
        &'a self,
        _ctx: &'a mut VuContext<R, C>,
    ) -> Pin<Box<dyn Future<Output = crate::error::Result<()>> + Send + 'a>> {
        Box::pin(async { Ok(()) })
    }
}
