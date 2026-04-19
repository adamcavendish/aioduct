use bytes::{Buf, BytesMut};
use http_body_util::BodyExt;

use crate::error::{HyperBody, Result};

/// A parsed Server-Sent Event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseEvent {
    /// Event type.
    pub event: Option<String>,
    /// Event payload.
    pub data: String,
    /// Event ID.
    pub id: Option<String>,
    /// Suggested reconnect delay in milliseconds.
    pub retry: Option<u64>,
}

/// Async iterator over a `text/event-stream` response body.
pub struct SseStream {
    body: HyperBody,
    buf: BytesMut,
    done: bool,
}

impl SseStream {
    pub(crate) fn new(body: HyperBody) -> Self {
        Self {
            body,
            buf: BytesMut::new(),
            done: false,
        }
    }

    /// Returns the next SSE event, or `None` when the stream ends.
    pub async fn next(&mut self) -> Option<Result<SseEvent>> {
        loop {
            if let Some(event) = try_parse_event(&mut self.buf) {
                return Some(Ok(event));
            }

            if self.done {
                return None;
            }

            match self.body.frame().await {
                Some(Ok(frame)) => {
                    if let Ok(data) = frame.into_data() {
                        self.buf.extend_from_slice(&data);
                    }
                }
                Some(Err(e)) => return Some(Err(e)),
                None => {
                    self.done = true;
                    if let Some(event) = try_parse_event(&mut self.buf) {
                        return Some(Ok(event));
                    }
                    return None;
                }
            }
        }
    }
}

fn try_parse_event(buf: &mut BytesMut) -> Option<SseEvent> {
    let separator = find_event_boundary(&buf[..])?;

    let block = &buf[..separator];
    let block_str = std::str::from_utf8(block).ok()?;

    let mut event_type = None;
    let mut data_lines = Vec::new();
    let mut id = None;
    let mut retry = None;

    for line in block_str.lines() {
        if line.is_empty() || line.starts_with(':') {
            continue;
        }

        if let Some((field, value)) = line.split_once(':') {
            let value = value.strip_prefix(' ').unwrap_or(value);
            match field {
                "event" => event_type = Some(value.to_owned()),
                "data" => data_lines.push(value.to_owned()),
                "id" => id = Some(value.to_owned()),
                "retry" => retry = value.parse().ok(),
                _ => {}
            }
        } else {
            match line {
                "data" => data_lines.push(String::new()),
                "event" => event_type = Some(String::new()),
                "id" => id = Some(String::new()),
                _ => {}
            }
        }
    }

    let consume = separator + skip_newlines(&buf[separator..]);
    buf.advance(consume);

    if data_lines.is_empty() && event_type.is_none() && id.is_none() && retry.is_none() {
        return None;
    }

    Some(SseEvent {
        event: event_type,
        data: data_lines.join("\n"),
        id,
        retry,
    })
}

fn find_event_boundary(bytes: &[u8]) -> Option<usize> {
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\n' {
            if i + 1 < bytes.len() && bytes[i + 1] == b'\n' {
                return Some(i);
            }
            if i + 2 < bytes.len() && bytes[i + 1] == b'\r' && bytes[i + 2] == b'\n' {
                return Some(i);
            }
        }
        if bytes[i] == b'\r' && i + 1 < bytes.len() && bytes[i + 1] == b'\n' {
            if i + 3 < bytes.len() && bytes[i + 2] == b'\r' && bytes[i + 3] == b'\n' {
                return Some(i);
            }
            if i + 2 < bytes.len() && bytes[i + 2] == b'\n' {
                return Some(i);
            }
        }
        i += 1;
    }
    None
}

fn skip_newlines(bytes: &[u8]) -> usize {
    let mut i = 0;
    while i < bytes.len() {
        if i + 1 < bytes.len() && bytes[i] == b'\r' && bytes[i + 1] == b'\n' {
            i += 2;
        } else if bytes[i] == b'\n' {
            i += 1;
        } else {
            break;
        }
    }
    i
}
