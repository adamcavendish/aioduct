//! Private Quinn adapter for the upstream `h3` transport traits.
//!
//! Portions are derived from `h3-quinn` 0.0.10. See the repository's
//! `THIRD_PARTY_LICENSES.md` for the applicable notice.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use bytes::{Buf, Bytes};
use futures_util::ready;
use futures_util::stream::{self, Stream, StreamExt as _};
use h3::error::Code;
use h3::quic::{self, ConnectionErrorIncoming, StreamErrorIncoming, StreamId, WriteBuf};
use quinn::{AcceptBi, AcceptUni, OpenBi, OpenUni, ReadError, VarInt};

type BoxStreamSync<'a, T> = Pin<Box<dyn Stream<Item = T> + Send + Sync + 'a>>;
type StopFuture = Pin<
    Box<dyn Future<Output = Result<Option<VarInt>, quinn::StoppedError>> + Send + Sync + 'static>,
>;

struct RequestStreamEntry {
    stopped: StopFuture,
    write_progress: WriteProgress,
}

#[derive(Clone, Default)]
pub(super) struct RequestStreamRegistry {
    entries: Arc<Mutex<HashMap<u64, RequestStreamEntry>>>,
}

impl RequestStreamRegistry {
    fn entries(&self) -> std::sync::MutexGuard<'_, HashMap<u64, RequestStreamEntry>> {
        self.entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn register(&self, stream: &quinn::SendStream) -> (RequestStreamRegistration, WriteProgress) {
        let id: u64 = stream.id().into();
        let write_progress = WriteProgress::default();
        let previous = self.entries().insert(
            id,
            RequestStreamEntry {
                stopped: Box::pin(stream.stopped()),
                write_progress: write_progress.clone(),
            },
        );
        debug_assert!(previous.is_none(), "duplicate HTTP/3 request stream id");
        (
            RequestStreamRegistration {
                id,
                registry: self.clone(),
            },
            write_progress,
        )
    }

    pub(super) fn take(&self, id: StreamId) -> Option<RequestStreamState> {
        self.entries()
            .remove(&id.into_inner())
            .map(|entry| RequestStreamState {
                stopped: entry.stopped,
                write_progress: entry.write_progress,
            })
    }

    fn remove(&self, id: u64) {
        self.entries().remove(&id);
    }
}

struct RequestStreamRegistration {
    id: u64,
    registry: RequestStreamRegistry,
}

impl Drop for RequestStreamRegistration {
    fn drop(&mut self) {
        self.registry.remove(self.id);
    }
}

#[derive(Clone, Default)]
pub(super) struct WriteProgress(Arc<AtomicU64>);

impl WriteProgress {
    fn record(&self, written: usize) {
        self.0.fetch_add(written as u64, Ordering::Release);
    }

    pub(super) fn load(&self) -> u64 {
        self.0.load(Ordering::Acquire)
    }
}

pub(super) struct RequestStreamState {
    stopped: StopFuture,
    write_progress: WriteProgress,
}

impl RequestStreamState {
    pub(super) fn write_progress(&self) -> WriteProgress {
        self.write_progress.clone()
    }
}

impl Future for RequestStreamState {
    type Output = Result<Option<VarInt>, quinn::StoppedError>;

    fn poll(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        self.stopped.as_mut().poll(context)
    }
}

pub(crate) struct Connection {
    connection: quinn::Connection,
    request_streams: RequestStreamRegistry,
    incoming_bidi: BoxStreamSync<'static, <AcceptBi<'static> as Future>::Output>,
    opening_bidi: Option<BoxStreamSync<'static, <OpenBi<'static> as Future>::Output>>,
    incoming_uni: BoxStreamSync<'static, <AcceptUni<'static> as Future>::Output>,
    opening_uni: Option<BoxStreamSync<'static, <OpenUni<'static> as Future>::Output>>,
}

impl Connection {
    pub(crate) fn new(connection: quinn::Connection) -> Self {
        Self {
            connection: connection.clone(),
            request_streams: RequestStreamRegistry::default(),
            incoming_bidi: Box::pin(stream::unfold(connection.clone(), |connection| async {
                Some((connection.accept_bi().await, connection))
            })),
            opening_bidi: None,
            incoming_uni: Box::pin(stream::unfold(connection.clone(), |connection| async {
                Some((connection.accept_uni().await, connection))
            })),
            opening_uni: None,
        }
    }

