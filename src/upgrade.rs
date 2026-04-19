use crate::error::{Error, Result};

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
    response: &mut http::Response<crate::error::HyperBody>,
) -> Result<Upgraded> {
    let on_upgrade = hyper::upgrade::on(response);
    let upgraded = on_upgrade.await.map_err(|e| Error::Other(Box::new(e)))?;
    Ok(Upgraded::new(upgraded))
}

#[cfg(test)]
mod tests {
    #[test]
    fn debug_format() {
        let dbg_str = format!("{:?}", "Upgraded");
        assert!(dbg_str.contains("Upgraded"));
    }
}
