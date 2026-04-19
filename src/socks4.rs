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
        read_buf = hyper::rt::ReadBuf::new(&mut one);
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
    let userid = auth
        .map(|a| a.username.as_bytes())
        .unwrap_or(b"");

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
