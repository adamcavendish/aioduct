use std::{rc::Rc, sync::Arc};

use crate::{
    Error,
    body::{LocalBodyObserver, SendBodyObserver},
    observer::{
        ChannelDetachReason, ChannelHandoffInfo, ChannelHandoffMode, ChannelIoEvent,
        ChannelIoOperation, ConnectionAssociation, Instant, Interest, ResponseChannelHandoffEvent,
    },
};

#[derive(Clone)]
enum Protocol {
    Upgrade(http::HeaderValue),
    Extended(hyper::ext::Protocol),
}

/// Only actual accepted handoffs carry this private dispatch-owned metadata.
#[derive(Clone)]
pub(crate) struct HandoffMetadata {
    connection: Option<ConnectionAssociation>,
    mode: ChannelHandoffMode,
    protocol: Option<Protocol>,
    status: http::StatusCode,
}

pub(crate) struct HandoffRequest {
    method: http::Method,
    protocol: Option<hyper::ext::Protocol>,
    upgrade_offered: bool,
    association_requested: bool,
}
impl HandoffRequest {
    pub(crate) fn capture<B>(request: &http::Request<B>) -> Option<Self> {
        let interest = request
            .extensions()
            .get::<crate::observer::RequestAttemptCounter>()?
            .interest();
        if !interest.intersects(Interest::RESPONSE_CHANNEL_HANDOFF | Interest::CHANNEL_LIFECYCLE) {
            return None;
        }
        let upgrade_offered = request.headers().contains_key(http::header::UPGRADE)
            && request
                .headers()
                .get_all(http::header::CONNECTION)
                .iter()
                .any(|value| {
                    value.to_str().is_ok_and(|value| {
                        value
                            .split(',')
                            .any(|token| token.trim().eq_ignore_ascii_case("upgrade"))
                    })
                });
        Some(Self {
            method: request.method().clone(),
            protocol: request.extensions().get::<hyper::ext::Protocol>().cloned(),
            upgrade_offered,
            association_requested: interest.contains(Interest::RESPONSE_CHANNEL_HANDOFF),
        })
    }
}

impl HandoffMetadata {
    pub(crate) fn selected(
        request: &HandoffRequest,
        version: http::Version,
        status: http::StatusCode,
        headers: &http::HeaderMap,
        connection: impl FnOnce() -> ConnectionAssociation,
    ) -> Option<Self> {
        let (mode, protocol) = match version {
            http::Version::HTTP_11
                if request.upgrade_offered && status == http::StatusCode::SWITCHING_PROTOCOLS =>
            {
                (
                    ChannelHandoffMode::Http1Upgrade,
                    headers
                        .get(http::header::UPGRADE)
                        .cloned()
                        .map(Protocol::Upgrade),
                )
            }
            http::Version::HTTP_10 | http::Version::HTTP_11
                if request.method == http::Method::CONNECT && status.is_success() =>
            {
                (ChannelHandoffMode::Http1Connect, None)
            }
            http::Version::HTTP_2
                if request.method == http::Method::CONNECT && status.is_success() =>
            {
                match request.protocol.as_ref() {
                    Some(protocol) => (
                        ChannelHandoffMode::Http2ExtendedConnect,
                        Some(Protocol::Extended(protocol.clone())),
                    ),
                    None => (ChannelHandoffMode::Http2Connect, None),
                }
            }
            _ => return None,
        };
        Some(Self {
            connection: request.association_requested.then(connection),
            mode,
            protocol,
            status,
        })
    }
    fn info(&self) -> ChannelHandoffInfo<'_> {
        ChannelHandoffInfo {
            mode: self.mode,
            status: self.status,
            protocol: self.protocol.as_ref().and_then(|protocol| match protocol {
                Protocol::Upgrade(value) => value.to_str().ok(),
                Protocol::Extended(value) => Some(value.as_str()),
            }),
        }
    }
}