    pub(super) fn request_streams(&self) -> RequestStreamRegistry {
        self.request_streams.clone()
    }
}

impl<B: Buf> quic::Connection<B> for Connection {
    type RecvStream = RecvStream;
    type OpenStreams = OpenStreams;

    fn poll_accept_bidi(
        &mut self,
        context: &mut Context<'_>,
    ) -> Poll<Result<Self::BidiStream, ConnectionErrorIncoming>> {
        let Some(incoming) = ready!(self.incoming_bidi.poll_next_unpin(context)) else {
            return Poll::Ready(Err(ConnectionErrorIncoming::InternalError(
                "HTTP/3 bidirectional accept stream ended".to_owned(),
            )));
        };
        let (send, recv) = incoming.map_err(convert_connection_error)?;
        Poll::Ready(Ok(BidiStream {
            send: SendStream::new(send, false),
            recv: RecvStream::new(recv, false),
        }))
    }

    fn poll_accept_recv(
        &mut self,
        context: &mut Context<'_>,
    ) -> Poll<Result<Self::RecvStream, ConnectionErrorIncoming>> {
        let Some(incoming) = ready!(self.incoming_uni.poll_next_unpin(context)) else {
            return Poll::Ready(Err(ConnectionErrorIncoming::InternalError(
                "HTTP/3 unidirectional accept stream ended".to_owned(),
            )));
        };
        let recv = incoming.map_err(convert_connection_error)?;
        Poll::Ready(Ok(RecvStream::new(recv, false)))
    }

    fn opener(&self) -> Self::OpenStreams {
        OpenStreams {
            connection: self.connection.clone(),
            request_streams: self.request_streams.clone(),
            opening_bidi: None,
            opening_uni: None,
        }
    }
}

impl<B: Buf> quic::OpenStreams<B> for Connection {
    type SendStream = SendStream<B>;
    type BidiStream = BidiStream<B>;

    fn poll_open_bidi(
        &mut self,
        context: &mut Context<'_>,
    ) -> Poll<Result<Self::BidiStream, StreamErrorIncoming>> {
        poll_open_bidi(
            &self.connection,
            &self.request_streams,
            &mut self.opening_bidi,
            context,
        )
    }

    fn poll_open_send(
        &mut self,
        context: &mut Context<'_>,
    ) -> Poll<Result<Self::SendStream, StreamErrorIncoming>> {
        poll_open_send(&self.connection, &mut self.opening_uni, context)
    }

    fn close(&mut self, code: Code, reason: &[u8]) {
        self.connection.close(quinn_varint(code.value()), reason);
    }
}

pub(crate) struct OpenStreams {
    connection: quinn::Connection,
    request_streams: RequestStreamRegistry,
    opening_bidi: Option<BoxStreamSync<'static, <OpenBi<'static> as Future>::Output>>,
    opening_uni: Option<BoxStreamSync<'static, <OpenUni<'static> as Future>::Output>>,
}

impl<B: Buf> quic::OpenStreams<B> for OpenStreams {
    type SendStream = SendStream<B>;
    type BidiStream = BidiStream<B>;

    fn poll_open_bidi(
        &mut self,
        context: &mut Context<'_>,
    ) -> Poll<Result<Self::BidiStream, StreamErrorIncoming>> {
        poll_open_bidi(
            &self.connection,
            &self.request_streams,
            &mut self.opening_bidi,
            context,
        )
    }

    fn poll_open_send(
        &mut self,
        context: &mut Context<'_>,
    ) -> Poll<Result<Self::SendStream, StreamErrorIncoming>> {
        poll_open_send(&self.connection, &mut self.opening_uni, context)
    }

    fn close(&mut self, code: Code, reason: &[u8]) {
        self.connection.close(quinn_varint(code.value()), reason);
    }
}

impl Clone for OpenStreams {
    fn clone(&self) -> Self {
        Self {
            connection: self.connection.clone(),
            request_streams: self.request_streams.clone(),
            opening_bidi: None,
            opening_uni: None,
        }
    }
}

