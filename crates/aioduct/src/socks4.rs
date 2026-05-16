use std::io::{self, Read, Write};
use std::net::{Ipv4Addr, TcpStream};

use crate::proxy::ProxyAuth;

const SOCKS4_VERSION: u8 = 0x04;
const CMD_CONNECT: u8 = 0x01;
const REPLY_GRANTED: u8 = 0x5A;

/// SOCKS4a handshake: connects through a SOCKS4 proxy using domain name resolution on the proxy.
pub(crate) fn socks4a_handshake(
    stream: &mut TcpStream,
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
    stream.write_all(&msg)?;

    let mut reply = [0u8; 8];
    stream.read_exact(&mut reply)?;

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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    fn make_reply(code: u8) -> [u8; 8] {
        [0x00, code, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]
    }

    fn run_test<F>(server_fn: F, client_fn: impl FnOnce(&mut TcpStream) + Send + 'static)
    where
        F: FnOnce(&mut std::net::TcpStream) + Send + 'static,
    {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();

        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            server_fn(&mut stream);
        });

        let mut client = TcpStream::connect(addr).unwrap();
        client_fn(&mut client);
        server.join().unwrap();
    }

    #[test]
    fn handshake_success() {
        run_test(
            |server| {
                let mut buf = [0u8; 256];
                let n = server.read(&mut buf).unwrap();
                assert!(n > 0);
                assert_eq!(buf[0], SOCKS4_VERSION);
                assert_eq!(buf[1], CMD_CONNECT);
                assert_eq!(buf[2], 0x00);
                assert_eq!(buf[3], 80);
                server.write_all(&make_reply(0x5A)).unwrap();
            },
            |client| {
                let result = socks4a_handshake(client, "example.com", 80, None);
                assert!(result.is_ok());
            },
        );
    }

    #[test]
    fn handshake_success_with_auth() {
        run_test(
            |server| {
                let mut buf = [0u8; 256];
                let n = server.read(&mut buf).unwrap();
                let msg = &buf[..n];
                // Verify userid is present
                assert_eq!(&msg[8..12], b"user");
                assert_eq!(msg[12], 0x00);
                assert_eq!(&msg[13..24], b"example.com");
                assert_eq!(msg[24], 0x00);
                server.write_all(&make_reply(0x5A)).unwrap();
            },
            |client| {
                let auth = ProxyAuth {
                    username: "user".into(),
                    password: "pass".into(),
                };
                let result = socks4a_handshake(client, "example.com", 443, Some(&auth));
                assert!(result.is_ok());
            },
        );
    }

    #[test]
    fn handshake_rejected() {
        run_test(
            |server| {
                let mut buf = [0u8; 256];
                let _ = server.read(&mut buf).unwrap();
                server.write_all(&make_reply(0x5B)).unwrap();
            },
            |client| {
                let err = socks4a_handshake(client, "example.com", 80, None).unwrap_err();
                assert!(err.to_string().contains("request rejected or failed"));
            },
        );
    }

    #[test]
    fn handshake_identd_error() {
        run_test(
            |server| {
                let mut buf = [0u8; 256];
                let _ = server.read(&mut buf).unwrap();
                server.write_all(&make_reply(0x5C)).unwrap();
            },
            |client| {
                let err = socks4a_handshake(client, "example.com", 80, None).unwrap_err();
                assert!(err.to_string().contains("identd"));
            },
        );
    }

    #[test]
    fn handshake_different_userid() {
        run_test(
            |server| {
                let mut buf = [0u8; 256];
                let _ = server.read(&mut buf).unwrap();
                server.write_all(&make_reply(0x5D)).unwrap();
            },
            |client| {
                let err = socks4a_handshake(client, "example.com", 80, None).unwrap_err();
                assert!(err.to_string().contains("different user-id"));
            },
        );
    }

    #[test]
    fn handshake_unknown_error_code() {
        run_test(
            |server| {
                let mut buf = [0u8; 256];
                let _ = server.read(&mut buf).unwrap();
                server.write_all(&make_reply(0xFF)).unwrap();
            },
            |client| {
                let err = socks4a_handshake(client, "example.com", 80, None).unwrap_err();
                assert!(err.to_string().contains("unknown error"));
            },
        );
    }

    #[test]
    fn handshake_port_encoding() {
        run_test(
            |server| {
                let mut buf = [0u8; 256];
                let n = server.read(&mut buf).unwrap();
                assert!(n > 0);
                // Port 8080 = 0x1F90
                assert_eq!(buf[2], 0x1F);
                assert_eq!(buf[3], 0x90);
                server.write_all(&make_reply(0x5A)).unwrap();
            },
            |client| {
                socks4a_handshake(client, "host.test", 8080, None).unwrap();
            },
        );
    }

    #[test]
    fn handshake_eof_during_reply() {
        run_test(
            |server| {
                let mut buf = [0u8; 256];
                let _ = server.read(&mut buf).unwrap();
                // Only send 4 of the 8 expected reply bytes
                server.write_all(&[0x00, 0x5A, 0x00, 0x00]).unwrap();
                // Closing connection — the owned TcpStream drops when this closure returns
            },
            |client| {
                let err = socks4a_handshake(client, "example.com", 80, None).unwrap_err();
                assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
            },
        );
    }

    #[test]
    fn handshake_socks4a_message_format_no_auth() {
        run_test(
            |server| {
                let mut buf = [0u8; 256];
                let n = server.read(&mut buf).unwrap();
                let msg = &buf[..n];
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
                server.write_all(&make_reply(0x5A)).unwrap();
            },
            |client| {
                socks4a_handshake(client, "target.host", 443, None).unwrap();
            },
        );
    }

    #[test]
    fn handshake_socks4a_message_format_with_auth() {
        run_test(
            |server| {
                let mut buf = [0u8; 256];
                let n = server.read(&mut buf).unwrap();
                let msg = &buf[..n];
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
                server.write_all(&make_reply(0x5A)).unwrap();
            },
            |client| {
                let auth = ProxyAuth {
                    username: "testuser".into(),
                    password: "ignored-in-socks4".into(),
                };
                socks4a_handshake(client, "host.io", 8080, Some(&auth)).unwrap();
            },
        );
    }

    #[test]
    fn handshake_eof_immediately() {
        run_test(
            |server| {
                let mut buf = [0u8; 256];
                let _ = server.read(&mut buf);
                // Closing connection — the owned TcpStream drops when this closure returns
            },
            |client| {
                let err = socks4a_handshake(client, "example.com", 80, None).unwrap_err();
                assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
            },
        );
    }
}
