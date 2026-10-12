//! A tiny deterministic HTTP/1.1 fixture shared by the non-Tokio examples.
//!
//! It intentionally uses only the standard library so the smol and compio
//! examples exercise their own client runtime rather than borrowing another
//! runtime for the test server.

use std::io::{self, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use std::thread;
use std::time::Duration;

pub struct FixtureServer {
    address: SocketAddr,
    stop: Arc<AtomicBool>,
}

struct Request {
    method: String,
    target: String,
    headers: Vec<(String, String)>,
}

impl FixtureServer {
    pub fn start() -> io::Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        listener.set_nonblocking(true)?;
        let address = listener.local_addr()?;
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let retry_count = Arc::new(AtomicUsize::new(0));

        thread::Builder::new()
            .name("aioduct-observability-fixture".to_owned())
            .spawn(move || {
                while !thread_stop.load(Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            let stop = Arc::clone(&thread_stop);
                            let retry_count = Arc::clone(&retry_count);
                            let _ = thread::Builder::new()
                                .name("aioduct-observability-connection".to_owned())
                                .spawn(move || serve_connection(stream, stop, retry_count));
                        }
                        Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(2));
                        }
                        Err(_) => break,
                    }
                }
            })?;

        Ok(Self { address, stop })
    }

    pub fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{path}", self.address.port())
    }
}

impl Drop for FixtureServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        // Wake the non-blocking accept loop. The connection is immediately
        // closed; active connection threads observe `stop` through their
        // short read timeout.
        let _ = TcpStream::connect(self.address);
    }
}

