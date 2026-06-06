use std::io::{self};
#[cfg(test)]
use std::io::{Read, Write};
use std::net::IpAddr;
#[cfg(test)]
use std::net::{TcpStream, ToSocketAddrs};

use crate::proxy::ProxyAuth;

const SOCKS5_VERSION: u8 = 0x05;
const AUTH_NONE: u8 = 0x00;
const AUTH_USERNAME_PASSWORD: u8 = 0x02;
const AUTH_NO_ACCEPTABLE: u8 = 0xFF;
const CMD_CONNECT: u8 = 0x01;
const ATYP_IPV4: u8 = 0x01;
const ATYP_DOMAIN: u8 = 0x03;
const ATYP_IPV6: u8 = 0x04;
const REPLY_SUCCESS: u8 = 0x00;
const USERNAME_PASSWORD_VERSION: u8 = 0x01;

/// Whether the SOCKS5 proxy or the client resolves the target hostname.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Socks5Dns {
    /// Client resolves DNS locally and sends the IP address to the proxy.
    Local,
    /// Proxy resolves DNS (SOCKS5h behavior) — client sends the hostname.
    Remote,
}

#[cfg(test)]
pub(crate) fn socks5_handshake(
    stream: &mut TcpStream,
    host: &str,
    port: u16,
    auth: Option<&ProxyAuth>,
    dns: Socks5Dns,
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

    let mut connect_msg = Vec::with_capacity(32);
    connect_msg.push(SOCKS5_VERSION);
    connect_msg.push(CMD_CONNECT);
    connect_msg.push(0x00); // reserved

    match dns {
        Socks5Dns::Local => {
            let addr = resolve_host(host, port)?;
            match addr {
                IpAddr::V4(v4) => {
                    connect_msg.push(ATYP_IPV4);
                    connect_msg.extend_from_slice(&v4.octets());
                }
                IpAddr::V6(v6) => {
                    connect_msg.push(ATYP_IPV6);
                    connect_msg.extend_from_slice(&v6.octets());
                }
            }
        }
        Socks5Dns::Remote => {
            let host_bytes = host.as_bytes();
            if host_bytes.len() > 255 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "SOCKS5: hostname too long",
                ));
            }
            connect_msg.push(ATYP_DOMAIN);
            connect_msg.push(host_bytes.len() as u8);
            connect_msg.extend_from_slice(host_bytes);
        }
    }
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

