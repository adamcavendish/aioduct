use std::pin::Pin;

use crate::error::Error;
use crate::proxy::ProxyConfig;

/// Perform an HTTP CONNECT handshake through `stream` to `target`.
///
/// Sends `CONNECT target HTTP/1.1`, reads the response, validates HTTP 200,
/// and returns the stream unchanged on success. Type-preserving: the returned
/// stream is the same `S` that was passed in, making it reusable for proxy
/// chaining.
///
/// Works for both Send and Local paths — only requires `Read + Write + Unpin`.
pub(crate) async fn do_connect_handshake<S>(
    mut stream: S,
    proxy: &ProxyConfig,
    target: &str,
) -> Result<S, Error>
where
    S: hyper::rt::Read + hyper::rt::Write + Unpin + 'static,
{
    let mut connect_msg = format!("CONNECT {target} HTTP/1.1\r\nHost: {target}\r\n");
    if let Some(auth_value) = proxy.connect_header(target) {
        connect_msg.push_str(&format!("Proxy-Authorization: {auth_value}\r\n"));
    }
    connect_msg.push_str("\r\n");

    let buf = connect_msg.into_bytes();
    let mut written = 0;
    while written < buf.len() {
        let n = std::future::poll_fn(|cx| Pin::new(&mut stream).poll_write(cx, &buf[written..]))
            .await
            .map_err(Error::Io)?;
        if n == 0 {
            return Err(Error::Io(std::io::Error::new(
                std::io::ErrorKind::WriteZero,
                "proxy closed connection during CONNECT handshake",
            )));
        }
        written += n;
    }

    // Flush after write: completion-based runtimes may buffer writes
    // internally; poll_flush ensures bytes reach the proxy before we
    // start reading the CONNECT response.
    std::future::poll_fn(|cx| Pin::new(&mut stream).poll_flush(cx))
        .await
        .map_err(Error::Io)?;

    let mut resp_buf = Vec::with_capacity(256);
    loop {
        let mut one = [0u8; 1];
        let mut read_buf = hyper::rt::ReadBuf::new(&mut one);
        std::future::poll_fn(|cx| Pin::new(&mut stream).poll_read(cx, read_buf.unfilled()))
            .await
            .map_err(Error::Io)?;

        if read_buf.filled().is_empty() {
            return Err(Error::Other("proxy closed connection".into()));
        }
        resp_buf.push(one[0]);

        if resp_buf.len() >= 4 && resp_buf[resp_buf.len() - 4..] == *b"\r\n\r\n" {
            break;
        }

        if resp_buf.len() > 8192 {
            return Err(Error::Other("CONNECT response too large".into()));
        }
    }

    let resp_str = String::from_utf8_lossy(&resp_buf);
    let status_line = resp_str
        .lines()
        .next()
        .ok_or_else(|| Error::Other("empty CONNECT response".into()))?;

    let status_code = parse_connect_status(status_line)?;
    if status_code != 200 {
        return Err(Error::Other(
            format!("CONNECT tunnel failed: {status_line}").into(),
        ));
    }

    Ok(stream)
}

pub(crate) fn parse_connect_status(status_line: &str) -> Result<u16, Error> {
    status_line
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse::<u16>().ok())
        .ok_or_else(|| Error::Other(format!("malformed CONNECT status line: {status_line}").into()))
}

#[cfg(test)]
mod tests {
    use super::parse_connect_status;

    #[test]
    fn parse_200_ok() {
        assert_eq!(parse_connect_status("HTTP/1.1 200 OK").unwrap(), 200);
    }

    #[test]
    fn parse_200_connection_established() {
        assert_eq!(
            parse_connect_status("HTTP/1.1 200 Connection Established").unwrap(),
            200
        );
    }

    #[test]
    fn parse_407_proxy_auth_required() {
        assert_eq!(
            parse_connect_status("HTTP/1.1 407 Proxy Authentication Required").unwrap(),
            407
        );
    }

    #[test]
    fn parse_403_forbidden() {
        assert_eq!(parse_connect_status("HTTP/1.1 403 Forbidden").unwrap(), 403);
    }

    #[test]
    fn malformed_status_line_returns_error() {
        assert!(parse_connect_status("garbage").is_err());
    }

