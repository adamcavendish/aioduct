use crate::error::Error;

/// A bidirectional IO stream from an HTTP upgrade (e.g., WebSocket).
///
/// Obtained by calling [`Response::upgrade()`](crate::Response::upgrade) after
/// receiving a `101 Switching Protocols` response. Implements hyper's `Read` and
/// `Write` traits for use with WebSocket libraries.
pub struct Upgraded {
    inner: hyper::upgrade::Upgraded,
}

impl Upgraded {
    pub(crate) fn new(inner: hyper::upgrade::Upgraded) -> Self {
        Self { inner }
    }

    /// Consume the upgraded connection, returning the underlying hyper `Upgraded`.
    pub fn into_inner(self) -> hyper::upgrade::Upgraded {
        self.inner
    }
}

impl From<hyper::upgrade::Upgraded> for Upgraded {
    fn from(inner: hyper::upgrade::Upgraded) -> Self {
        Self::new(inner)
    }
}

impl hyper::rt::Read for Upgraded {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: hyper::rt::ReadBufCursor<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

impl hyper::rt::Write for Upgraded {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        std::pin::Pin::new(&mut self.inner).poll_write(cx, buf)
    }

    fn poll_flush(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

impl std::fmt::Debug for Upgraded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Upgraded").finish()
    }
}

#[cfg(feature = "tokio")]
impl tokio::io::AsyncRead for Upgraded {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        let n = unsafe {
            let mut hbuf = hyper::rt::ReadBuf::uninit(buf.unfilled_mut());
            match hyper::rt::Read::poll_read(
                std::pin::Pin::new(&mut self.inner),
                cx,
                hbuf.unfilled(),
            ) {
                std::task::Poll::Ready(Ok(())) => hbuf.filled().len(),
                std::task::Poll::Ready(Err(e)) => return std::task::Poll::Ready(Err(e)),
                std::task::Poll::Pending => return std::task::Poll::Pending,
            }
        };
        buf.advance(n);
        std::task::Poll::Ready(Ok(()))
    }
}

#[cfg(feature = "tokio")]
impl tokio::io::AsyncWrite for Upgraded {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        hyper::rt::Write::poll_write(std::pin::Pin::new(&mut self.inner), cx, buf)
    }

    fn poll_flush(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        hyper::rt::Write::poll_flush(std::pin::Pin::new(&mut self.inner), cx)
    }

    fn poll_shutdown(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        hyper::rt::Write::poll_shutdown(std::pin::Pin::new(&mut self.inner), cx)
    }
}

pub(crate) async fn on_upgrade(
    response: &mut http::Response<crate::response::ResponseBoxSendBody>,
) -> Result<Upgraded, Error> {
    let on_upgrade = hyper::upgrade::on(response);
    let upgraded = on_upgrade.await.map_err(|e| Error::Other(Box::new(e)))?;
    Ok(Upgraded::new(upgraded))
}

pub(crate) async fn on_upgrade_local(
    response: &mut http::Response<crate::body::ResponseBoxLocalBody>,
) -> Result<Upgraded, Error> {
    let on_upgrade = hyper::upgrade::on(response);
    let upgraded = on_upgrade.await.map_err(|e| Error::Other(Box::new(e)))?;
    Ok(Upgraded::new(upgraded))
}

#[cfg(all(test, feature = "tokio"))]
mod tests {
    use super::*;
    use crate::runtime::tokio_rt::TokioIo;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    async fn upgraded_from_handshake() -> (Upgraded, tokio::io::DuplexStream) {
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
        (Upgraded::new(hyper_upgraded), server)
    }

    #[tokio::test]
    async fn debug_format() {
        let (upgraded, _server) = upgraded_from_handshake().await;
        let dbg = format!("{upgraded:?}");
        assert!(dbg.contains("Upgraded"));
    }

    #[tokio::test]
    async fn into_inner_returns_hyper_type() {
        let (upgraded, _server) = upgraded_from_handshake().await;
        let _inner: hyper::upgrade::Upgraded = upgraded.into_inner();
    }

    #[tokio::test]
    async fn from_trait_impl() {
        let (upgraded, _server) = upgraded_from_handshake().await;
        let inner = upgraded.into_inner();
        let _back: Upgraded = Upgraded::from(inner);
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
}