fn poll_open_bidi<B: Buf>(
    connection: &quinn::Connection,
    request_streams: &RequestStreamRegistry,
    opening: &mut Option<BoxStreamSync<'static, <OpenBi<'static> as Future>::Output>>,
    context: &mut Context<'_>,
) -> Poll<Result<BidiStream<B>, StreamErrorIncoming>> {
    let streams = opening.get_or_insert_with(|| {
        Box::pin(stream::unfold(connection.clone(), |connection| async {
            Some((connection.open_bi().await, connection))
        }))
    });
    let Some(opened) = ready!(streams.poll_next_unpin(context)) else {
        return Poll::Ready(Err(StreamErrorIncoming::ConnectionErrorIncoming {
            connection_error: ConnectionErrorIncoming::InternalError(
                "HTTP/3 bidirectional open stream ended".to_owned(),
            ),
        }));
    };
    let (send, recv) = opened.map_err(|error| StreamErrorIncoming::ConnectionErrorIncoming {
        connection_error: convert_connection_error(error),
    })?;
    Poll::Ready(Ok(BidiStream {
        send: SendStream::new_request(send, request_streams),
        recv: RecvStream::new(recv, true),
    }))
}

fn poll_open_send<B: Buf>(
    connection: &quinn::Connection,
    opening: &mut Option<BoxStreamSync<'static, <OpenUni<'static> as Future>::Output>>,
    context: &mut Context<'_>,
) -> Poll<Result<SendStream<B>, StreamErrorIncoming>> {
    let streams = opening.get_or_insert_with(|| {
        Box::pin(stream::unfold(connection.clone(), |connection| async {
            Some((connection.open_uni().await, connection))
        }))
    });
    let Some(opened) = ready!(streams.poll_next_unpin(context)) else {
        return Poll::Ready(Err(StreamErrorIncoming::ConnectionErrorIncoming {
            connection_error: ConnectionErrorIncoming::InternalError(
                "HTTP/3 unidirectional open stream ended".to_owned(),
            ),
        }));
    };
    let send = opened.map_err(|error| StreamErrorIncoming::ConnectionErrorIncoming {
        connection_error: convert_connection_error(error),
    })?;
    Poll::Ready(Ok(SendStream::new(send, false)))
}

pub(crate) struct BidiStream<B: Buf> {
    send: SendStream<B>,
    recv: RecvStream,
}

impl<B: Buf> quic::BidiStream<B> for BidiStream<B> {
    type SendStream = SendStream<B>;
    type RecvStream = RecvStream;

    fn split(self) -> (Self::SendStream, Self::RecvStream) {
        (self.send, self.recv)
    }
}

impl<B: Buf> quic::RecvStream for BidiStream<B> {
    type Buf = Bytes;

    fn poll_data(
        &mut self,
        context: &mut Context<'_>,
    ) -> Poll<Result<Option<Self::Buf>, StreamErrorIncoming>> {
        self.recv.poll_data(context)
    }

    fn stop_sending(&mut self, error_code: u64) {
        self.recv.stop_sending(error_code);
    }

    fn recv_id(&self) -> StreamId {
        self.recv.recv_id()
    }
}

impl<B: Buf> quic::SendStream<B> for BidiStream<B> {
    fn poll_ready(&mut self, context: &mut Context<'_>) -> Poll<Result<(), StreamErrorIncoming>> {
        self.send.poll_ready(context)
    }

    fn poll_finish(&mut self, context: &mut Context<'_>) -> Poll<Result<(), StreamErrorIncoming>> {
        self.send.poll_finish(context)
    }

    fn reset(&mut self, reset_code: u64) {
        self.send.reset(reset_code);
    }

    fn send_data<D: Into<WriteBuf<B>>>(&mut self, data: D) -> Result<(), StreamErrorIncoming> {
        self.send.send_data(data)
    }

    fn send_id(&self) -> StreamId {
        self.send.send_id()
    }
}

impl<B: Buf> quic::SendStreamUnframed<B> for BidiStream<B> {
    fn poll_send<D: Buf>(
        &mut self,
        context: &mut Context<'_>,
        buffer: &mut D,
    ) -> Poll<Result<usize, StreamErrorIncoming>> {
        self.send.poll_send(context, buffer)
    }
}

