use std::task::Poll;

use crate::error::Error;

/// A bidirectional IO stream from an HTTP upgrade on a `Send` runtime.
///
/// Obtained by calling [`Response::upgrade()`](crate::Response::upgrade) after
/// receiving a `101 Switching Protocols` response. Implements hyper's `Read` and
/// `Write` traits for use with WebSocket libraries.
pub struct UpgradedSend {
    inner: hyper::upgrade::Upgraded,
    _active_stream_permit: Option<crate::pool::ActiveStreamPermit>,
}

impl UpgradedSend {
    pub(crate) fn new(inner: hyper::upgrade::Upgraded) -> Self {
        Self {
            inner,
            _active_stream_permit: None,
        }
    }

    /// Consume the upgraded connection, returning the underlying
    /// `hyper::upgrade::Upgraded`.
    pub fn into_inner(self) -> hyper::upgrade::Upgraded {
        self.inner
    }
}

impl From<hyper::upgrade::Upgraded> for UpgradedSend {
    fn from(inner: hyper::upgrade::Upgraded) -> Self {
        Self::new(inner)
    }
}

impl hyper::rt::Read for UpgradedSend {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: hyper::rt::ReadBufCursor<'_>,
    ) -> Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

impl hyper::rt::Write for UpgradedSend {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        std::pin::Pin::new(&mut self.inner).poll_write(cx, buf)
    }

    fn poll_flush(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

impl std::fmt::Debug for UpgradedSend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UpgradedSend").finish()
    }
}

#[cfg(feature = "tokio")]
impl tokio::io::AsyncRead for UpgradedSend {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let n = unsafe {
            let mut hbuf = hyper::rt::ReadBuf::uninit(buf.unfilled_mut());
            match hyper::rt::Read::poll_read(
                std::pin::Pin::new(&mut self.inner),
                cx,
                hbuf.unfilled(),
            ) {
                Poll::Ready(Ok(())) => hbuf.filled().len(),
                Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                Poll::Pending => return Poll::Pending,
            }
        };
        buf.advance(n);
        Poll::Ready(Ok(()))
    }
}

