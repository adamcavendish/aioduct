use std::pin::Pin;

use crate::error::Error;
use crate::proxy::ProxyConfig;

const MAX_CONNECT_RESPONSE_HEAD: usize = 8192;
const MAX_CONNECT_RESPONSE_HEADERS: usize = 64;
const MAX_CONNECT_INFORMATIONAL_RESPONSES: usize = 16;

/// Perform an HTTP CONNECT handshake through `stream` to `target`.
///
/// Sends `CONNECT target HTTP/1.1`, reads the response, validates a successful
/// HTTP/1.x status, and returns the stream unchanged on success.
/// Type-preserving: the returned stream is the same `S` that was passed in,
/// making it reusable for proxy chaining.
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

    let mut informational_responses = 0;
    loop {
        let mut resp_buf = Vec::with_capacity(256);
        loop {
            let mut one = [0u8; 1];
            let mut read_buf = hyper::rt::ReadBuf::new(&mut one);
            std::future::poll_fn(|cx| Pin::new(&mut stream).poll_read(cx, read_buf.unfilled()))
                .await
                .map_err(Error::Io)?;

            if read_buf.filled().is_empty() {
                return Err(Error::Io(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "proxy closed connection during CONNECT handshake",
                )));
            }
            resp_buf.push(one[0]);

            if resp_buf.len() > MAX_CONNECT_RESPONSE_HEAD {
                return Err(Error::Other("CONNECT response too large".into()));
            }

            if resp_buf.len() >= 4 && resp_buf[resp_buf.len() - 4..] == *b"\r\n\r\n" {
                break;
            }
        }

        let status_code = parse_connect_response(&resp_buf)?;
        if status_code == 101 {
            return Err(Error::Other(
                "CONNECT proxy switched protocols with status 101 before establishing a tunnel"
                    .into(),
            ));
        }
        if (100..=199).contains(&status_code) {
            informational_responses += 1;
            if informational_responses > MAX_CONNECT_INFORMATIONAL_RESPONSES {
                return Err(Error::Other(
                    "too many informational CONNECT responses".into(),
                ));
            }
            continue;
        }
        if !(200..=299).contains(&status_code) {
            return Err(Error::Other(
                format!("CONNECT tunnel failed with status {status_code}").into(),
            ));
        }
        return Ok(stream);
    }
}

pub(crate) fn parse_connect_response(response_head: &[u8]) -> Result<u16, Error> {
    if !response_head.ends_with(b"\r\n\r\n")
        || response_head.iter().enumerate().any(|(index, byte)| {
            (*byte == b'\n' && (index == 0 || response_head[index - 1] != b'\r'))
                || (*byte == b'\r' && response_head.get(index + 1).copied() != Some(b'\n'))
        })
    {
        return Err(Error::Other(
            "malformed CONNECT response: lines must use CRLF framing".into(),
        ));
    }

    let mut headers = [httparse::EMPTY_HEADER; MAX_CONNECT_RESPONSE_HEADERS];
    let mut response = httparse::Response::new(&mut headers);
    let parsed = response
        .parse(response_head)
        .map_err(|error| Error::Other(format!("malformed CONNECT response: {error}").into()))?;

    match parsed {
        httparse::Status::Complete(length) if length == response_head.len() => {}
        httparse::Status::Complete(_) => {
            return Err(Error::Other(
                "malformed CONNECT response: bytes follow the header section".into(),
            ));
        }
        httparse::Status::Partial => {
            return Err(Error::Other("incomplete CONNECT response".into()));
        }
    }

    if !matches!(response.version, Some(0 | 1)) {
        return Err(Error::Other(
            "CONNECT proxy must respond with HTTP/1.0 or HTTP/1.1".into(),
        ));
    }

    response
        .code
        .ok_or_else(|| Error::Other("CONNECT response is missing a status code".into()))
}
