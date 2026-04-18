use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use hyper::rt::{self, Read, Write};
use pin_project_lite::pin_project;

use super::Runtime;

pub struct CompioRuntime;

impl Runtime for CompioRuntime {
    // TODO: compio uses a completion-based I/O model that differs from readiness-based.
    // The TcpStream here needs a compatibility shim between compio's completion model
    // and hyper's readiness-based Read/Write traits. This is a placeholder.
    type TcpStream = CompioIo;
    type Sleep = CompioSleep;

    async fn connect(addr: SocketAddr) -> io::Result<Self::TcpStream> {
        let _stream = compio_net::TcpStream::connect(addr).await?;
        todo!("compio TcpStream -> CompioIo adapter requires completion-to-readiness bridge")
    }

    async fn resolve(_host: &str, _port: u16) -> io::Result<SocketAddr> {
        todo!("compio DNS resolution")
    }

    fn sleep(duration: Duration) -> Self::Sleep {
        CompioSleep { duration }
    }

    fn spawn<F>(future: F)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        compio_runtime::spawn(future).detach();
    }
}

// -- CompioSleep --

pub struct CompioSleep {
    duration: Duration,
}

impl Future for CompioSleep {
    type Output = ();

    fn poll(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Self::Output> {
        // TODO: integrate with compio's timer
        todo!("compio sleep integration")
    }
}

// -- CompioIo: placeholder for completion-to-readiness bridge --

pub struct CompioIo {
    _priv: (),
}

impl Read for CompioIo {
    fn poll_read(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        _buf: rt::ReadBufCursor<'_>,
    ) -> Poll<io::Result<()>> {
        todo!("compio completion-to-readiness Read bridge")
    }
}

impl Write for CompioIo {
    fn poll_write(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        _buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        todo!("compio completion-to-readiness Write bridge")
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        todo!("compio completion-to-readiness flush bridge")
    }

    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        todo!("compio completion-to-readiness shutdown bridge")
    }
}
