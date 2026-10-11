use super::Instant;
use crate::Error;

/// An opaque association with one actual connection, not a resource handle.
///
/// Equality/hash remain valid after the resource ends. The private value and
/// Debug representation are not stable identifiers across processes/restarts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ConnectionAssociation(u64);

#[cfg(not(target_arch = "wasm32"))]
static NEXT_ASSOCIATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

#[cfg(not(target_arch = "wasm32"))]
impl ConnectionAssociation {
    #[allow(clippy::expect_used)] // Exhausting the identity space cannot wrap/reuse an identity.
    pub(crate) fn allocate() -> Self {
        Self::allocate_from(&NEXT_ASSOCIATION)
    }

    #[allow(clippy::expect_used)] // Exhaustion is an invariant failure, not silent identity reuse.
    fn allocate_from(counter: &std::sync::atomic::AtomicU64) -> Self {
        use std::sync::atomic::Ordering;
        let mut previous = counter.load(Ordering::Relaxed);
        loop {
            let next = previous
                .checked_add(1)
                .expect("connection association identity space exhausted");
            match counter.compare_exchange_weak(
                previous,
                next,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => return Self(next),
                Err(observed) => previous = observed,
            }
        }
    }
}

/// The wire mechanism that hands a response to a bidirectional channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ChannelHandoffMode {
    /// HTTP/1.1 101 Switching Protocols.
    Http1Upgrade,
    /// A successful ordinary HTTP/1 CONNECT tunnel.
    Http1Connect,
    /// A successful HTTP/2 CONNECT tunnel without :protocol.
    Http2Connect,
    /// A successful HTTP/2 CONNECT stream with :protocol.
    Http2ExtendedConnect,
}

/// Accepted response metadata, borrowed during a handoff callback.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct ChannelHandoffInfo<'a> {
    /// The actual wire mechanism.
    pub mode: ChannelHandoffMode,
    /// The declared Upgrade / :protocol token; ordinary CONNECT has none.
    pub protocol: Option<&'a str>,
    /// The accepted response status.
    pub status: http::StatusCode,
}

/// Low-frequency facts about actually acquiring an accepted response's channel.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub enum ResponseChannelHandoffEvent<'a> {
    /// The caller actually started acquiring the accepted channel.
    ChannelHandoffStarted {
        /// Accepted response metadata.
        info: ChannelHandoffInfo<'a>,
        /// Fact timestamp.
        at: Instant,
    },
    /// The channel was acquired successfully; HTTP responsibility ends here.
    ResponseChannelHandoff {
        /// Accepted response metadata.
        info: ChannelHandoffInfo<'a>,
        /// The actual underlying connection, shared by multiplexed streams.
        connection: ConnectionAssociation,
        /// Fact timestamp.
        at: Instant,
    },
    /// Acquiring the accepted channel returned a real aioduct error.
    ChannelHandoffFailed {
        /// Accepted response metadata.
        info: ChannelHandoffInfo<'a>,
        /// Original returned error, not cloned.
        error: &'a Error,
        /// Fact timestamp.
        at: Instant,
    },
    /// The started handoff future was released before its result was determined.
    ChannelHandoffCancelled {
        /// Accepted response metadata.
        info: ChannelHandoffInfo<'a>,
        /// Fact timestamp.
        at: Instant,
    },
}

/// The operation that returned a real channel I/O error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ChannelIoOperation {
    /// Reading into a caller buffer.
    Read,
    /// Accepting caller bytes for writing.
    Write,
    /// Flushing previously accepted writes.
    Flush,
    /// Shutting down the write direction.
    Shutdown,
}

/// Why the remaining channel observation capability left aioduct.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ChannelDetachReason {
    /// The wrapper was released with at least one direction not confirmed ended.
    Dropped,
    /// Raw I/O was explicitly taken out of the wrapper.
    IntoInner,
}

/// Channel facts independent of HTTP and physical connection lifecycles.
///
/// Bytes are cumulative actual wrapper delivery/acceptance, not wire or remote
/// receipt. None means bytes were not collected. An I/O error alone does not
/// end either direction. Successful reads/writes/flushes and Pending emit nothing.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub enum ChannelIoEvent<'a> {
    /// A read with nonzero available capacity confirmed EOF.
    ReadEof {
        /// Selected cumulative read delivery.
        read_bytes_so_far: Option<u64>,
    },
    /// Write shutdown completed successfully.
    WriteShutdown {
        /// Selected cumulative write acceptance.
        write_bytes_so_far: Option<u64>,
    },
    /// An actual read/write/flush/shutdown error, without inferred termination.
    IoError {
        /// The actual raw operation.
        operation: ChannelIoOperation,
        /// Borrowed original error.
        error: &'a Error,
        /// Selected cumulative read delivery.
        read_bytes_so_far: Option<u64>,
        /// Selected cumulative write acceptance.
        write_bytes_so_far: Option<u64>,
    },
    /// Remaining observation capability ended without proving resource closure.
    Detached {
        /// Wrapper release versus explicit raw extraction.
        reason: ChannelDetachReason,
        /// Whether aioduct actually confirmed read EOF.
        read_eof: bool,
        /// Whether aioduct actually confirmed successful write shutdown.
        write_shutdown: bool,
        /// Selected cumulative read delivery.
        read_bytes_so_far: Option<u64>,
        /// Selected cumulative write acceptance.
        write_bytes_so_far: Option<u64>,
    },
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    #[test]
    fn association_allocator_exhaustion_never_reuses_an_identity() {
        use std::sync::atomic::{AtomicU64, Ordering};
        let counter = AtomicU64::new(u64::MAX - 1);
        assert_eq!(
            ConnectionAssociation::allocate_from(&counter),
            ConnectionAssociation(u64::MAX)
        );
        assert!(
            std::panic::catch_unwind(|| ConnectionAssociation::allocate_from(&counter)).is_err()
        );
        assert_eq!(counter.load(Ordering::Relaxed), u64::MAX);
    }
}