/// Async version of [`socks5_handshake`] that works on any hyper-compatible stream.
///
/// Unlike the blocking version, this avoids `into_std_tcp`/`from_std_tcp` round-trips
/// that can cause TCP resets with some SOCKS5 proxies (e.g., Clash).
///
/// For `Socks5Dns::Local`, the caller must pre-resolve and pass `Some(addr)`.
pub(crate) async fn socks5_handshake_async<S>(
    stream: &mut S,
    host: &str,
    port: u16,
    auth: Option<&ProxyAuth>,
    dns: Socks5Dns,
    resolved_addr: Option<IpAddr>,
) -> io::Result<()>
where
    S: hyper::rt::Read + hyper::rt::Write + Unpin,
{
    use std::future::poll_fn;
    use std::pin::Pin;

    // Helper: async write_all
    async fn write_all<S: hyper::rt::Write + Unpin>(stream: &mut S, buf: &[u8]) -> io::Result<()> {
        let mut written = 0;
        while written < buf.len() {
            let n = poll_fn(|cx| Pin::new(&mut *stream).poll_write(cx, &buf[written..])).await?;
            if n == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "SOCKS5: proxy closed connection",
                ));
            }
            written += n;
        }
        poll_fn(|cx| Pin::new(&mut *stream).poll_flush(cx)).await?;
        Ok(())
    }

    // Helper: async read_exact
    async fn read_exact<S: hyper::rt::Read + Unpin>(
        stream: &mut S,
        buf: &mut [u8],
    ) -> io::Result<()> {
        let mut read = 0;
        while read < buf.len() {
            let mut remaining = hyper::rt::ReadBuf::new(&mut buf[read..]);
            poll_fn(|cx| Pin::new(&mut *stream).poll_read(cx, remaining.unfilled())).await?;
            let n = remaining.filled().len();
            if n == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "SOCKS5: proxy closed connection",
                ));
            }
            read += n;
        }
        Ok(())
    }

    // 1. Method negotiation
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

    // 2. CONNECT request
    let mut connect_msg = Vec::with_capacity(32);
    connect_msg.push(SOCKS5_VERSION);
    connect_msg.push(CMD_CONNECT);
    connect_msg.push(0x00);

    match dns {
        Socks5Dns::Local => {
            let addr = resolved_addr.ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "SOCKS5: resolved_addr required for Socks5Dns::Local",
                )
            })?;
            match addr {
                IpAddr::V4(v4) => {
                    connect_msg.push(ATYP_IPV4);
                    connect_msg.extend_from_slice(&v4.octets());
                }
                IpAddr::V6(v6) => {
                    connect_msg.push(ATYP_IPV6);
                    connect_msg.extend_from_slice(&v6.octets());
                }
            }
        }
        Socks5Dns::Remote => {
            let host_bytes = host.as_bytes();
            if host_bytes.len() > 255 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "SOCKS5: hostname too long",
                ));
            }
            connect_msg.push(ATYP_DOMAIN);
            connect_msg.push(host_bytes.len() as u8);
            connect_msg.extend_from_slice(host_bytes);
        }
    }
    connect_msg.push((port >> 8) as u8);
    connect_msg.push(port as u8);
    write_all(stream, &connect_msg).await?;

    // 3. Read reply
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
            let mut buf = [0u8; 6];
            read_exact(stream, &mut buf).await?;
        }
        0x03 => {
            let mut len_buf = [0u8; 1];
            read_exact(stream, &mut len_buf).await?;
            let mut buf = vec![0u8; len_buf[0] as usize + 2];
            read_exact(stream, &mut buf).await?;
        }
        0x04 => {
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

#[cfg(test)]
fn resolve_host(host: &str, port: u16) -> io::Result<IpAddr> {
    let addr = (host, port)
        .to_socket_addrs()?
        .next()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "failed to resolve host"))?;
    Ok(addr.ip())
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
                let result = socks5_handshake(client, "example.com", 80, None, Socks5Dns::Remote);
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
                let result =
                    socks5_handshake(client, "example.com", 80, Some(&auth), Socks5Dns::Remote);
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
                let err =
                    socks5_handshake(client, "example.com", 80, Some(&auth), Socks5Dns::Remote)
                        .unwrap_err();
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
                let err = socks5_handshake(client, "example.com", 80, None, Socks5Dns::Remote)
                    .unwrap_err();
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
                let err = socks5_handshake(client, "example.com", 80, None, Socks5Dns::Remote)
                    .unwrap_err();
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
                let err = socks5_handshake(client, "example.com", 80, None, Socks5Dns::Remote)
                    .unwrap_err();
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
                let err = socks5_handshake(client, "example.com", 80, None, Socks5Dns::Remote)
                    .unwrap_err();
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
                let err = socks5_handshake(client, "example.com", 80, None, Socks5Dns::Remote)
                    .unwrap_err();
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
                let err = socks5_handshake(client, "example.com", 80, None, Socks5Dns::Remote)
                    .unwrap_err();
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
                let err = socks5_handshake(client, "example.com", 80, None, Socks5Dns::Remote)
                    .unwrap_err();
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
                let result = socks5_handshake(client, "example.com", 80, None, Socks5Dns::Remote);
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
                let result = socks5_handshake(client, "example.com", 80, None, Socks5Dns::Remote);
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
                let err = socks5_handshake(client, "example.com", 80, None, Socks5Dns::Remote)
                    .unwrap_err();
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
                let err =
                    socks5_handshake(client, &long_host, 80, None, Socks5Dns::Remote).unwrap_err();
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
                let err = socks5_handshake(client, "example.com", 80, None, Socks5Dns::Remote)
                    .unwrap_err();
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
                let result = socks5_handshake(client, "test.io", 8080, None, Socks5Dns::Remote);
                assert!(result.is_ok());
            },
        );
    }

    #[test]
    fn handshake_connect_message_format_local_dns() {
        run_test(
            |server| {
                // Read greeting
                let mut greeting = [0u8; 3];
                server.read_exact(&mut greeting).unwrap();
                server.write_all(&[SOCKS5_VERSION, AUTH_NONE]).unwrap();

                // Read and verify CONNECT message with local DNS resolution
                let mut connect = [0u8; 256];
                let n = server.read(&mut connect).unwrap();
                let msg = &connect[..n];
                assert_eq!(msg[0], SOCKS5_VERSION);
                assert_eq!(msg[1], CMD_CONNECT);
                assert_eq!(msg[2], 0x00); // reserved

                match msg[3] {
                    ATYP_IPV4 => {
                        assert_eq!(&msg[4..8], &[127, 0, 0, 1]); // localhost IPv4
                        assert_eq!(msg[8], 0x1F);
                        assert_eq!(msg[9], 0x90); // port 8080
                    }
                    ATYP_IPV6 => {
                        // ::1
                        assert_eq!(
                            &msg[4..20],
                            &[0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]
                        );
                        assert_eq!(msg[20], 0x1F);
                        assert_eq!(msg[21], 0x90); // port 8080
                    }
                    other => panic!("unexpected ATYP: 0x{other:02x}"),
                }

                server.write_all(&ipv4_reply()).unwrap();
            },
            |client| {
                let result = socks5_handshake(client, "localhost", 8080, None, Socks5Dns::Local);
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
                let result =
                    socks5_handshake(client, "target.com", 443, Some(&auth), Socks5Dns::Remote);
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
                    let err = socks5_handshake(client, "example.com", 80, None, Socks5Dns::Remote)
                        .unwrap_err();
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
                let err = socks5_handshake(client, "example.com", 80, None, Socks5Dns::Remote)
                    .unwrap_err();
                assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
            },
        );
    }

    #[test]
    fn handshake_respects_read_timeout() {
        use std::time::{Duration, Instant};

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();

        // Server accepts but never responds — simulates a hung proxy.
        let _server = std::thread::spawn(move || {
            let (_stream, _) = listener.accept().unwrap();
            std::thread::sleep(Duration::from_secs(10));
        });

        let mut client = TcpStream::connect(addr).unwrap();
        client
            .set_read_timeout(Some(Duration::from_millis(100)))
            .unwrap();
        client
            .set_write_timeout(Some(Duration::from_millis(100)))
            .unwrap();

        let start = Instant::now();
        let err =
            socks5_handshake(&mut client, "example.com", 80, None, Socks5Dns::Remote).unwrap_err();
        let elapsed = start.elapsed();

        assert!(
            elapsed < Duration::from_secs(2),
            "handshake should have timed out quickly, took {elapsed:?}"
        );
        assert!(
            err.kind() == io::ErrorKind::WouldBlock || err.kind() == io::ErrorKind::TimedOut,
            "expected timeout error, got: {err:?}"
        );
    }

    #[test]
    fn resolve_host_localhost() {
        // localhost resolution should always work and return loopback
        let ip = resolve_host("localhost", 80).unwrap();
        assert!(
            ip.is_loopback(),
            "localhost should resolve to loopback, got {ip}"
        );
    }

    #[test]
    fn resolve_host_unknown_fails() {
        // A clearly invalid hostname should fail
        let result = resolve_host("invalid.invalid.invalid.test", 80);
        assert!(result.is_err(), "invalid hostname should fail to resolve");
    }
}
