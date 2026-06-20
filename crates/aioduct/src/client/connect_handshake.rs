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
    for (name, value) in &proxy.connect_headers {
        let value_str = value.to_str().map_err(|e| {
            Error::InvalidHeader(format!(
                "proxy CONNECT header `{}` is not valid HTTP/1 text: {e}",
                name.as_str()
            ))
        })?;
        connect_msg.push_str(&format!("{}: {value_str}\r\n", name.as_str()));
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
