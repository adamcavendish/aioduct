use std::pin::Pin;
use std::task::{Context, Poll};

use hyper::rt::{Read, ReadBufCursor, Write};

trait SendIo: Read + Write + Send + Unpin + 'static {}
impl<T> SendIo for T where T: Read + Write + Send + Unpin + 'static {}

pub(super) struct ProxyStreamSend(Box<dyn SendIo>);

impl ProxyStreamSend {
    pub(super) fn new<T>(stream: T) -> Self
    where
        T: Read + Write + Send + Unpin + 'static,
    {
        Self(Box::new(stream))
    }
}

impl Read for ProxyStreamSend {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: ReadBufCursor<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut *self.0).poll_read(cx, buf)
    }
}

impl Write for ProxyStreamSend {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut *self.0).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut *self.0).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut *self.0).poll_shutdown(cx)
    }
}

trait LocalIo: Read + Write + Unpin + 'static {}
impl<T> LocalIo for T where T: Read + Write + Unpin + 'static {}

pub(super) struct ProxyStreamLocal(Box<dyn LocalIo>);

impl ProxyStreamLocal {
    pub(super) fn new<T>(stream: T) -> Self
    where
        T: Read + Write + Unpin + 'static,
    {
        Self(Box::new(stream))
    }
}

impl Read for ProxyStreamLocal {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: ReadBufCursor<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut *self.0).poll_read(cx, buf)
    }
}

impl Write for ProxyStreamLocal {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut *self.0).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut *self.0).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut *self.0).poll_shutdown(cx)
    }
}