pub(super) trait Receiver: Clone {
    fn interest(&self) -> Interest;
    fn handoff(&self, event: &ResponseChannelHandoffEvent<'_>);
    fn io(&self, event: &ChannelIoEvent<'_>);
}
impl Receiver for Arc<dyn SendBodyObserver> {
    fn interest(&self) -> Interest {
        self.sse_interest()
    }
    fn handoff(&self, event: &ResponseChannelHandoffEvent<'_>) {
        self.on_response_channel_handoff(event);
    }
    fn io(&self, event: &ChannelIoEvent<'_>) {
        self.on_channel_io(event);
    }
}
impl Receiver for Rc<dyn LocalBodyObserver> {
    fn interest(&self) -> Interest {
        self.sse_interest()
    }
    fn handoff(&self, event: &ResponseChannelHandoffEvent<'_>) {
        self.on_response_channel_handoff(event);
    }
    fn io(&self, event: &ChannelIoEvent<'_>) {
        self.on_channel_io(event);
    }
}

/// A started handoff has its own cancellation responsibility, not a channel guard.
pub(super) struct Handoff<R: Receiver> {
    receiver: R,
    metadata: HandoffMetadata,
    waiting: bool,
}
impl<R: Receiver> Handoff<R> {
    pub(super) fn resolved(&mut self) {
        self.waiting = false;
    }
    pub(super) fn start(receiver: R, metadata: HandoffMetadata) -> Self {
        let handoff = Self {
            receiver,
            metadata,
            waiting: true,
        };
        if handoff
            .receiver
            .interest()
            .contains(Interest::RESPONSE_CHANNEL_HANDOFF)
        {
            handoff
                .receiver
                .handoff(&ResponseChannelHandoffEvent::ChannelHandoffStarted {
                    info: handoff.metadata.info(),
                    at: Instant::now(),
                });
        }
        handoff
    }
    pub(super) fn failed(&mut self, error: &Error) {
        self.waiting = false;
        if self
            .receiver
            .interest()
            .contains(Interest::RESPONSE_CHANNEL_HANDOFF)
        {
            self.receiver
                .handoff(&ResponseChannelHandoffEvent::ChannelHandoffFailed {
                    info: self.metadata.info(),
                    error,
                    at: Instant::now(),
                });
        }
    }
    pub(super) fn succeeded(mut self) -> Option<Box<Channel<R>>> {
        self.waiting = false;
        let channel = self
            .receiver
            .interest()
            .contains(Interest::CHANNEL_LIFECYCLE)
            .then(|| Box::new(Channel::new(&self.receiver)));
        if self
            .receiver
            .interest()
            .contains(Interest::RESPONSE_CHANNEL_HANDOFF)
            && let Some(connection) = self.metadata.connection
        {
            self.receiver
                .handoff(&ResponseChannelHandoffEvent::ResponseChannelHandoff {
                    info: self.metadata.info(),
                    connection,
                    at: Instant::now(),
                });
        }
        channel
    }
}
impl<R: Receiver> Drop for Handoff<R> {
    fn drop(&mut self) {
        if std::mem::replace(&mut self.waiting, false)
            && self
                .receiver
                .interest()
                .contains(Interest::RESPONSE_CHANNEL_HANDOFF)
        {
            self.receiver
                .handoff(&ResponseChannelHandoffEvent::ChannelHandoffCancelled {
                    info: self.metadata.info(),
                    at: Instant::now(),
                });
        }
    }
}

pub(super) struct Channel<R: Receiver> {
    receiver: R,
    // Allocated only when byte collection is selected, not on lifecycle-only paths.
    bytes: Option<Box<(u64, u64)>>,
    read_eof: bool,
    write_shutdown: bool,
    detached: bool,
}
impl<R: Receiver + Clone> Channel<R> {
    fn new(receiver: &R) -> Self {
        Self {
            receiver: receiver.clone(),
            bytes: receiver
                .interest()
                .contains(Interest::CHANNEL_BYTES)
                .then(|| Box::new((0, 0))),
            read_eof: false,
            write_shutdown: false,
            detached: false,
        }
    }
}
impl<R: Receiver> Channel<R> {
    fn read_bytes(&self) -> Option<u64> {
        self.bytes.as_ref().map(|bytes| bytes.0)
    }
    fn write_bytes(&self) -> Option<u64> {
        self.bytes.as_ref().map(|bytes| bytes.1)
    }
    pub(super) fn read(&mut self, available: usize, delivered: usize) {
        if self.detached || self.read_eof {
            return;
        }
        if let Some(bytes) = &mut self.bytes {
            bytes.0 = bytes.0.saturating_add(delivered as u64);
        }
        if available != 0 && delivered == 0 {
            self.read_eof = true;
            self.receiver.io(&ChannelIoEvent::ReadEof {
                read_bytes_so_far: self.read_bytes(),
            });
        }
    }
    pub(super) fn written(&mut self, accepted: usize) {
        if !self.detached
            && !self.write_shutdown
            && let Some(bytes) = &mut self.bytes
        {
            bytes.1 = bytes.1.saturating_add(accepted as u64);
        }
    }
    pub(super) fn shutdown(&mut self) {
        if !self.detached && !std::mem::replace(&mut self.write_shutdown, true) {
            self.receiver.io(&ChannelIoEvent::WriteShutdown {
                write_bytes_so_far: self.write_bytes(),
            });
        }
    }
    pub(super) fn error(&self, operation: ChannelIoOperation, error: &Error) {
        let direction_ended = match operation {
            ChannelIoOperation::Read => self.read_eof,
            ChannelIoOperation::Write
            | ChannelIoOperation::Flush
            | ChannelIoOperation::Shutdown => self.write_shutdown,
        };
        if !self.detached && !direction_ended {
            self.receiver.io(&ChannelIoEvent::IoError {
                operation,
                error,
                read_bytes_so_far: self.read_bytes(),
                write_bytes_so_far: self.write_bytes(),
            });
        }
    }
    pub(super) fn detach(&mut self, reason: ChannelDetachReason) {
        if std::mem::replace(&mut self.detached, true) || (self.read_eof && self.write_shutdown) {
            return;
        }
        self.receiver.io(&ChannelIoEvent::Detached {
            reason,
            read_eof: self.read_eof,
            write_shutdown: self.write_shutdown,
            read_bytes_so_far: self.read_bytes(),
            write_bytes_so_far: self.write_bytes(),
        });
    }
}
impl<R: Receiver> Drop for Channel<R> {
    fn drop(&mut self) {
        self.detach(ChannelDetachReason::Dropped);
    }
}

pub(super) type SendChannel = Channel<Arc<dyn SendBodyObserver>>;
pub(super) type LocalChannel = Channel<Rc<dyn LocalBodyObserver>>;

/// Preserve the original io::Error allocation while borrowing it as aioduct::Error.
pub(super) fn io_error<T, R: Receiver>(
    channel: &Option<Box<Channel<R>>>,
    operation: ChannelIoOperation,
    error: std::io::Error,
) -> std::io::Result<T> {
    let Some(channel) = channel else {
        return Err(error);
    };
    let error = Error::Io(error);
    channel.error(operation, &error);
    let Error::Io(error) = error else {
        unreachable!("channel error wrapper changed")
    };
    Err(error)
}

#[cfg(test)]
#[path = "observation_tests.rs"]
mod tests;