fn serve_connection(mut stream: TcpStream, stop: Arc<AtomicBool>, retry_count: Arc<AtomicUsize>) {
    let _ = stream.set_read_timeout(Some(Duration::from_millis(100)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
    let mut pending = Vec::new();

    loop {
        let request = match read_request(&mut stream, &mut pending, &stop) {
            Ok(Some(request)) => request,
            Ok(None) | Err(_) => return,
        };
        let path: &str = request
            .target
            .split_once('?')
            .map_or(request.target.as_str(), |(path, _)| path);

        if request.method.eq_ignore_ascii_case("CONNECT") {
            if write_raw(
                &mut stream,
                b"HTTP/1.1 200 OK\r\nConnection: keep-alive\r\n\r\n",
            )
            .is_err()
            {
                return;
            }
            echo_channel(&mut stream, pending, &stop);
            return;
        }

        if path == "/upgrade"
            && header(&request, "upgrade")
                .is_some_and(|value| value.eq_ignore_ascii_case("websocket"))
        {
            if write_raw(
                &mut stream,
                b"HTTP/1.1 101 Switching Protocols\r\nConnection: Upgrade\r\nUpgrade: websocket\r\n\r\n",
            )
            .is_err()
            {
                return;
            }
            echo_channel(&mut stream, pending, &stop);
            return;
        }

        match path {
            "/ok" => {
                if write_response(&mut stream, "200 OK", &[], b"hello from /ok", false).is_err() {
                    return;
                }
            }
            "/redirect" => {
                if write_response(&mut stream, "302 Found", &[("Location", "/ok")], &[], false)
                    .is_err()
                {
                    return;
                }
            }
            "/retry" => {
                let first = retry_count.fetch_add(1, Ordering::SeqCst) == 0;
                let (status, body) = if first {
                    ("503 Service Unavailable", b"retry once".as_slice())
                } else {
                    ("200 OK", b"retry succeeded".as_slice())
                };
                if write_response(&mut stream, status, &[], body, false).is_err() {
                    return;
                }
            }
            "/upload" => {
                let body_len = content_length(&request).unwrap_or(0);
                if body_len > pending.len() {
                    let mut body = vec![0; body_len - pending.len()];
                    if read_exact_with_stop(&mut stream, &mut body, &stop).is_err() {
                        return;
                    }
                }
                pending.clear();
                let body = format!("server received {body_len} upload bytes");
                if write_response(&mut stream, "200 OK", &[], body.as_bytes(), false).is_err() {
                    return;
                }
            }
            "/hang" => {
                while !stop.load(Ordering::Relaxed) {
                    thread::sleep(Duration::from_millis(10));
                }
                return;
            }
            "/body-error" => {
                let _ = write_raw(
                    &mut stream,
                    b"HTTP/1.1 200 OK\r\nContent-Length: 1024\r\nConnection: close\r\n\r\nshort body",
                );
                let _ = stream.shutdown(Shutdown::Both);
                return;
            }
            "/sse" => {
                let body = b": a comment\nretry: 250\n\nevent: greeting\ndata: hello\nid: first\n\ndata: world\n\n";
                if write_response(
                    &mut stream,
                    "200 OK",
                    &[
                        ("Content-Type", "text/event-stream"),
                        ("Cache-Control", "no-cache"),
                    ],
                    body,
                    false,
                )
                .is_err()
                {
                    return;
                }
            }
            "/status/204" => {
                if write_response(&mut stream, "204 No Content", &[], &[], false).is_err() {
                    return;
                }
            }
            _ => {
                if write_response(&mut stream, "404 Not Found", &[], b"not found", false).is_err() {
                    return;
                }
            }
        }
    }
}

fn read_request(
    stream: &mut TcpStream,
    pending: &mut Vec<u8>,
    stop: &AtomicBool,
) -> io::Result<Option<Request>> {
    let header_end = loop {
        if let Some(end) = pending.windows(4).position(|window| window == b"\r\n\r\n") {
            break end + 4;
        }
        let mut buffer = [0_u8; 4096];
        match stream.read(&mut buffer) {
            Ok(0) => return Ok(None),
            Ok(read) => pending.extend_from_slice(&buffer[..read]),
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) && !stop.load(Ordering::Relaxed) =>
            {
                continue;
            }
            Err(error) => return Err(error),
        }
    };

    let header_bytes = pending[..header_end].to_vec();
    let header_text = String::from_utf8_lossy(&header_bytes);
    let mut lines = header_text.split("\r\n");
    let Some(request_line) = lines.next() else {
        return Ok(None);
    };
    let mut parts = request_line.split_whitespace();
    let Some(method) = parts.next() else {
        return Ok(None);
    };
    let Some(target) = parts.next() else {
        return Ok(None);
    };
    let headers = lines
        .filter_map(|line| {
            let (name, value) = line.split_once(':')?;
            Some((name.to_ascii_lowercase(), value.trim().to_owned()))
        })
        .collect();
    pending.drain(..header_end);

    Ok(Some(Request {
        method: method.to_owned(),
        target: target.to_owned(),
        headers,
    }))
}

fn read_exact_with_stop(
    stream: &mut TcpStream,
    buffer: &mut [u8],
    stop: &AtomicBool,
) -> io::Result<()> {
    let mut offset = 0;
    while offset < buffer.len() {
        match stream.read(&mut buffer[offset..]) {
            Ok(0) => return Err(io::Error::from(io::ErrorKind::UnexpectedEof)),
            Ok(read) => offset += read,
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) && !stop.load(Ordering::Relaxed) =>
            {
                continue;
            }
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn write_response(
    stream: &mut TcpStream,
    status: &str,
    extra_headers: &[(&str, &str)],
    body: &[u8],
    close: bool,
) -> io::Result<()> {
    let mut response = format!(
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: {}\r\n",
        body.len(),
        if close { "close" } else { "keep-alive" }
    );
    for (name, value) in extra_headers {
        response.push_str(name);
        response.push_str(": ");
        response.push_str(value);
        response.push_str("\r\n");
    }
    response.push_str("\r\n");
    write_raw(stream, response.as_bytes())?;
    write_raw(stream, body)
}

fn write_raw(stream: &mut TcpStream, bytes: &[u8]) -> io::Result<()> {
    stream.write_all(bytes)
}

fn echo_channel(stream: &mut TcpStream, mut pending: Vec<u8>, stop: &AtomicBool) {
    if !pending.is_empty() {
        let mut reply = b"echo: ".to_vec();
        reply.append(&mut pending);
        if write_raw(stream, &reply).is_err() {
            return;
        }
    }
    let mut buffer = [0_u8; 4096];
    while !stop.load(Ordering::Relaxed) {
        match stream.read(&mut buffer) {
            Ok(0) => return,
            Ok(read) => {
                let mut reply = b"echo: ".to_vec();
                reply.extend_from_slice(&buffer[..read]);
                if write_raw(stream, &reply).is_err() {
                    return;
                }
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                continue;
            }
            Err(_) => return,
        }
    }
}

fn header<'a>(request: &'a Request, name: &str) -> Option<&'a str> {
    request
        .headers
        .iter()
        .find(|(header_name, _)| header_name == name)
        .map(|(_, value)| value.as_str())
}

fn content_length(request: &Request) -> Option<usize> {
    header(request, "content-length")?.parse().ok()
}
