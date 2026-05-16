use std::io;
use std::net::Ipv4Addr;
use std::pin::Pin;

use hyper::rt::{Read, Write};

use crate::proxy::ProxyAuth;

const SOCKS4_VERSION: u8 = 0x04;
const CMD_CONNECT: u8 = 0x01;
const REPLY_GRANTED: u8 = 0x5A;

async fn write_all<T: Write + Unpin>(stream: &mut T, buf: &[u8]) -> io::Result<()> {
    let mut written = 0;
    while written < buf.len() {
        let n = std::future::poll_fn(|cx| Pin::new(&mut *stream).poll_write(cx, &buf[written..]))
            .await?;
        if n == 0 {
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "SOCKS4: write returned 0",
            ));
        }
        written += n;
    }
    Ok(())
}

async fn read_exact<T: Read + Unpin>(stream: &mut T, buf: &mut [u8]) -> io::Result<()> {
    let mut filled = 0;
    while filled < buf.len() {
        let mut one = [0u8; 1];
        let mut read_buf = hyper::rt::ReadBuf::new(&mut one);
        std::future::poll_fn(|cx| Pin::new(&mut *stream).poll_read(cx, read_buf.unfilled()))
            .await?;
        if read_buf.filled().is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "SOCKS4: unexpected EOF",
            ));
        }
        buf[filled] = one[0];
        filled += 1;
    }
    Ok(())
}

/// SOCKS4a handshake: connects through a SOCKS4 proxy using domain name resolution on the proxy.
pub(crate) async fn socks4a_handshake<T: Read + Write + Unpin>(
    stream: &mut T,
    host: &str,
    port: u16,
    auth: Option<&ProxyAuth>,
) -> io::Result<()> {
    let userid = auth.map(|a| a.username.as_bytes()).unwrap_or(b"");

    // SOCKS4a: set DSTIP to 0.0.0.1 to signal domain-based addressing
    let dstip = Ipv4Addr::new(0, 0, 0, 1);

    let mut msg = Vec::with_capacity(10 + userid.len() + host.len());
    msg.push(SOCKS4_VERSION);
    msg.push(CMD_CONNECT);
    msg.push((port >> 8) as u8);
    msg.push(port as u8);
    msg.extend_from_slice(&dstip.octets());
    msg.extend_from_slice(userid);
    msg.push(0x00); // NULL terminator for userid
    msg.extend_from_slice(host.as_bytes());
    msg.push(0x00); // NULL terminator for domain
    write_all(stream, &msg).await?;

    let mut reply = [0u8; 8];
    read_exact(stream, &mut reply).await?;

    if reply[1] != REPLY_GRANTED {
        let msg = match reply[1] {
            0x5B => "request rejected or failed",
            0x5C => "cannot connect to identd on the client",
            0x5D => "client's identd reported different user-id",
            _ => "unknown error",
        };
        return Err(io::Error::other(format!(
            "SOCKS4: {msg} (code 0x{:02X})",
            reply[1]
        )));
    }

    Ok(())
}

