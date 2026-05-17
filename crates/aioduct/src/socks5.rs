use std::io::{self, Read, Write};
use std::net::TcpStream;

use crate::proxy::ProxyAuth;

const SOCKS5_VERSION: u8 = 0x05;
const AUTH_NONE: u8 = 0x00;
const AUTH_USERNAME_PASSWORD: u8 = 0x02;
const AUTH_NO_ACCEPTABLE: u8 = 0xFF;
const CMD_CONNECT: u8 = 0x01;
const ATYP_DOMAIN: u8 = 0x03;
const REPLY_SUCCESS: u8 = 0x00;
const USERNAME_PASSWORD_VERSION: u8 = 0x01;

pub(crate) fn socks5_handshake(
    stream: &mut TcpStream,
    host: &str,
    port: u16,
    auth: Option<&ProxyAuth>,
) -> io::Result<()> {
    let methods: Vec<u8> = if auth.is_some() {
        vec![SOCKS5_VERSION, 2, AUTH_NONE, AUTH_USERNAME_PASSWORD]
    } else {
        vec![SOCKS5_VERSION, 1, AUTH_NONE]
    };
    stream.write_all(&methods)?;

    let mut resp = [0u8; 2];
    stream.read_exact(&mut resp)?;

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
            if auth.username.len() > 255 || auth.password.len() > 255 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "SOCKS5: username and password must be at most 255 bytes",
                ));
            }
            auth_msg.push(auth.username.len() as u8);
            auth_msg.extend_from_slice(auth.username.as_bytes());
            auth_msg.push(auth.password.len() as u8);
            auth_msg.extend_from_slice(auth.password.as_bytes());
            stream.write_all(&auth_msg)?;

            let mut auth_resp = [0u8; 2];
            stream.read_exact(&mut auth_resp)?;
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
    stream.write_all(&connect_msg)?;

    let mut reply_header = [0u8; 4];
    stream.read_exact(&mut reply_header)?;

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
            stream.read_exact(&mut buf)?;
        }
        0x03 => {
            // Domain: 1 byte length + domain + 2 port
            let mut len_buf = [0u8; 1];
            stream.read_exact(&mut len_buf)?;
            let mut buf = vec![0u8; len_buf[0] as usize + 2];
            stream.read_exact(&mut buf)?;
        }
        0x04 => {
            // IPv6: 16 bytes + 2 port
            let mut buf = [0u8; 18];
            stream.read_exact(&mut buf)?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

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
    fn handshake_no_auth_ipv4() {
        run_test(
            |server| {
                let mut buf = [0u8; 3];
                server.read_exact(&mut buf).unwrap();
                assert_eq!(buf[0], SOCKS5_VERSION);
                assert_eq!(buf[1], 1);
                assert_eq!(buf[2], AUTH_NONE);

                server.write_all(&[SOCKS5_VERSION, AUTH_NONE]).unwrap();

                let mut connect = [0u8; 256];
                let n = server.read(&mut connect).unwrap();
                assert!(n > 0);

                server.write_all(&ipv4_reply()).unwrap();
            },
            |client| {
                let result = socks5_handshake(client, "example.com", 80, None);
                assert!(result.is_ok());
            },
        );
    }

    #[test]
    fn handshake_with_auth_success() {
        run_test(
            |server| {
                let mut greeting = [0u8; 4];
                server.read_exact(&mut greeting).unwrap();
                assert_eq!(greeting[0], SOCKS5_VERSION);
                assert_eq!(greeting[1], 2); // 2 methods

                server
                    .write_all(&[SOCKS5_VERSION, AUTH_USERNAME_PASSWORD])
                    .unwrap();

                // Read auth sub-negotiation
                let mut auth = [0u8; 256];
                let n = server.read(&mut auth).unwrap();
                assert!(n > 0);
                assert_eq!(auth[0], USERNAME_PASSWORD_VERSION);

                // Auth success
                server.write_all(&[0x01, 0x00]).unwrap();

                // Read CONNECT
                let mut connect = [0u8; 256];
                let _n = server.read(&mut connect).unwrap();

                server.write_all(&ipv4_reply()).unwrap();
            },
            |client| {
                let auth = ProxyAuth {
                    username: "user".into(),
                    password: "pass".into(),
                };
                let result = socks5_handshake(client, "example.com", 80, Some(&auth));
                assert!(result.is_ok());
            },
        );
    }

    #[test]
    fn handshake_auth_failed() {
        run_test(
            |server| {
                let mut buf = [0u8; 4];
                server.read_exact(&mut buf).unwrap();
                server
                    .write_all(&[SOCKS5_VERSION, AUTH_USERNAME_PASSWORD])
                    .unwrap();
                let mut auth = [0u8; 256];
                let _ = server.read(&mut auth).unwrap();
                server.write_all(&[0x01, 0x01]).unwrap(); // auth failed
            },
            |client| {
                let auth = ProxyAuth {
                    username: "user".into(),
                    password: "wrong".into(),
                };
                let err = socks5_handshake(client, "example.com", 80, Some(&auth)).unwrap_err();
                assert!(err.to_string().contains("authentication failed"));
            },
        );
    }

    #[test]
    fn handshake_no_acceptable_method() {
        run_test(
            |server| {
                let mut buf = [0u8; 3];
                server.read_exact(&mut buf).unwrap();
                server
                    .write_all(&[SOCKS5_VERSION, AUTH_NO_ACCEPTABLE])
                    .unwrap();
            },
            |client| {
                let err = socks5_handshake(client, "example.com", 80, None).unwrap_err();
                assert!(
                    err.to_string()
                        .contains("no acceptable authentication method")
                );
            },
        );
    }

    #[test]
    fn handshake_unsupported_auth_method() {
        run_test(
            |server| {
                let mut buf = [0u8; 3];
                server.read_exact(&mut buf).unwrap();
                server.write_all(&[SOCKS5_VERSION, 0x03]).unwrap();
            },
            |client| {
                let err = socks5_handshake(client, "example.com", 80, None).unwrap_err();
                assert!(err.to_string().contains("unsupported auth method"));
            },
        );
    }

    #[test]
    fn handshake_unexpected_version() {
        run_test(
            |server| {
                let mut buf = [0u8; 3];
                server.read_exact(&mut buf).unwrap();
                server.write_all(&[0x04, AUTH_NONE]).unwrap();
            },
            |client| {
                let err = socks5_handshake(client, "example.com", 80, None).unwrap_err();
                assert!(err.to_string().contains("unexpected version"));
            },
        );
    }

    #[test]
    fn handshake_unexpected_reply_version() {
        run_test(
            |server| {
                let mut buf = [0u8; 3];
                server.read_exact(&mut buf).unwrap();
                server.write_all(&[SOCKS5_VERSION, AUTH_NONE]).unwrap();
                let mut connect = [0u8; 256];
                let _ = server.read(&mut connect).unwrap();
                // Bad reply version
                server
                    .write_all(&[0x04, REPLY_SUCCESS, 0x00, 0x01])
                    .unwrap();
                server.write_all(&[127, 0, 0, 1, 0x00, 0x50]).unwrap();
            },
            |client| {
                let err = socks5_handshake(client, "example.com", 80, None).unwrap_err();
                assert!(err.to_string().contains("unexpected reply version"));
            },
        );
    }

    #[test]
    fn handshake_reply_general_failure() {
        run_test(
            |server| {
                let mut buf = [0u8; 3];
                server.read_exact(&mut buf).unwrap();
                server.write_all(&[SOCKS5_VERSION, AUTH_NONE]).unwrap();
                let mut connect = [0u8; 256];
                let _ = server.read(&mut connect).unwrap();
                server
                    .write_all(&[SOCKS5_VERSION, 0x01, 0x00, 0x01])
                    .unwrap();
                server.write_all(&[0, 0, 0, 0, 0, 0]).unwrap();
            },
            |client| {
                let err = socks5_handshake(client, "example.com", 80, None).unwrap_err();
                assert!(err.to_string().contains("general failure"));
            },
        );
    }

    #[test]
    fn handshake_reply_connection_refused() {
        run_test(
            |server| {
                let mut buf = [0u8; 3];
                server.read_exact(&mut buf).unwrap();
                server.write_all(&[SOCKS5_VERSION, AUTH_NONE]).unwrap();
                let mut connect = [0u8; 256];
                let _ = server.read(&mut connect).unwrap();
                server
                    .write_all(&[SOCKS5_VERSION, 0x05, 0x00, 0x01])
                    .unwrap();
                server.write_all(&[0, 0, 0, 0, 0, 0]).unwrap();
            },
            |client| {
                let err = socks5_handshake(client, "example.com", 80, None).unwrap_err();
                assert!(err.to_string().contains("connection refused"));
            },
        );
    }

    #[test]
    fn handshake_reply_unknown_error() {
        run_test(
            |server| {
                let mut buf = [0u8; 3];
                server.read_exact(&mut buf).unwrap();
                server.write_all(&[SOCKS5_VERSION, AUTH_NONE]).unwrap();
                let mut connect = [0u8; 256];
                let _ = server.read(&mut connect).unwrap();
                server
                    .write_all(&[SOCKS5_VERSION, 0x09, 0x00, 0x01])
                    .unwrap();
                server.write_all(&[0, 0, 0, 0, 0, 0]).unwrap();
            },
            |client| {
                let err = socks5_handshake(client, "example.com", 80, None).unwrap_err();
                assert!(err.to_string().contains("unknown error"));
            },
        );
    }

    #[test]
    fn handshake_domain_reply() {
        run_test(
            |server| {
                let mut buf = [0u8; 3];
                server.read_exact(&mut buf).unwrap();
                server.write_all(&[SOCKS5_VERSION, AUTH_NONE]).unwrap();
                let mut connect = [0u8; 256];
                let _ = server.read(&mut connect).unwrap();
                server.write_all(&domain_reply("bound.host")).unwrap();
            },
            |client| {
                let result = socks5_handshake(client, "example.com", 80, None);
                assert!(result.is_ok());
            },
        );
    }

    #[test]
    fn handshake_ipv6_reply() {
        run_test(
            |server| {
                let mut buf = [0u8; 3];
                server.read_exact(&mut buf).unwrap();
                server.write_all(&[SOCKS5_VERSION, AUTH_NONE]).unwrap();
                let mut connect = [0u8; 256];
                let _ = server.read(&mut connect).unwrap();
                server.write_all(&ipv6_reply()).unwrap();
            },
            |client| {
                let result = socks5_handshake(client, "example.com", 80, None);
                assert!(result.is_ok());
            },
        );
    }

    #[test]
    fn handshake_unknown_address_type() {
        run_test(
            |server| {
                let mut buf = [0u8; 3];
                server.read_exact(&mut buf).unwrap();
                server.write_all(&[SOCKS5_VERSION, AUTH_NONE]).unwrap();
                let mut connect = [0u8; 256];
                let _ = server.read(&mut connect).unwrap();
                server
                    .write_all(&[SOCKS5_VERSION, REPLY_SUCCESS, 0x00, 0x05])
                    .unwrap();
            },
            |client| {
                let err = socks5_handshake(client, "example.com", 80, None).unwrap_err();
                assert!(err.to_string().contains("unknown address type"));
            },
        );
    }

    #[test]
    fn handshake_hostname_too_long() {
        let long_host = "a".repeat(256);
        run_test(
            |server| {
                let mut buf = [0u8; 3];
                server.read_exact(&mut buf).unwrap();
                server.write_all(&[SOCKS5_VERSION, AUTH_NONE]).unwrap();
            },
            move |client| {
                let err = socks5_handshake(client, &long_host, 80, None).unwrap_err();
                assert!(err.to_string().contains("hostname too long"));
            },
        );
    }

    #[test]
    fn handshake_auth_required_but_not_provided() {
        run_test(
            |server| {
                let mut buf = [0u8; 3];
                server.read_exact(&mut buf).unwrap();
                server
                    .write_all(&[SOCKS5_VERSION, AUTH_USERNAME_PASSWORD])
                    .unwrap();
            },
            |client| {
                let err = socks5_handshake(client, "example.com", 80, None).unwrap_err();
                assert!(err.to_string().contains("server requires auth"));
            },
        );
    }

    #[test]
    fn handshake_connect_message_format() {
        run_test(
            |server| {
                // Read greeting
                let mut greeting = [0u8; 3];
                server.read_exact(&mut greeting).unwrap();
                server.write_all(&[SOCKS5_VERSION, AUTH_NONE]).unwrap();

                // Read and verify CONNECT message
                let mut connect = [0u8; 256];
                let n = server.read(&mut connect).unwrap();
                let msg = &connect[..n];
                assert_eq!(msg[0], SOCKS5_VERSION);
                assert_eq!(msg[1], CMD_CONNECT);
                assert_eq!(msg[2], 0x00); // reserved
                assert_eq!(msg[3], ATYP_DOMAIN);
                assert_eq!(msg[4], 7); // "test.io" length
                assert_eq!(&msg[5..12], b"test.io");
                // Port 8080 = 0x1F90
                assert_eq!(msg[12], 0x1F);
                assert_eq!(msg[13], 0x90);

                server.write_all(&ipv4_reply()).unwrap();
            },
            |client| {
                let result = socks5_handshake(client, "test.io", 8080, None);
                assert!(result.is_ok());
            },
        );
    }

    #[test]
    fn handshake_auth_subneg_message_format() {
        run_test(
            |server| {
                let mut greeting = [0u8; 4];
                server.read_exact(&mut greeting).unwrap();
                assert_eq!(greeting[0], SOCKS5_VERSION);
                assert_eq!(greeting[1], 2); // 2 methods
                assert_eq!(greeting[2], AUTH_NONE);
                assert_eq!(greeting[3], AUTH_USERNAME_PASSWORD);

                server
                    .write_all(&[SOCKS5_VERSION, AUTH_USERNAME_PASSWORD])
                    .unwrap();

                // Read and verify auth sub-negotiation
                let mut auth_msg = [0u8; 256];
                let n = server.read(&mut auth_msg).unwrap();
                let msg = &auth_msg[..n];
                assert_eq!(msg[0], USERNAME_PASSWORD_VERSION);
                assert_eq!(msg[1], 5); // "admin"
                assert_eq!(&msg[2..7], b"admin");
                assert_eq!(msg[7], 6); // "secret"
                assert_eq!(&msg[8..14], b"secret");

                server.write_all(&[0x01, 0x00]).unwrap();

                // Read CONNECT and reply
                let mut connect = [0u8; 256];
                let _ = server.read(&mut connect).unwrap();
                server.write_all(&ipv4_reply()).unwrap();
            },
            |client| {
                let auth = ProxyAuth {
                    username: "admin".into(),
                    password: "secret".into(),
                };
                let result = socks5_handshake(client, "target.com", 443, Some(&auth));
                assert!(result.is_ok());
            },
        );
    }

    #[test]
    fn handshake_reply_all_error_codes() {
        let codes = [
            (0x02, "connection not allowed by ruleset"),
            (0x03, "network unreachable"),
            (0x04, "host unreachable"),
            (0x06, "TTL expired"),
            (0x07, "command not supported"),
            (0x08, "address type not supported"),
        ];
        for (code, expected_msg) in codes {
            run_test(
                move |server| {
                    let mut buf = [0u8; 3];
                    server.read_exact(&mut buf).unwrap();
                    server.write_all(&[SOCKS5_VERSION, AUTH_NONE]).unwrap();
                    let mut connect = [0u8; 256];
                    let _ = server.read(&mut connect).unwrap();
                    server
                        .write_all(&[SOCKS5_VERSION, code, 0x00, 0x01])
                        .unwrap();
                    server.write_all(&[0, 0, 0, 0, 0, 0]).unwrap();
                },
                move |client| {
                    let err = socks5_handshake(client, "example.com", 80, None).unwrap_err();
                    assert!(
                        err.to_string().contains(expected_msg),
                        "code 0x{code:02x}: expected '{expected_msg}', got '{}'",
                        err
                    );
                },
            );
        }
    }

    #[test]
    fn handshake_eof_during_greeting() {
        run_test(
            |server| {
                let mut buf = [0u8; 3];
                let _ = server.read(&mut buf);
                // connection closes when closure returns
            },
            |client| {
                let err = socks5_handshake(client, "example.com", 80, None).unwrap_err();
                assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
            },
        );
    }
}
