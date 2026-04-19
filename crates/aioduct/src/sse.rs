use bytes::{Buf, BytesMut};
use http_body_util::BodyExt;

use crate::error::{Error, HyperBody};

/// A parsed Server-Sent Event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseEvent {
    /// Event type (from the `event:` field).
    pub event: Option<String>,
    /// Event payload (from the `data:` field).
    pub data: String,
    /// Event ID (from the `id:` field).
    pub id: Option<String>,
    /// Suggested reconnect delay in milliseconds (from the `retry:` field).
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
    pub async fn next(&mut self) -> Option<Result<SseEvent, Error>> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_simple_data_event() {
        let mut buf = BytesMut::from("data: hello\n\n");
        let event = try_parse_event(&mut buf).unwrap();
        assert_eq!(event.data, "hello");
        assert!(event.event.is_none());
        assert!(event.id.is_none());
        assert!(event.retry.is_none());
    }

    #[test]
    fn parse_event_with_type() {
        let mut buf = BytesMut::from("event: update\ndata: payload\n\n");
        let event = try_parse_event(&mut buf).unwrap();
        assert_eq!(event.event.as_deref(), Some("update"));
        assert_eq!(event.data, "payload");
    }

    #[test]
    fn parse_event_with_id() {
        let mut buf = BytesMut::from("id: 42\ndata: msg\n\n");
        let event = try_parse_event(&mut buf).unwrap();
        assert_eq!(event.id.as_deref(), Some("42"));
        assert_eq!(event.data, "msg");
    }

    #[test]
    fn parse_event_with_retry() {
        let mut buf = BytesMut::from("retry: 3000\ndata: reconnect\n\n");
        let event = try_parse_event(&mut buf).unwrap();
        assert_eq!(event.retry, Some(3000));
    }

    #[test]
    fn parse_multiline_data() {
        let mut buf = BytesMut::from("data: line1\ndata: line2\ndata: line3\n\n");
        let event = try_parse_event(&mut buf).unwrap();
        assert_eq!(event.data, "line1\nline2\nline3");
    }

    #[test]
    fn parse_comment_ignored() {
        let mut buf = BytesMut::from(": this is a comment\ndata: actual\n\n");
        let event = try_parse_event(&mut buf).unwrap();
        assert_eq!(event.data, "actual");
    }

    #[test]
    fn parse_crlf_boundary() {
        let mut buf = BytesMut::from("data: crlf\r\n\r\n");
        let event = try_parse_event(&mut buf).unwrap();
        assert_eq!(event.data, "crlf");
    }

    #[test]
    fn parse_data_without_space_after_colon() {
        let mut buf = BytesMut::from("data:nospace\n\n");
        let event = try_parse_event(&mut buf).unwrap();
        assert_eq!(event.data, "nospace");
    }

    #[test]
    fn parse_data_field_only_name() {
        let mut buf = BytesMut::from("data\n\n");
        let event = try_parse_event(&mut buf).unwrap();
        assert_eq!(event.data, "");
    }

    #[test]
    fn parse_event_field_only_name() {
        let mut buf = BytesMut::from("event\n\n");
        let event = try_parse_event(&mut buf).unwrap();
        assert_eq!(event.event.as_deref(), Some(""));
        assert_eq!(event.data, "");
    }

    #[test]
    fn parse_id_field_only_name() {
        let mut buf = BytesMut::from("id\n\n");
        let event = try_parse_event(&mut buf).unwrap();
        assert_eq!(event.id.as_deref(), Some(""));
    }

    #[test]
    fn parse_unknown_field_ignored() {
        let mut buf = BytesMut::from("unknown: val\ndata: real\n\n");
        let event = try_parse_event(&mut buf).unwrap();
        assert_eq!(event.data, "real");
    }

    #[test]
    fn no_event_without_double_newline() {
        let mut buf = BytesMut::from("data: incomplete\n");
        assert!(try_parse_event(&mut buf).is_none());
    }

    #[test]
    fn empty_block_returns_none() {
        let mut buf = BytesMut::from("\n\n");
        assert!(try_parse_event(&mut buf).is_none());
    }

    #[test]
    fn find_event_boundary_lf_lf() {
        assert_eq!(find_event_boundary(b"data: x\n\nrest"), Some(7));
    }

    #[test]
    fn find_event_boundary_crlf_crlf() {
        assert_eq!(find_event_boundary(b"data: x\r\n\r\nrest"), Some(7));
    }

    #[test]
    fn find_event_boundary_mixed() {
        assert_eq!(find_event_boundary(b"data: x\r\n\nrest"), Some(7));
    }

    #[test]
    fn find_event_boundary_none() {
        assert_eq!(find_event_boundary(b"data: x\n"), None);
    }

    #[test]
    fn skip_newlines_lf() {
        assert_eq!(skip_newlines(b"\n\nrest"), 2);
    }

    #[test]
    fn skip_newlines_crlf() {
        assert_eq!(skip_newlines(b"\r\n\r\nrest"), 4);
    }

    #[test]
    fn skip_newlines_mixed() {
        assert_eq!(skip_newlines(b"\r\n\nrest"), 3);
    }

    #[test]
    fn skip_newlines_none() {
        assert_eq!(skip_newlines(b"rest"), 0);
    }

    #[test]
    fn parse_full_event() {
        let mut buf = BytesMut::from("id: 1\nevent: message\nretry: 5000\ndata: hello world\n\n");
        let event = try_parse_event(&mut buf).unwrap();
        assert_eq!(event.id.as_deref(), Some("1"));
        assert_eq!(event.event.as_deref(), Some("message"));
        assert_eq!(event.retry, Some(5000));
        assert_eq!(event.data, "hello world");
    }

    #[test]
    fn parse_two_events_sequentially() {
        let mut buf = BytesMut::from("data: first\n\ndata: second\n\n");
        let e1 = try_parse_event(&mut buf).unwrap();
        assert_eq!(e1.data, "first");
        let e2 = try_parse_event(&mut buf).unwrap();
        assert_eq!(e2.data, "second");
    }

    #[test]
    fn retry_non_numeric_ignored() {
        let mut buf = BytesMut::from("retry: abc\ndata: x\n\n");
        let event = try_parse_event(&mut buf).unwrap();
        assert!(event.retry.is_none());
    }
}