pub(crate) struct RecvStream {
    stream: quinn::RecvStream,
    cancel_on_drop: bool,
    closed: bool,
}

impl RecvStream {
    fn new(stream: quinn::RecvStream, cancel_on_drop: bool) -> Self {
        Self {
            stream,
            cancel_on_drop,
            closed: false,
        }
    }
}

impl Drop for RecvStream {
    fn drop(&mut self) {
        if self.cancel_on_drop && !self.closed {
            let _ = self
                .stream
                .stop(quinn_varint(Code::H3_REQUEST_CANCELLED.value()));
        }
    }
}

impl quic::RecvStream for RecvStream {
    type Buf = Bytes;

    fn poll_data(
        &mut self,
        context: &mut Context<'_>,
    ) -> Poll<Result<Option<Self::Buf>, StreamErrorIncoming>> {
        // Quinn documents `read_chunk` as cancellation-safe. Polling a fresh
        // borrowing future keeps the stream available for synchronous
        // STOP_SENDING when the response body is dropped.
        let read = self.stream.read_chunk(usize::MAX, true);
        let mut read = std::pin::pin!(read);
        match ready!(read.as_mut().poll(context)) {
            Ok(Some(chunk)) => Poll::Ready(Ok(Some(chunk.bytes))),
            Ok(None) => {
                self.closed = true;
                Poll::Ready(Ok(None))
            }
            Err(error) => {
                self.closed = true;
                Poll::Ready(Err(convert_read_error(error)))
            }
        }
    }

    fn stop_sending(&mut self, error_code: u64) {
        if self.closed {
            return;
        }
        self.closed = true;
        let _ = self.stream.stop(quinn_varint(error_code));
    }

    fn recv_id(&self) -> StreamId {
        h3_stream_id(self.stream.id())
    }
}

pub(crate) struct SendStream<B: Buf> {
    stream: quinn::SendStream,
    writing: Option<WriteBuf<B>>,
    write_progress: Option<WriteProgress>,
    cancel_on_drop: bool,
    closed: bool,
    _registration: Option<RequestStreamRegistration>,
}

impl<B: Buf> SendStream<B> {
    fn new(stream: quinn::SendStream, cancel_on_drop: bool) -> Self {
        Self {
            stream,
            writing: None,
            write_progress: None,
            cancel_on_drop,
            closed: false,
            _registration: None,
        }
    }

    fn new_request(stream: quinn::SendStream, registry: &RequestStreamRegistry) -> Self {
        let (registration, write_progress) = registry.register(&stream);
        Self {
            stream,
            writing: None,
            write_progress: Some(write_progress),
            cancel_on_drop: true,
            closed: false,
            _registration: Some(registration),
        }
    }

    fn record_write(&self, written: usize) {
        if let Some(progress) = &self.write_progress {
            progress.record(written);
        }
    }
}

impl<B: Buf> Drop for SendStream<B> {
    fn drop(&mut self) {
        if self.cancel_on_drop && !self.closed {
            let _ = self
                .stream
                .reset(quinn_varint(Code::H3_REQUEST_CANCELLED.value()));
        }
    }
}

impl<B: Buf> quic::SendStream<B> for SendStream<B> {
    fn poll_ready(&mut self, context: &mut Context<'_>) -> Poll<Result<(), StreamErrorIncoming>> {
        let write_progress = self.write_progress.clone();
        if let Some(data) = self.writing.as_mut() {
            while data.has_remaining() {
                let written = ready!(Pin::new(&mut self.stream).poll_write(context, data.chunk()))
                    .map_err(convert_write_error)?;
                if written == 0 {
                    return Poll::Ready(Err(write_zero()));
                }
                data.advance(written);
                if let Some(progress) = &write_progress {
                    progress.record(written);
                }
            }
        }
        self.writing = None;
        Poll::Ready(Ok(()))
    }

