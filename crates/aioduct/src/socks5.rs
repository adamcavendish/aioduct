use std::io;
use std::pin::Pin;

use hyper::rt::{Read, Write};

use crate::proxy::ProxyAuth;

const SOCKS5_VERSION: u8 = 0x05;
const AUTH_NONE: u8 = 0x00;
const AUTH_USERNAME_PASSWORD: u8 = 0x02;
const AUTH_NO_ACCEPTABLE: u8 = 0xFF;
const CMD_CONNECT: u8 = 0x01;
const ATYP_DOMAIN: u8 = 0x03;
const REPLY_SUCCESS: u8 = 0x00;
const USERNAME_PASSWORD_VERSION: u8 = 0x01;

async fn write_all<T: Write + Unpin>(stream: &mut T, buf: &[u8]) -> io::Result<()> {
    let mut written = 0;
    while written < buf.len() {
        let n = std::future::poll_fn(|cx| Pin::new(&mut *stream).poll_write(cx, &buf[written..]))
            .await?;
        if n == 0 {
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "SOCKS5: write returned 0",
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
                "SOCKS5: unexpected EOF",
            ));
        }
        buf[filled] = one[0];
        filled += 1;
    }
    Ok(())
}

pub(crate) async fn socks5_handshake<T: Read + Write + Unpin>(
    stream: &mut T,
    host: &str,
    port: u16,
    auth: Option<&ProxyAuth>,
) -> io::Result<()> {
    let methods: Vec<u8> = if auth.is_some() {
        vec![SOCKS5_VERSION, 2, AUTH_NONE, AUTH_USERNAME_PASSWORD]
    } else {
        vec![SOCKS5_VERSION, 1, AUTH_NONE]
    };
    write_all(stream, &methods).await?;

    let mut resp = [0u8; 2];
    read_exact(stream, &mut resp).await?;

    if resp[0] != SOCKS5_VERSION {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("SOCKS5: unexpected version {}", resp[0]),
        ));
    }

    match resp[1] {
        AUTH_NONE => {}
        AUTH_USERNAME_PASSWORD => {
            let auth = auth.ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "SOCKS5: server requires auth but none provided",
                )
            })?;
            let mut auth_msg = Vec::with_capacity(3 + auth.username.len() + auth.password.len());
            auth_msg.push(USERNAME_PASSWORD_VERSION);
            auth_msg.push(auth.username.len() as u8);
            auth_msg.extend_from_slice(auth.username.as_bytes());
            auth_msg.push(auth.password.len() as u8);
            auth_msg.extend_from_slice(auth.password.as_bytes());
            write_all(stream, &auth_msg).await?;

            let mut auth_resp = [0u8; 2];
            read_exact(stream, &mut auth_resp).await?;
            if auth_resp[1] != 0x00 {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "SOCKS5: authentication failed",
                ));
            }
        }
        AUTH_NO_ACCEPTABLE => {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "SOCKS5: no acceptable authentication method",
            ));
        }
        other => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("SOCKS5: unsupported auth method {other}"),
            ));
        }
    }

    let host_bytes = host.as_bytes();
    if host_bytes.len() > 255 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "SOCKS5: hostname too long",
        ));
    }
    let mut connect_msg = Vec::with_capacity(7 + host_bytes.len());
    connect_msg.push(SOCKS5_VERSION);
    connect_msg.push(CMD_CONNECT);
    connect_msg.push(0x00); // reserved
    connect_msg.push(ATYP_DOMAIN);
    connect_msg.push(host_bytes.len() as u8);
    connect_msg.extend_from_slice(host_bytes);
    connect_msg.push((port >> 8) as u8);
    connect_msg.push(port as u8);
    write_all(stream, &connect_msg).await?;

    let mut reply_header = [0u8; 4];
    read_exact(stream, &mut reply_header).await?;

    if reply_header[0] != SOCKS5_VERSION {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("SOCKS5: unexpected reply version {}", reply_header[0]),
        ));
    }

    if reply_header[1] != REPLY_SUCCESS {
        let msg = match reply_header[1] {
            0x01 => "general failure",
            0x02 => "connection not allowed by ruleset",
            0x03 => "network unreachable",
            0x04 => "host unreachable",
            0x05 => "connection refused",
            0x06 => "TTL expired",
            0x07 => "command not supported",
            0x08 => "address type not supported",
            _ => "unknown error",
        };
        return Err(io::Error::other(format!(
            "SOCKS5: {msg} (code 0x{:02x})",
            reply_header[1]
        )));
    }

    // Read and discard the bound address
    match reply_header[3] {
        0x01 => {
            // IPv4: 4 bytes + 2 port
            let mut buf = [0u8; 6];
            read_exact(stream, &mut buf).await?;
        }
        0x03 => {
            // Domain: 1 byte length + domain + 2 port
            let mut len_buf = [0u8; 1];
            read_exact(stream, &mut len_buf).await?;
            let mut buf = vec![0u8; len_buf[0] as usize + 2];
            read_exact(stream, &mut buf).await?;
        }
        0x04 => {
            // IPv6: 16 bytes + 2 port
            let mut buf = [0u8; 18];
            read_exact(stream, &mut buf).await?;
        }
        other => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("SOCKS5: unknown address type {other}"),
            ));
        }
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

    fn ipv4_reply() -> Vec<u8> {
        let mut v = vec![SOCKS5_VERSION, REPLY_SUCCESS, 0x00, 0x01];
        v.extend_from_slice(&[127, 0, 0, 1]);
        v.extend_from_slice(&[0x00, 0x50]);
        v
    }

    fn domain_reply(domain: &str) -> Vec<u8> {
        let mut v = vec![SOCKS5_VERSION, REPLY_SUCCESS, 0x00, 0x03];
        v.push(domain.len() as u8);
        v.extend_from_slice(domain.as_bytes());
        v.extend_from_slice(&[0x00, 0x50]);
        v
    }

    fn ipv6_reply() -> Vec<u8> {
        let mut v = vec![SOCKS5_VERSION, REPLY_SUCCESS, 0x00, 0x04];
        v.extend_from_slice(&[0u8; 16]);
        v.extend_from_slice(&[0x00, 0x50]);
        v
    }

    #[tokio::test]
    async fn handshake_no_auth_ipv4() {
        let mut reply = vec![SOCKS5_VERSION, AUTH_NONE];
        reply.extend_from_slice(&ipv4_reply());
        let mut stream = MockStream::new(&reply);
        let result = socks5_handshake(&mut stream, "example.com", 80, None).await;
        assert!(result.is_ok());
        assert_eq!(stream.written[0], SOCKS5_VERSION);
        assert_eq!(stream.written[1], 1);
        assert_eq!(stream.written[2], AUTH_NONE);
    }

    #[tokio::test]
    async fn handshake_with_auth_success() {
        let mut reply = vec![SOCKS5_VERSION, AUTH_USERNAME_PASSWORD];
        reply.extend_from_slice(&[0x01, 0x00]);
        reply.extend_from_slice(&ipv4_reply());
        let mut stream = MockStream::new(&reply);
        let auth = ProxyAuth {
            username: "user".into(),
            password: "pass".into(),
        };
        let result = socks5_handshake(&mut stream, "example.com", 80, Some(&auth)).await;
        assert!(result.is_ok());
        assert_eq!(stream.written[0], SOCKS5_VERSION);
        assert_eq!(stream.written[1], 2);
    }

    #[tokio::test]
    async fn handshake_auth_failed() {
        let mut reply = vec![SOCKS5_VERSION, AUTH_USERNAME_PASSWORD];
        reply.extend_from_slice(&[0x01, 0x01]);
        let mut stream = MockStream::new(&reply);
        let auth = ProxyAuth {
            username: "user".into(),
            password: "wrong".into(),
        };
        let err = socks5_handshake(&mut stream, "example.com", 80, Some(&auth))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("authentication failed"));
    }

    #[tokio::test]
    async fn handshake_no_acceptable_method() {
        let reply = vec![SOCKS5_VERSION, AUTH_NO_ACCEPTABLE];
        let mut stream = MockStream::new(&reply);
        let err = socks5_handshake(&mut stream, "example.com", 80, None)
            .await
            .unwrap_err();
        assert!(
            err.to_string()
                .contains("no acceptable authentication method")
        );
    }

    #[tokio::test]
    async fn handshake_unsupported_auth_method() {
        let reply = vec![SOCKS5_VERSION, 0x03];
        let mut stream = MockStream::new(&reply);
        let err = socks5_handshake(&mut stream, "example.com", 80, None)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("unsupported auth method"));
    }

    #[tokio::test]
    async fn handshake_unexpected_version() {
        let reply = vec![0x04, AUTH_NONE];
        let mut stream = MockStream::new(&reply);
        let err = socks5_handshake(&mut stream, "example.com", 80, None)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("unexpected version"));
    }

    #[tokio::test]
    async fn handshake_unexpected_reply_version() {
        let mut reply = vec![SOCKS5_VERSION, AUTH_NONE];
        reply.extend_from_slice(&[0x04, REPLY_SUCCESS, 0x00, 0x01]);
        reply.extend_from_slice(&[127, 0, 0, 1, 0x00, 0x50]);
        let mut stream = MockStream::new(&reply);
        let err = socks5_handshake(&mut stream, "example.com", 80, None)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("unexpected reply version"));
    }

    #[tokio::test]
    async fn handshake_reply_general_failure() {
        let mut reply = vec![SOCKS5_VERSION, AUTH_NONE];
        reply.extend_from_slice(&[SOCKS5_VERSION, 0x01, 0x00, 0x01]);
        reply.extend_from_slice(&[0, 0, 0, 0, 0, 0]);
        let mut stream = MockStream::new(&reply);
        let err = socks5_handshake(&mut stream, "example.com", 80, None)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("general failure"));
    }

    #[tokio::test]
    async fn handshake_reply_connection_refused() {
        let mut reply = vec![SOCKS5_VERSION, AUTH_NONE];
        reply.extend_from_slice(&[SOCKS5_VERSION, 0x05, 0x00, 0x01]);
        reply.extend_from_slice(&[0, 0, 0, 0, 0, 0]);
        let mut stream = MockStream::new(&reply);
        let err = socks5_handshake(&mut stream, "example.com", 80, None)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("connection refused"));
    }

    #[tokio::test]
    async fn handshake_reply_unknown_error() {
        let mut reply = vec![SOCKS5_VERSION, AUTH_NONE];
        reply.extend_from_slice(&[SOCKS5_VERSION, 0x09, 0x00, 0x01]);
        reply.extend_from_slice(&[0, 0, 0, 0, 0, 0]);
        let mut stream = MockStream::new(&reply);
        let err = socks5_handshake(&mut stream, "example.com", 80, None)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("unknown error"));
    }

    #[tokio::test]
    async fn handshake_domain_reply() {
        let mut reply = vec![SOCKS5_VERSION, AUTH_NONE];
        reply.extend_from_slice(&domain_reply("bound.host"));
        let mut stream = MockStream::new(&reply);
        let result = socks5_handshake(&mut stream, "example.com", 80, None).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn handshake_ipv6_reply() {
        let mut reply = vec![SOCKS5_VERSION, AUTH_NONE];
        reply.extend_from_slice(&ipv6_reply());
        let mut stream = MockStream::new(&reply);
        let result = socks5_handshake(&mut stream, "example.com", 80, None).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn handshake_unknown_address_type() {
        let mut reply = vec![SOCKS5_VERSION, AUTH_NONE];
        reply.extend_from_slice(&[SOCKS5_VERSION, REPLY_SUCCESS, 0x00, 0x05]);
        let mut stream = MockStream::new(&reply);
        let err = socks5_handshake(&mut stream, "example.com", 80, None)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("unknown address type"));
    }

    #[tokio::test]
    async fn handshake_hostname_too_long() {
        let long_host = "a".repeat(256);
        let mut reply = vec![SOCKS5_VERSION, AUTH_NONE];
        reply.extend_from_slice(&ipv4_reply());
        let mut stream = MockStream::new(&reply);
        let err = socks5_handshake(&mut stream, &long_host, 80, None)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("hostname too long"));
    }

    #[tokio::test]
    async fn handshake_auth_required_but_not_provided() {
        let reply = vec![SOCKS5_VERSION, AUTH_USERNAME_PASSWORD];
        let mut stream = MockStream::new(&reply);
        let err = socks5_handshake(&mut stream, "example.com", 80, None)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("server requires auth"));
    }

    #[tokio::test]
    async fn handshake_reply_connection_not_allowed() {
        let mut reply = vec![SOCKS5_VERSION, AUTH_NONE];
        reply.extend_from_slice(&[SOCKS5_VERSION, 0x02, 0x00, 0x01]);
        reply.extend_from_slice(&[0, 0, 0, 0, 0, 0]);
        let mut stream = MockStream::new(&reply);
        let err = socks5_handshake(&mut stream, "example.com", 80, None)
            .await
            .unwrap_err();
        assert!(
            err.to_string()
                .contains("connection not allowed by ruleset")
        );
    }

    #[tokio::test]
    async fn handshake_reply_network_unreachable() {
        let mut reply = vec![SOCKS5_VERSION, AUTH_NONE];
        reply.extend_from_slice(&[SOCKS5_VERSION, 0x03, 0x00, 0x01]);
        reply.extend_from_slice(&[0, 0, 0, 0, 0, 0]);
        let mut stream = MockStream::new(&reply);
        let err = socks5_handshake(&mut stream, "example.com", 80, None)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("network unreachable"));
    }

    #[tokio::test]
    async fn handshake_reply_host_unreachable() {
        let mut reply = vec![SOCKS5_VERSION, AUTH_NONE];
        reply.extend_from_slice(&[SOCKS5_VERSION, 0x04, 0x00, 0x01]);
        reply.extend_from_slice(&[0, 0, 0, 0, 0, 0]);
        let mut stream = MockStream::new(&reply);
        let err = socks5_handshake(&mut stream, "example.com", 80, None)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("host unreachable"));
    }

    #[tokio::test]
    async fn handshake_reply_ttl_expired() {
        let mut reply = vec![SOCKS5_VERSION, AUTH_NONE];
        reply.extend_from_slice(&[SOCKS5_VERSION, 0x06, 0x00, 0x01]);
        reply.extend_from_slice(&[0, 0, 0, 0, 0, 0]);
        let mut stream = MockStream::new(&reply);
        let err = socks5_handshake(&mut stream, "example.com", 80, None)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("TTL expired"));
    }

    #[tokio::test]
    async fn handshake_reply_command_not_supported() {
        let mut reply = vec![SOCKS5_VERSION, AUTH_NONE];
        reply.extend_from_slice(&[SOCKS5_VERSION, 0x07, 0x00, 0x01]);
        reply.extend_from_slice(&[0, 0, 0, 0, 0, 0]);
        let mut stream = MockStream::new(&reply);
        let err = socks5_handshake(&mut stream, "example.com", 80, None)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("command not supported"));
    }

    #[tokio::test]
    async fn handshake_reply_address_type_not_supported() {
        let mut reply = vec![SOCKS5_VERSION, AUTH_NONE];
        reply.extend_from_slice(&[SOCKS5_VERSION, 0x08, 0x00, 0x01]);
        reply.extend_from_slice(&[0, 0, 0, 0, 0, 0]);
        let mut stream = MockStream::new(&reply);
        let err = socks5_handshake(&mut stream, "example.com", 80, None)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("address type not supported"));
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
        let err = socks5_handshake(&mut stream, "example.com", 80, None)
            .await
            .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::WriteZero);
    }

    struct EofStream;

    impl hyper::rt::Read for EofStream {
        fn poll_read(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            _buf: hyper::rt::ReadBufCursor<'_>,
        ) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }

    impl hyper::rt::Write for EofStream {
        fn poll_write(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            buf: &[u8],
        ) -> Poll<io::Result<usize>> {
            Poll::Ready(Ok(buf.len()))
        }

        fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }

        fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }

    #[tokio::test]
    async fn read_eof_returns_error() {
        let mut stream = EofStream;
        let err = socks5_handshake(&mut stream, "example.com", 80, None)
            .await
            .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
    }

    #[tokio::test]
    async fn handshake_no_auth_domain_reply() {
        let mut reply = vec![SOCKS5_VERSION, AUTH_NONE];
        reply.extend_from_slice(&domain_reply("bound.example.com"));
        let mut stream = MockStream::new(&reply);
        let result = socks5_handshake(&mut stream, "example.com", 80, None).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn handshake_no_auth_ipv6_reply() {
        let mut reply = vec![SOCKS5_VERSION, AUTH_NONE];
        reply.extend_from_slice(&ipv6_reply());
        let mut stream = MockStream::new(&reply);
        let result = socks5_handshake(&mut stream, "example.com", 80, None).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn handshake_auth_required_but_none_provided() {
        let reply = vec![SOCKS5_VERSION, AUTH_USERNAME_PASSWORD];
        let mut stream = MockStream::new(&reply);
        let err = socks5_handshake(&mut stream, "example.com", 80, None)
            .await
            .unwrap_err();
        assert!(
            err.to_string()
                .contains("server requires auth but none provided")
        );
    }

    #[tokio::test]
    async fn handshake_auth_sends_correct_subneg_message() {
        // Verify the username/password sub-negotiation message format
        let mut reply = vec![SOCKS5_VERSION, AUTH_USERNAME_PASSWORD];
        reply.extend_from_slice(&[0x01, 0x00]); // auth success
        reply.extend_from_slice(&ipv4_reply());
        let mut stream = MockStream::new(&reply);
        let auth = ProxyAuth {
            username: "admin".into(),
            password: "secret".into(),
        };
        let result = socks5_handshake(&mut stream, "target.com", 443, Some(&auth)).await;
        assert!(result.is_ok());

        // Verify the greeting offered both auth methods
        assert_eq!(stream.written[0], SOCKS5_VERSION);
        assert_eq!(stream.written[1], 2); // 2 methods
        assert_eq!(stream.written[2], AUTH_NONE);
        assert_eq!(stream.written[3], AUTH_USERNAME_PASSWORD);

        // Verify the auth sub-negotiation message (starts at offset 4)
        assert_eq!(stream.written[4], USERNAME_PASSWORD_VERSION); // sub-neg version
        assert_eq!(stream.written[5], 5); // username length "admin"
        assert_eq!(&stream.written[6..11], b"admin");
        assert_eq!(stream.written[11], 6); // password length "secret"
        assert_eq!(&stream.written[12..18], b"secret");
    }

    #[tokio::test]
    async fn handshake_connect_message_format() {
        // Verify the CONNECT request message format
        let mut reply = vec![SOCKS5_VERSION, AUTH_NONE];
        reply.extend_from_slice(&ipv4_reply());
        let mut stream = MockStream::new(&reply);
        let result = socks5_handshake(&mut stream, "test.io", 8080, None).await;
        assert!(result.is_ok());

        // The connect message starts after the greeting (3 bytes: VER, NMETHODS, METHOD)
        let connect_start = 3; // greeting is 3 bytes for no-auth
        let msg = &stream.written[connect_start..];
        assert_eq!(msg[0], SOCKS5_VERSION);
        assert_eq!(msg[1], CMD_CONNECT);
        assert_eq!(msg[2], 0x00); // reserved
        assert_eq!(msg[3], ATYP_DOMAIN);
        assert_eq!(msg[4], 7); // "test.io" length
        assert_eq!(&msg[5..12], b"test.io");
        // Port 8080 = 0x1F90
        assert_eq!(msg[12], 0x1F);
        assert_eq!(msg[13], 0x90);
    }

    #[tokio::test]
    async fn write_zero_during_auth_phase() {
        // A stream that allows the greeting write but returns 0 on subsequent writes
        struct WriteOnceStream {
            read_data: VecDeque<u8>,
            writes_allowed: usize,
            written: Vec<u8>,
        }

        impl hyper::rt::Read for WriteOnceStream {
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

        impl hyper::rt::Write for WriteOnceStream {
            fn poll_write(
                mut self: Pin<&mut Self>,
                _cx: &mut Context<'_>,
                buf: &[u8],
            ) -> Poll<io::Result<usize>> {
                if self.writes_allowed > 0 {
                    self.writes_allowed -= 1;
                    self.written.extend_from_slice(buf);
                    Poll::Ready(Ok(buf.len()))
                } else {
                    Poll::Ready(Ok(0))
                }
            }

            fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
                Poll::Ready(Ok(()))
            }

            fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
                Poll::Ready(Ok(()))
            }
        }

        // Allow the greeting to be written, then fail on auth sub-negotiation
        let mut read_data = VecDeque::new();
        // Server responds with AUTH_USERNAME_PASSWORD
        read_data.extend(&[SOCKS5_VERSION, AUTH_USERNAME_PASSWORD]);
        let mut stream = WriteOnceStream {
            read_data,
            writes_allowed: 1, // only the greeting write succeeds
            written: Vec::new(),
        };
        let auth = ProxyAuth {
            username: "user".into(),
            password: "pass".into(),
        };
        let err = socks5_handshake(&mut stream, "example.com", 80, Some(&auth))
            .await
            .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::WriteZero);
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
        assert_eq!(stream.read_data.len(), 1);
        assert_eq!(stream.read_data[0], 0xCD);
    }

    #[test]
    fn eof_stream_poll_flush_returns_ready_ok() {
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
        let mut stream = EofStream;
        let pinned = Pin::new(&mut stream);
        let result = hyper::rt::Write::poll_flush(pinned, &mut cx);
        assert!(matches!(result, Poll::Ready(Ok(()))));
    }

    #[test]
    fn eof_stream_poll_shutdown_returns_ready_ok() {
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
        let mut stream = EofStream;
        let pinned = Pin::new(&mut stream);
        let result = hyper::rt::Write::poll_shutdown(pinned, &mut cx);
        assert!(matches!(result, Poll::Ready(Ok(()))));
    }
}