    #[test]
    fn empty_status_line_returns_error() {
        assert!(parse_connect_status("").is_err());
    }

    #[test]
    fn status_with_200_in_reason_is_not_200() {
        assert_eq!(
            parse_connect_status("HTTP/1.1 403 Contains 200 in text").unwrap(),
            403
        );
    }

    #[test]
    fn parse_non_numeric_status_code_returns_error() {
        assert!(parse_connect_status("HTTP/1.1 abc Forbidden").is_err());
    }

    #[test]
    fn parse_no_second_token_returns_error() {
        assert!(parse_connect_status("HTTP/1.1").is_err());
    }

    #[test]
    fn parse_301_redirect() {
        assert_eq!(
            parse_connect_status("HTTP/1.1 301 Moved Permanently").unwrap(),
            301
        );
    }

    #[test]
    fn parse_503_service_unavailable() {
        assert_eq!(
            parse_connect_status("HTTP/1.1 503 Service Unavailable").unwrap(),
            503
        );
    }
}

#[cfg(all(test, feature = "tokio"))]
mod tokio_tests {
    use std::cell::Cell;
    use std::pin::Pin;
    use std::task::{Context, Poll};

    use super::*;

    // ── do_connect_handshake integration tests ──────────────────────────────

    #[tokio::test]
    async fn do_connect_handshake_succeeds_with_200() {
        let (client_io, mut server_io) = tokio::io::duplex(8192);
        let target = "target.example.com:443".to_string();

        tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let mut buf = [0u8; 4096];
            let n = server_io.read(&mut buf).await.unwrap();
            let req = String::from_utf8_lossy(&buf[..n]);
            assert!(
                req.starts_with("CONNECT target.example.com:443"),
                "got: {req}"
            );
            assert!(req.contains("Host: target.example.com:443"), "got: {req}");
            server_io
                .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                .await
                .unwrap();
        });

        let proxy = crate::proxy::ProxyConfig::http("http://proxy:8080").unwrap();
        let stream = crate::runtime::tokio_rt::TokioIo::new(client_io);
        let result = do_connect_handshake(stream, &proxy, &target).await;
        assert!(result.is_ok(), "handshake should succeed");
    }

    #[tokio::test]
    async fn do_connect_handshake_fails_on_407() {
        let (client_io, mut server_io) = tokio::io::duplex(8192);
        let target = "target.example.com:443".to_string();

        tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let mut buf = [0u8; 4096];
            let _ = server_io.read(&mut buf).await.unwrap();
            server_io
                .write_all(
                    b"HTTP/1.1 407 Proxy Authentication Required\r\nContent-Length: 0\r\n\r\n",
                )
                .await
                .unwrap();
        });

        let proxy = crate::proxy::ProxyConfig::http("http://proxy:8080").unwrap();
        let stream = crate::runtime::tokio_rt::TokioIo::new(client_io);
        let result = do_connect_handshake(stream, &proxy, &target).await;
        assert!(result.is_err());
        let err = format!("{}", result.err().unwrap());
        assert!(err.contains("407"), "error should contain 407, got: {err}");
    }

    #[tokio::test]
    async fn do_connect_handshake_fails_on_malformed_response() {
        let (client_io, mut server_io) = tokio::io::duplex(8192);
        let target = "target.example.com:443".to_string();

        tokio::spawn(async move {
            use tokio::io::AsyncWriteExt;
            server_io
                .write_all(b"garbage without status\r\n\r\n")
                .await
                .unwrap();
        });

        let proxy = crate::proxy::ProxyConfig::http("http://proxy:8080").unwrap();
        let stream = crate::runtime::tokio_rt::TokioIo::new(client_io);
        let result = do_connect_handshake(stream, &proxy, &target).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn do_connect_handshake_includes_proxy_auth() {
        let (client_io, mut server_io) = tokio::io::duplex(8192);
        let target = "target.example.com:443".to_string();

        tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let mut buf = [0u8; 4096];
            let n = server_io.read(&mut buf).await.unwrap();
            let req = String::from_utf8_lossy(&buf[..n]);
            assert!(
                req.contains("Proxy-Authorization:"),
                "should include auth header, got: {req}"
            );
            server_io
                .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                .await
                .unwrap();
        });

        let proxy = crate::proxy::ProxyConfig::http("http://proxy:8080")
            .unwrap()
            .basic_auth("user", "pass");
        let stream = crate::runtime::tokio_rt::TokioIo::new(client_io);
        let result = do_connect_handshake(stream, &proxy, &target).await;
        assert!(result.is_ok());
    }

    // ── poll_flush ordering tests ───────────────────────────────────────────

    /// Mock stream that records whether poll_flush was called before poll_read.
    struct FlushTrackingStream {
        response: Vec<u8>,
        read_pos: usize,
        flushed: Cell<bool>,
    }

    impl FlushTrackingStream {
        fn new(response: &[u8]) -> Self {
            Self {
                response: response.to_vec(),
                read_pos: 0,
                flushed: Cell::new(false),
            }
        }
        fn was_flushed(&self) -> bool {
            self.flushed.get()
        }
    }

    impl hyper::rt::Write for FlushTrackingStream {
        fn poll_write(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            buf: &[u8],
        ) -> Poll<std::io::Result<usize>> {
            Poll::Ready(Ok(buf.len()))
        }
        fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            self.flushed.set(true);
            Poll::Ready(Ok(()))
        }
        fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }

    impl hyper::rt::Read for FlushTrackingStream {
        fn poll_read(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            mut buf: hyper::rt::ReadBufCursor<'_>,
        ) -> Poll<std::io::Result<()>> {
            let this = self.get_mut();
            // Assert flush was called before any read
            assert!(
                this.flushed.get(),
                "poll_flush must be called before poll_read"
            );
            if this.read_pos < this.response.len() {
                let remaining = &this.response[this.read_pos..];
                let to_copy = remaining.len().min(buf.remaining());
                let dest = unsafe { buf.as_mut() };
                // Manually copy from initialized bytes into MaybeUninit buffer
                for (i, &byte) in remaining[..to_copy].iter().enumerate() {
                    dest[i].write(byte);
                }
                unsafe { buf.advance(to_copy) };
                this.read_pos += to_copy;
            }
            Poll::Ready(Ok(()))
        }
    }

    #[test]
    fn connect_handshake_flushes_before_read() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let proxy = crate::proxy::ProxyConfig::http("http://proxy:8080").unwrap();
            let response = b"HTTP/1.1 200 Connection Established\r\n\r\n";
            let stream = FlushTrackingStream::new(response);

            let result = do_connect_handshake(stream, &proxy, "example.com:80").await;
            assert!(
                result.is_ok(),
                "handshake should succeed: {:?}",
                result.err()
            );
            assert!(
                result.unwrap().was_flushed(),
                "stream should have been flushed"
            );
        });
    }

    #[test]
    fn connect_handshake_flush_failure_propagates() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let proxy = crate::proxy::ProxyConfig::http("http://proxy:8080").unwrap();

            // Stream that fails on flush
            #[derive(Debug)]
            struct FlushFailingStream;
            impl hyper::rt::Write for FlushFailingStream {
                fn poll_write(
                    self: Pin<&mut Self>,
                    _cx: &mut Context<'_>,
                    buf: &[u8],
                ) -> Poll<std::io::Result<usize>> {
                    Poll::Ready(Ok(buf.len()))
                }
                fn poll_flush(
                    self: Pin<&mut Self>,
                    _cx: &mut Context<'_>,
                ) -> Poll<std::io::Result<()>> {
                    Poll::Ready(Err(std::io::Error::new(
                        std::io::ErrorKind::BrokenPipe,
                        "flush failed",
                    )))
                }
                fn poll_shutdown(
                    self: Pin<&mut Self>,
                    _cx: &mut Context<'_>,
                ) -> Poll<std::io::Result<()>> {
                    Poll::Ready(Ok(()))
                }
            }
            impl hyper::rt::Read for FlushFailingStream {
                fn poll_read(
                    self: Pin<&mut Self>,
                    _cx: &mut Context<'_>,
                    _buf: hyper::rt::ReadBufCursor<'_>,
                ) -> Poll<std::io::Result<()>> {
                    Poll::Ready(Ok(()))
                }
            }

            let result = do_connect_handshake(FlushFailingStream, &proxy, "example.com:80").await;
            assert!(result.is_err());
            assert!(
                format!("{}", result.unwrap_err()).contains("flush"),
                "error should mention flush failure"
            );
        });
    }
}