    fn poll_finish(&mut self, context: &mut Context<'_>) -> Poll<Result<(), StreamErrorIncoming>> {
        ready!(quic::SendStream::poll_ready(self, context))?;
        let result = self
            .stream
            .finish()
            .map_err(|error| StreamErrorIncoming::Unknown(Box::new(error)));
        if result.is_ok() {
            self.closed = true;
        }
        Poll::Ready(result)
    }

    fn reset(&mut self, reset_code: u64) {
        if self.closed {
            return;
        }
        self.closed = true;
        let _ = self.stream.reset(quinn_varint(reset_code));
    }

    fn send_data<D: Into<WriteBuf<B>>>(&mut self, data: D) -> Result<(), StreamErrorIncoming> {
        if self.writing.is_some() {
            return Err(StreamErrorIncoming::ConnectionErrorIncoming {
                connection_error: ConnectionErrorIncoming::InternalError(
                    "HTTP/3 send_data called before the stream became ready".to_owned(),
                ),
            });
        }
        self.writing = Some(data.into());
        Ok(())
    }

    fn send_id(&self) -> StreamId {
        h3_stream_id(self.stream.id())
    }
}

impl<B: Buf> quic::SendStreamUnframed<B> for SendStream<B> {
    fn poll_send<D: Buf>(
        &mut self,
        context: &mut Context<'_>,
        buffer: &mut D,
    ) -> Poll<Result<usize, StreamErrorIncoming>> {
        if self.writing.is_some() {
            return Poll::Ready(Err(StreamErrorIncoming::ConnectionErrorIncoming {
                connection_error: ConnectionErrorIncoming::InternalError(
                    "HTTP/3 unframed write started before the stream became ready".to_owned(),
                ),
            }));
        }
        match ready!(Pin::new(&mut self.stream).poll_write(context, buffer.chunk())) {
            Ok(0) if buffer.has_remaining() => Poll::Ready(Err(write_zero())),
            Ok(written) => {
                buffer.advance(written);
                self.record_write(written);
                Poll::Ready(Ok(written))
            }
            Err(error) => Poll::Ready(Err(convert_write_error(error))),
        }
    }
}

fn convert_connection_error(error: quinn::ConnectionError) -> ConnectionErrorIncoming {
    match error {
        quinn::ConnectionError::ApplicationClosed(close) => {
            ConnectionErrorIncoming::ApplicationClose {
                error_code: close.error_code.into(),
            }
        }
        quinn::ConnectionError::TimedOut => ConnectionErrorIncoming::Timeout,
        error => ConnectionErrorIncoming::Undefined(Arc::new(error)),
    }
}

fn convert_read_error(error: ReadError) -> StreamErrorIncoming {
    match error {
        ReadError::Reset(code) => StreamErrorIncoming::StreamTerminated {
            error_code: code.into_inner(),
        },
        ReadError::ConnectionLost(error) => StreamErrorIncoming::ConnectionErrorIncoming {
            connection_error: convert_connection_error(error),
        },
        ReadError::IllegalOrderedRead => StreamErrorIncoming::Unknown(Box::new(
            std::io::Error::other("HTTP/3 adapter encountered an illegal ordered read"),
        )),
        error => StreamErrorIncoming::Unknown(Box::new(error)),
    }
}

fn convert_write_error(error: quinn::WriteError) -> StreamErrorIncoming {
    match error {
        quinn::WriteError::Stopped(code) => StreamErrorIncoming::StreamTerminated {
            error_code: code.into_inner(),
        },
        quinn::WriteError::ConnectionLost(error) => StreamErrorIncoming::ConnectionErrorIncoming {
            connection_error: convert_connection_error(error),
        },
        error => StreamErrorIncoming::Unknown(Box::new(error)),
    }
}

fn quinn_varint(value: u64) -> VarInt {
    VarInt::from_u64(value).unwrap_or(VarInt::MAX)
}

#[allow(clippy::expect_used)]
fn h3_stream_id(id: quinn::StreamId) -> StreamId {
    let id: u64 = id.into();
    id.try_into()
        .expect("Quinn stream IDs are always valid h3 stream IDs")
}

fn write_zero() -> StreamErrorIncoming {
    StreamErrorIncoming::Unknown(Box::new(std::io::Error::new(
        std::io::ErrorKind::WriteZero,
        "HTTP/3 transport accepted no request bytes",
    )))
}