#[cfg(all(test, feature = "tokio"))]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::io;
    use std::pin::Pin;
    use std::task::{Context, Poll};

    struct MockStream {
        read_data: VecDeque<u8>,
        written: Vec<u8>,
    }

    impl MockStream {
        fn new(read_data: &[u8]) -> Self {
            Self {
                read_data: VecDeque::from(read_data.to_vec()),
                written: Vec::new(),
            }
        }
    }

    impl hyper::rt::Read for MockStream {
        fn poll_read(
            mut self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            mut buf: hyper::rt::ReadBufCursor<'_>,
        ) -> Poll<io::Result<()>> {
            if let Some(byte) = self.read_data.pop_front() {
                unsafe {
                    let dst = buf.as_mut();
                    if !dst.is_empty() {
                        dst[0].write(byte);
                        buf.advance(1);
                    }
                }
            }
            Poll::Ready(Ok(()))
        }
    }

    impl hyper::rt::Write for MockStream {
        fn poll_write(
            mut self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            buf: &[u8],
        ) -> Poll<io::Result<usize>> {
            self.written.extend_from_slice(buf);
            Poll::Ready(Ok(buf.len()))
        }

        fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }

        fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }

    fn make_reply(code: u8) -> [u8; 8] {
        [0x00, code, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]
    }

    #[tokio::test]
    async fn handshake_success() {
        let reply = make_reply(0x5A);
        let mut stream = MockStream::new(&reply);
        let result = socks4a_handshake(&mut stream, "example.com", 80, None).await;
        assert!(result.is_ok());
        assert_eq!(stream.written[0], SOCKS4_VERSION);
        assert_eq!(stream.written[1], CMD_CONNECT);
        assert_eq!(stream.written[2], 0x00);
        assert_eq!(stream.written[3], 80);
    }

    #[tokio::test]
    async fn handshake_success_with_auth() {
        let reply = make_reply(0x5A);
        let mut stream = MockStream::new(&reply);
        let auth = ProxyAuth {
            username: "user".into(),
            password: "pass".into(),
        };
        let result = socks4a_handshake(&mut stream, "example.com", 443, Some(&auth)).await;
        assert!(result.is_ok());
        let msg = &stream.written;
        assert_eq!(&msg[8..12], b"user");
        assert_eq!(msg[12], 0x00);
        assert_eq!(&msg[13..24], b"example.com");
        assert_eq!(msg[24], 0x00);
    }

    #[tokio::test]
    async fn handshake_rejected() {
        let reply = make_reply(0x5B);
        let mut stream = MockStream::new(&reply);
        let err = socks4a_handshake(&mut stream, "example.com", 80, None)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("request rejected or failed"));
    }

    #[tokio::test]
    async fn handshake_identd_error() {
        let reply = make_reply(0x5C);
        let mut stream = MockStream::new(&reply);
        let err = socks4a_handshake(&mut stream, "example.com", 80, None)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("identd"));
    }

    #[tokio::test]
    async fn handshake_different_userid() {
        let reply = make_reply(0x5D);
        let mut stream = MockStream::new(&reply);
        let err = socks4a_handshake(&mut stream, "example.com", 80, None)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("different user-id"));
    }

    #[tokio::test]
    async fn handshake_unknown_error_code() {
        let reply = make_reply(0xFF);
        let mut stream = MockStream::new(&reply);
        let err = socks4a_handshake(&mut stream, "example.com", 80, None)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("unknown error"));
    }

    #[tokio::test]
    async fn handshake_port_encoding() {
        let reply = make_reply(0x5A);
        let mut stream = MockStream::new(&reply);
        socks4a_handshake(&mut stream, "host.test", 8080, None)
            .await
            .unwrap();
        assert_eq!(stream.written[2], 0x1F);
        assert_eq!(stream.written[3], 0x90);
    }

    #[tokio::test]
    async fn handshake_eof_during_reply() {
        let mut stream = MockStream::new(&[0x00, 0x5A, 0x00, 0x00]);
        let err = socks4a_handshake(&mut stream, "example.com", 80, None)
            .await
            .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
    }

    struct WriteZeroStream {
        read_data: VecDeque<u8>,
    }

    impl hyper::rt::Read for WriteZeroStream {
        fn poll_read(
            mut self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            mut buf: hyper::rt::ReadBufCursor<'_>,
        ) -> Poll<io::Result<()>> {
            if let Some(byte) = self.read_data.pop_front() {
                unsafe {
                    let dst = buf.as_mut();
                    if !dst.is_empty() {
                        dst[0].write(byte);
                        buf.advance(1);
                    }
                }
            }
            Poll::Ready(Ok(()))
        }
    }

    impl hyper::rt::Write for WriteZeroStream {
        fn poll_write(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            _buf: &[u8],
        ) -> Poll<io::Result<usize>> {
            Poll::Ready(Ok(0))
        }

        fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }

        fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }

    #[tokio::test]
    async fn write_zero_returns_error() {
        let mut stream = WriteZeroStream {
            read_data: VecDeque::new(),
        };
        let err = socks4a_handshake(&mut stream, "example.com", 80, None)
            .await
            .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::WriteZero);
    }

    #[tokio::test]
    async fn handshake_socks4a_message_format_no_auth() {
        // Verify the exact byte format of the SOCKS4a CONNECT request
        let reply = make_reply(0x5A);
        let mut stream = MockStream::new(&reply);
        socks4a_handshake(&mut stream, "target.host", 443, None)
            .await
            .unwrap();

        let msg = &stream.written;
        assert_eq!(msg[0], SOCKS4_VERSION);
        assert_eq!(msg[1], CMD_CONNECT);
        // Port 443 = 0x01BB
        assert_eq!(msg[2], 0x01);
        assert_eq!(msg[3], 0xBB);
        // DSTIP = 0.0.0.1 (SOCKS4a indicator)
        assert_eq!(&msg[4..8], &[0, 0, 0, 1]);
        // Empty userid followed by NULL
        assert_eq!(msg[8], 0x00);
        // Domain name followed by NULL
        assert_eq!(&msg[9..20], b"target.host");
        assert_eq!(msg[20], 0x00);
    }

    #[tokio::test]
    async fn handshake_socks4a_message_format_with_auth() {
        // Verify userid is included in the message
        let reply = make_reply(0x5A);
        let mut stream = MockStream::new(&reply);
        let auth = ProxyAuth {
            username: "testuser".into(),
            password: "ignored-in-socks4".into(),
        };
        socks4a_handshake(&mut stream, "host.io", 8080, Some(&auth))
            .await
            .unwrap();

        let msg = &stream.written;
        assert_eq!(msg[0], SOCKS4_VERSION);
        assert_eq!(msg[1], CMD_CONNECT);
        // Port 8080 = 0x1F90
        assert_eq!(msg[2], 0x1F);
        assert_eq!(msg[3], 0x90);
        // DSTIP
        assert_eq!(&msg[4..8], &[0, 0, 0, 1]);
        // Userid "testuser" followed by NULL
        assert_eq!(&msg[8..16], b"testuser");
        assert_eq!(msg[16], 0x00);
        // Domain "host.io" followed by NULL
        assert_eq!(&msg[17..24], b"host.io");
        assert_eq!(msg[24], 0x00);
    }

    #[tokio::test]
    async fn handshake_eof_immediately() {
        // Server returns no bytes at all
        let mut stream = MockStream::new(&[]);
        let err = socks4a_handshake(&mut stream, "example.com", 80, None)
            .await
            .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
    }

    #[tokio::test]
    async fn handshake_partial_reply_eof() {
        // Server sends only 3 bytes of the 8-byte reply
        let mut stream = MockStream::new(&[0x00, 0x5A, 0x00]);
        let err = socks4a_handshake(&mut stream, "example.com", 80, None)
            .await
            .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
    }

    // --- Tests for mock stream methods to cover poll_flush/poll_shutdown ---

    #[test]
    fn mock_stream_poll_flush_returns_ready_ok() {
        use std::task::{RawWaker, RawWakerVTable, Waker};
        fn dummy_raw_waker() -> RawWaker {
            fn no_op(_: *const ()) {}
            fn clone(ptr: *const ()) -> RawWaker {
                RawWaker::new(ptr, &VTABLE)
            }
            const VTABLE: RawWakerVTable = RawWakerVTable::new(clone, no_op, no_op, no_op);
            RawWaker::new(std::ptr::null(), &VTABLE)
        }
        let waker = unsafe { Waker::from_raw(dummy_raw_waker()) };
        let mut cx = Context::from_waker(&waker);
        let mut stream = MockStream::new(&[]);
        let pinned = Pin::new(&mut stream);
        let result = hyper::rt::Write::poll_flush(pinned, &mut cx);
        assert!(matches!(result, Poll::Ready(Ok(()))));
    }

    #[test]
    fn mock_stream_poll_shutdown_returns_ready_ok() {
        use std::task::{RawWaker, RawWakerVTable, Waker};
        fn dummy_raw_waker() -> RawWaker {
            fn no_op(_: *const ()) {}
            fn clone(ptr: *const ()) -> RawWaker {
                RawWaker::new(ptr, &VTABLE)
            }
            const VTABLE: RawWakerVTable = RawWakerVTable::new(clone, no_op, no_op, no_op);
            RawWaker::new(std::ptr::null(), &VTABLE)
        }
        let waker = unsafe { Waker::from_raw(dummy_raw_waker()) };
        let mut cx = Context::from_waker(&waker);
        let mut stream = MockStream::new(&[]);
        let pinned = Pin::new(&mut stream);
        let result = hyper::rt::Write::poll_shutdown(pinned, &mut cx);
        assert!(matches!(result, Poll::Ready(Ok(()))));
    }

    #[test]
    fn write_zero_stream_poll_flush_returns_ready_ok() {
        use std::task::{RawWaker, RawWakerVTable, Waker};
        fn dummy_raw_waker() -> RawWaker {
            fn no_op(_: *const ()) {}
            fn clone(ptr: *const ()) -> RawWaker {
                RawWaker::new(ptr, &VTABLE)
            }
            const VTABLE: RawWakerVTable = RawWakerVTable::new(clone, no_op, no_op, no_op);
            RawWaker::new(std::ptr::null(), &VTABLE)
        }
        let waker = unsafe { Waker::from_raw(dummy_raw_waker()) };
        let mut cx = Context::from_waker(&waker);
        let mut stream = WriteZeroStream {
            read_data: VecDeque::new(),
        };
        let pinned = Pin::new(&mut stream);
        let result = hyper::rt::Write::poll_flush(pinned, &mut cx);
        assert!(matches!(result, Poll::Ready(Ok(()))));
    }

    #[test]
    fn write_zero_stream_poll_shutdown_returns_ready_ok() {
        use std::task::{RawWaker, RawWakerVTable, Waker};
        fn dummy_raw_waker() -> RawWaker {
            fn no_op(_: *const ()) {}
            fn clone(ptr: *const ()) -> RawWaker {
                RawWaker::new(ptr, &VTABLE)
            }
            const VTABLE: RawWakerVTable = RawWakerVTable::new(clone, no_op, no_op, no_op);
            RawWaker::new(std::ptr::null(), &VTABLE)
        }
        let waker = unsafe { Waker::from_raw(dummy_raw_waker()) };
        let mut cx = Context::from_waker(&waker);
        let mut stream = WriteZeroStream {
            read_data: VecDeque::new(),
        };
        let pinned = Pin::new(&mut stream);
        let result = hyper::rt::Write::poll_shutdown(pinned, &mut cx);
        assert!(matches!(result, Poll::Ready(Ok(()))));
    }

    #[test]
    fn write_zero_stream_poll_read_with_data() {
        use std::task::{RawWaker, RawWakerVTable, Waker};
        fn dummy_raw_waker() -> RawWaker {
            fn no_op(_: *const ()) {}
            fn clone(ptr: *const ()) -> RawWaker {
                RawWaker::new(ptr, &VTABLE)
            }
            const VTABLE: RawWakerVTable = RawWakerVTable::new(clone, no_op, no_op, no_op);
            RawWaker::new(std::ptr::null(), &VTABLE)
        }
        let waker = unsafe { Waker::from_raw(dummy_raw_waker()) };
        let mut cx = Context::from_waker(&waker);
        let mut stream = WriteZeroStream {
            read_data: VecDeque::from(vec![0xAB, 0xCD]),
        };
        let mut buf_storage = [0u8; 4];
        let mut read_buf = hyper::rt::ReadBuf::new(&mut buf_storage);
        let result =
            hyper::rt::Read::poll_read(Pin::new(&mut stream), &mut cx, read_buf.unfilled());
        assert!(matches!(result, Poll::Ready(Ok(()))));
        // After reading, the first byte should have been consumed
        assert_eq!(stream.read_data.len(), 1);
        assert_eq!(stream.read_data[0], 0xCD);
    }
}