#[cfg(feature = "tokio")]
impl tokio::io::AsyncWrite for UpgradedSend {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        hyper::rt::Write::poll_write(std::pin::Pin::new(&mut self.inner), cx, buf)
    }

    fn poll_flush(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<std::io::Result<()>> {
        hyper::rt::Write::poll_flush(std::pin::Pin::new(&mut self.inner), cx)
    }

    fn poll_shutdown(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<std::io::Result<()>> {
        hyper::rt::Write::poll_shutdown(std::pin::Pin::new(&mut self.inner), cx)
    }
}

pub(crate) async fn on_upgrade(
    response: &mut http::Response<crate::response::ResponseBodySend>,
    active_stream_permit: Option<crate::pool::ActiveStreamPermit>,
) -> Result<UpgradedSend, Error> {
    let on_upgrade = hyper::upgrade::on(response);
    let upgraded = on_upgrade.await.map_err(|e| Error::Other(Box::new(e)))?;
    Ok(UpgradedSend {
        inner: upgraded,
        _active_stream_permit: active_stream_permit,
    })
}

#[cfg(all(test, feature = "tokio"))]
mod tests {
    use super::*;
    use crate::runtime::tokio_rt::TokioIo;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    async fn upgraded_from_handshake() -> (UpgradedSend, tokio::io::DuplexStream) {
        let (client_io, server_io) = tokio::io::duplex(1024);
        let io = TokioIo::new(client_io);

        let (mut sender, conn) =
            hyper::client::conn::http1::handshake::<_, http_body_util::Empty<bytes::Bytes>>(io)
                .await
                .unwrap();

        tokio::spawn(async move {
            let _ = conn.with_upgrades().await;
        });

        let server_handle = tokio::spawn(async move {
            let mut server = server_io;
            let mut buf = [0u8; 4096];
            let _ = AsyncReadExt::read(&mut server, &mut buf).await;
            let resp =
                b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: raw\r\nConnection: Upgrade\r\n\r\n";
            AsyncWriteExt::write_all(&mut server, resp).await.unwrap();
            server
        });

        let req = http::Request::builder()
            .uri("http://localhost/up")
            .header("connection", "upgrade")
            .header("upgrade", "raw")
            .body(http_body_util::Empty::<bytes::Bytes>::new())
            .unwrap();

        let resp = sender.send_request(req).await.unwrap();
        assert_eq!(resp.status(), http::StatusCode::SWITCHING_PROTOCOLS);

        let hyper_upgraded = hyper::upgrade::on(resp).await.unwrap();
        let server = server_handle.await.unwrap();
        (UpgradedSend::new(hyper_upgraded), server)
    }

    #[tokio::test]
    async fn debug_format() {
        let (upgraded, _server) = upgraded_from_handshake().await;
        let dbg = format!("{upgraded:?}");
        assert!(dbg.contains("UpgradedSend"));
    }

    #[tokio::test]
    async fn async_read_write_round_trip() {
        let (mut upgraded, mut server) = upgraded_from_handshake().await;

        upgraded.write_all(b"ping").await.unwrap();
        upgraded.flush().await.unwrap();

        let mut buf = [0u8; 4];
        server.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"ping");

        server.write_all(b"pong").await.unwrap();
        server.flush().await.unwrap();

        let mut buf = [0u8; 4];
        upgraded.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"pong");
    }

    #[tokio::test]
    async fn shutdown_closes_write_side() {
        let (mut upgraded, mut server) = upgraded_from_handshake().await;
        upgraded.shutdown().await.unwrap();
        let mut buf = [0u8; 1];
        let n = server.read(&mut buf).await.unwrap();
        assert_eq!(n, 0);
    }

    #[tokio::test]
    async fn hyper_read_write_round_trip() {
        use std::future::poll_fn;
        use std::pin::Pin;

        let (mut upgraded, mut server) = upgraded_from_handshake().await;

        let data = b"hyper-test";
        let n = poll_fn(|cx| hyper::rt::Write::poll_write(Pin::new(&mut upgraded), cx, data))
            .await
            .unwrap();
        assert_eq!(n, data.len());

        poll_fn(|cx| hyper::rt::Write::poll_flush(Pin::new(&mut upgraded), cx))
            .await
            .unwrap();

        let mut buf = [0u8; 10];
        server.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, data);

        server.write_all(b"back").await.unwrap();
        server.flush().await.unwrap();

        let mut read_buf = vec![0u8; 4];
        poll_fn(|cx| {
            let mut hbuf = hyper::rt::ReadBuf::new(&mut read_buf);
            match hyper::rt::Read::poll_read(Pin::new(&mut upgraded), cx, hbuf.unfilled()) {
                std::task::Poll::Ready(Ok(())) => std::task::Poll::Ready(Ok(hbuf.filled().len())),
                std::task::Poll::Ready(Err(e)) => std::task::Poll::Ready(Err(e)),
                std::task::Poll::Pending => std::task::Poll::Pending,
            }
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn hyper_write_shutdown() {
        use std::future::poll_fn;
        use std::pin::Pin;

        let (mut upgraded, mut server) = upgraded_from_handshake().await;
        poll_fn(|cx| hyper::rt::Write::poll_shutdown(Pin::new(&mut upgraded), cx))
            .await
            .unwrap();
        let mut buf = [0u8; 1];
        let n = server.read(&mut buf).await.unwrap();
        assert_eq!(n, 0);
    }

    #[tokio::test]
    async fn tokio_async_read_returns_data_correctly() {
        let (mut upgraded, mut server) = upgraded_from_handshake().await;

        server.write_all(b"test-data").await.unwrap();
        server.flush().await.unwrap();

        let mut buf = vec![0u8; 64];
        let n = upgraded.read(&mut buf).await.unwrap();
        assert!(n > 0, "should read some bytes");
        assert_eq!(&buf[..n], b"test-data");
    }

    #[tokio::test]
    async fn tokio_async_read_eof_after_server_close() {
        let (mut upgraded, server) = upgraded_from_handshake().await;

        drop(server);

        let mut buf = vec![0u8; 64];
        let n = upgraded.read(&mut buf).await.unwrap();
        assert_eq!(n, 0, "should get EOF after server closes");
    }
}
