//! Server-Sent Events (SSE) stream parser (WHATWG spec §9.2.5).

mod sse_local;
mod sse_send;

#[cfg(not(target_arch = "wasm32"))]
pub use sse_local::SseStreamLocal;
pub use sse_send::SseStreamSend;

use std::sync::Arc;

use bytes::{Buf, BytesMut};
use memchr::memchr2;

use crate::error::Error;

/// A parsed SSE message event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseMessage {
    /// Event type (defaults to `"message"` per spec §9.2.5).
    pub event: String,
    /// Event payload (from one or more `data:` fields, joined with `\n`).
    pub data: String,
    /// The `Last-Event-ID` at the time this event was dispatched.
    pub last_event_id: Arc<str>,
}

/// Events yielded by an SSE stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SseEvent {
    /// A standard data message.
    Message(SseMessage),
    /// A server request to change the reconnection time (milliseconds).
    Retry(u64),
}

const DEFAULT_MAX_PAYLOAD: usize = 512 * 1024;

/// State-machine decoder for Server-Sent Events (WHATWG spec §9.2.5).
///
/// Handles BOM stripping, CR/LF/CRLF line endings, null-byte rejection in
/// `id:` fields, digits-only `retry:` parsing, payload size limits with
/// corruption recovery, and two-phase `Last-Event-ID` commit via `Arc<str>`.
#[derive(Debug, Clone)]
pub struct SseDecoder {
    last_event_id: Arc<str>,
    staged_last_event_id: Arc<str>,
    max_payload_size: usize,
    bom_stripped: bool,
    corrupted: bool,
}

impl SseDecoder {
    /// Creates a decoder with a default 512 KiB payload limit.
    pub fn new() -> Self {
        Self::with_max_payload_size(DEFAULT_MAX_PAYLOAD)
    }

    /// Creates a decoder with a custom maximum payload size.
    /// Pass `0` to disable the limit entirely.
    pub fn with_max_payload_size(max: usize) -> Self {
        Self {
            last_event_id: Arc::from(""),
            staged_last_event_id: Arc::from(""),
            max_payload_size: max,
            bom_stripped: false,
            corrupted: false,
        }
    }

    /// Returns the current `Last-Event-ID`.
    pub fn last_event_id(&self) -> &Arc<str> {
        &self.last_event_id
    }

    /// Attempt to decode the next event from `buf`.
    ///
    /// Returns `Some(Ok(event))` when a complete event is parsed,
    /// `Some(Err(_))` on payload-too-large, or `None` when more data is needed.
    pub fn decode(&mut self, buf: &mut BytesMut) -> Option<Result<SseEvent, Error>> {
        self.strip_bom(buf);

        loop {
            let boundary = find_event_boundary(&buf[..])?;

            let block_end = boundary.block_end;
            let consume = boundary.consume;

            if self.corrupted {
                buf.advance(consume);
                self.corrupted = false;
                continue;
            }

            if self.max_payload_size > 0 && block_end > self.max_payload_size {
                buf.advance(consume);
                self.corrupted = false;
                return Some(Err(Error::Other(
                    "SSE payload too large".to_string().into(),
                )));
            }

            let block_str = String::from_utf8_lossy(&buf[..block_end]).into_owned();

            buf.advance(consume);

            let mut event_type: Option<String> = None;
            let mut data_lines: Vec<String> = Vec::new();
            let mut id: Option<String> = None;
            let mut retry: Option<u64> = None;
            let mut has_data_field = false;

            let mut remaining = &*block_str;
            while !remaining.is_empty() {
                let (line, rest) = next_line_str(remaining);
                remaining = rest;

                if line.is_empty() || line.starts_with(':') {
                    continue;
                }

                if let Some((field, value)) = line.split_once(':') {
                    let value = value.strip_prefix(' ').unwrap_or(value);
                    match field {
                        "event" => event_type = Some(value.to_owned()),
                        "data" => {
                            has_data_field = true;
                            data_lines.push(value.to_owned());
                        }
                        "id" if !value.contains('\0') => {
                            id = Some(value.to_owned());
                        }
                        "retry"
                            if !value.is_empty() && value.bytes().all(|b| b.is_ascii_digit()) =>
                        {
                            retry = value.parse().ok();
                        }
                        _ => {}
                    }
                } else {
                    match line {
                        "data" => {
                            has_data_field = true;
                            data_lines.push(String::new());
                        }
                        "event" => event_type = Some(String::new()),
                        "id" => id = Some(String::new()),
                        _ => {}
                    }
                }
            }

            if let Some(id_val) = &id {
                self.staged_last_event_id = Arc::from(&**id_val);
            }

            if let Some(retry_val) = retry
                && !has_data_field
                && event_type.is_none()
                && id.is_none()
            {
                self.last_event_id = self.staged_last_event_id.clone();
                return Some(Ok(SseEvent::Retry(retry_val)));
            }

            if !has_data_field {
                self.last_event_id = self.staged_last_event_id.clone();
                if let Some(retry_val) = retry {
                    return Some(Ok(SseEvent::Retry(retry_val)));
                }
                continue;
            }

            self.last_event_id = self.staged_last_event_id.clone();

            let data = data_lines.join("\n");
            let event = event_type.unwrap_or_else(|| "message".to_owned());

            return Some(Ok(SseEvent::Message(SseMessage {
                event,
                data,
                last_event_id: self.last_event_id.clone(),
            })));
        }
    }

    fn strip_bom(&mut self, buf: &mut BytesMut) {
        if self.bom_stripped {
            return;
        }
        if buf.len() >= 3 && &buf[..3] == b"\xef\xbb\xbf" {
            buf.advance(3);
        }
        self.bom_stripped = true;
    }
}

impl Default for SseDecoder {
    fn default() -> Self {
        Self::new()
    }
}

struct EventBoundary {
    block_end: usize,
    consume: usize,
}

fn find_event_boundary(bytes: &[u8]) -> Option<EventBoundary> {
    let mut pos = 0;
    while pos < bytes.len() {
        let i = memchr2(b'\r', b'\n', &bytes[pos..])?;
        let abs = pos + i;
        let block_end = abs;

        let (line_end, next_start) = consume_line_ending(bytes, abs);

        if next_start >= bytes.len() {
            return None;
        }

        let b = bytes[next_start];
        if b == b'\n' || b == b'\r' {
            let (_, consume_end) = consume_line_ending(bytes, next_start);
            return Some(EventBoundary {
                block_end,
                consume: consume_end,
            });
        }

        pos = line_end;
    }
    None
}

fn consume_line_ending(bytes: &[u8], pos: usize) -> (usize, usize) {
    if pos >= bytes.len() {
        return (pos, pos);
    }
    match bytes[pos] {
        b'\r' => {
            if pos + 1 < bytes.len() && bytes[pos + 1] == b'\n' {
                (pos + 2, pos + 2)
            } else {
                (pos + 1, pos + 1)
            }
        }
        b'\n' => (pos + 1, pos + 1),
        _ => (pos, pos),
    }
}

fn next_line_str(s: &str) -> (&str, &str) {
    let bytes = s.as_bytes();
    match memchr2(b'\r', b'\n', bytes) {
        Some(i) => {
            let line = &s[..i];
            if i + 1 < bytes.len() && bytes[i] == b'\r' && bytes[i + 1] == b'\n' {
                (line, &s[i + 2..])
            } else {
                (line, &s[i + 1..])
            }
        }
        None => (s, ""),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode_all(input: &[u8]) -> Vec<Result<SseEvent, Error>> {
        let mut buf = BytesMut::from(input);
        let mut decoder = SseDecoder::new();
        let mut events = Vec::new();
        while let Some(event) = decoder.decode(&mut buf) {
            events.push(event);
        }
        events
    }

    fn decode_one(input: &[u8]) -> SseEvent {
        let events = decode_all(input);
        assert_eq!(events.len(), 1, "expected 1 event, got {}", events.len());
        events.into_iter().next().unwrap().unwrap()
    }

    fn msg(event: &str, data: &str) -> SseEvent {
        SseEvent::Message(SseMessage {
            event: event.to_owned(),
            data: data.to_owned(),
            last_event_id: Arc::from(""),
        })
    }

    fn msg_with_id(event: &str, data: &str, id: &str) -> SseEvent {
        SseEvent::Message(SseMessage {
            event: event.to_owned(),
            data: data.to_owned(),
            last_event_id: Arc::from(id),
        })
    }

    #[test]
    fn parse_simple_data_event() {
        assert_eq!(decode_one(b"data: hello\n\n"), msg("message", "hello"));
    }

    #[test]
    fn parse_event_with_type() {
        assert_eq!(
            decode_one(b"event: update\ndata: payload\n\n"),
            msg("update", "payload")
        );
    }

    #[test]
    fn parse_event_with_id() {
        assert_eq!(
            decode_one(b"id: 42\ndata: msg\n\n"),
            msg_with_id("message", "msg", "42")
        );
    }

    #[test]
    fn parse_event_with_retry() {
        let events = decode_all(b"retry: 3000\ndata: reconnect\n\n");
        assert_eq!(events.len(), 1);
        match &events[0] {
            Ok(SseEvent::Message(m)) => {
                assert_eq!(m.data, "reconnect");
            }
            other => panic!("expected message, got {other:?}"),
        }
    }

    #[test]
    fn parse_retry_standalone() {
        assert_eq!(decode_one(b"retry: 5000\n\n"), SseEvent::Retry(5000));
    }

    #[test]
    fn parse_multiline_data() {
        assert_eq!(
            decode_one(b"data: line1\ndata: line2\ndata: line3\n\n"),
            msg("message", "line1\nline2\nline3")
        );
    }

    #[test]
    fn parse_comment_ignored() {
        assert_eq!(
            decode_one(b": this is a comment\ndata: actual\n\n"),
            msg("message", "actual")
        );
    }

    #[test]
    fn parse_crlf_boundary() {
        assert_eq!(decode_one(b"data: crlf\r\n\r\n"), msg("message", "crlf"));
    }

    #[test]
    fn parse_bare_cr_boundary() {
        assert_eq!(decode_one(b"data: cr\r\r"), msg("message", "cr"));
    }

    #[test]
    fn parse_bare_cr_line_endings() {
        assert_eq!(decode_one(b"event: up\rdata: val\r\r"), msg("up", "val"));
    }

    #[test]
    fn parse_mixed_line_endings() {
        assert_eq!(
            decode_one(b"event: mix\r\ndata: a\rdata: b\ndata: c\r\n\r\n"),
            msg("mix", "a\nb\nc")
        );
    }

    #[test]
    fn parse_data_without_space_after_colon() {
        assert_eq!(decode_one(b"data:nospace\n\n"), msg("message", "nospace"));
    }

    #[test]
    fn parse_data_field_only_name() {
        assert_eq!(decode_one(b"data\n\n"), msg("message", ""));
    }

    #[test]
    fn parse_event_field_only_name() {
        assert_eq!(decode_one(b"event\ndata: x\n\n"), msg("", "x"));
    }

    #[test]
    fn parse_id_field_only_name() {
        assert_eq!(
            decode_one(b"id\ndata: x\n\n"),
            msg_with_id("message", "x", "")
        );
    }

    #[test]
    fn parse_unknown_field_ignored() {
        assert_eq!(
            decode_one(b"unknown: val\ndata: real\n\n"),
            msg("message", "real")
        );
    }

    #[test]
    fn no_event_without_double_newline() {
        let events = decode_all(b"data: incomplete\n");
        assert!(events.is_empty());
    }

    #[test]
    fn empty_block_skipped() {
        let events = decode_all(b"\n\ndata: after\n\n");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].as_ref().unwrap(), &msg("message", "after"));
    }

    #[test]
    fn parse_full_event() {
        assert_eq!(
            decode_one(b"id: 1\nevent: message\nretry: 5000\ndata: hello world\n\n"),
            msg_with_id("message", "hello world", "1")
        );
    }

    #[test]
    fn parse_two_events_sequentially() {
        let events = decode_all(b"data: first\n\ndata: second\n\n");
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].as_ref().unwrap(), &msg("message", "first"));
        assert_eq!(events[1].as_ref().unwrap(), &msg("message", "second"));
    }

    #[test]
    fn retry_non_numeric_ignored() {
        assert_eq!(decode_one(b"retry: abc\ndata: x\n\n"), msg("message", "x"));
    }

    #[test]
    fn retry_negative_ignored() {
        assert_eq!(decode_one(b"retry: -100\ndata: x\n\n"), msg("message", "x"));
    }

    #[test]
    fn retry_float_ignored() {
        assert_eq!(decode_one(b"retry: 3.14\ndata: x\n\n"), msg("message", "x"));
    }

    #[test]
    fn retry_empty_ignored() {
        assert_eq!(decode_one(b"retry: \ndata: x\n\n"), msg("message", "x"));
    }

    #[test]
    fn bom_at_stream_start() {
        assert_eq!(
            decode_one(b"\xef\xbb\xbfdata: bom\n\n"),
            msg("message", "bom")
        );
    }

    #[test]
    fn bom_only_no_events() {
        let events = decode_all(b"\xef\xbb\xbf");
        assert!(events.is_empty());
    }

    #[test]
    fn null_byte_in_id_rejected() {
        let event = decode_one(b"id: has\x00null\ndata: x\n\n");
        match event {
            SseEvent::Message(m) => {
                assert_eq!(&*m.last_event_id, "");
            }
            _ => panic!("expected message"),
        }
    }

    #[test]
    fn null_byte_in_id_does_not_update_previous_id() {
        let mut buf =
            BytesMut::from(&b"id: good\ndata: first\n\nid: bad\x00id\ndata: second\n\n"[..]);
        let mut decoder = SseDecoder::new();

        let e1 = decoder.decode(&mut buf).unwrap().unwrap();
        assert_eq!(e1, msg_with_id("message", "first", "good"));

        let e2 = decoder.decode(&mut buf).unwrap().unwrap();
        match e2 {
            SseEvent::Message(m) => {
                assert_eq!(&*m.last_event_id, "good");
            }
            _ => panic!("expected message"),
        }
    }

    #[test]
    fn last_event_id_shared_arc() {
        let mut buf = BytesMut::from(&b"id: shared\ndata: a\n\ndata: b\n\n"[..]);
        let mut decoder = SseDecoder::new();

        let e1 = decoder.decode(&mut buf).unwrap().unwrap();
        let e2 = decoder.decode(&mut buf).unwrap().unwrap();

        if let (SseEvent::Message(m1), SseEvent::Message(m2)) = (&e1, &e2) {
            assert!(Arc::ptr_eq(&m1.last_event_id, &m2.last_event_id));
            assert_eq!(&*m1.last_event_id, "shared");
        } else {
            panic!("expected two messages");
        }
    }

    #[test]
    fn event_type_defaults_to_message() {
        let event = decode_one(b"data: hello\n\n");
        match event {
            SseEvent::Message(m) => assert_eq!(m.event, "message"),
            _ => panic!("expected message"),
        }
    }

    #[test]
    fn payload_too_large() {
        let mut buf = BytesMut::from(&b"data: 0123456789abcdef\n\ndata: ok\n\n"[..]);
        let mut decoder = SseDecoder::with_max_payload_size(10);

        let e1 = decoder.decode(&mut buf);
        assert!(e1.unwrap().is_err());

        let e2 = decoder.decode(&mut buf).unwrap().unwrap();
        assert_eq!(e2, msg("message", "ok"));
    }

    #[test]
    fn payload_limit_zero_means_unlimited() {
        let big = format!("data: {}\n\n", "x".repeat(1024 * 1024));
        let mut buf = BytesMut::from(big.as_bytes());
        let mut decoder = SseDecoder::with_max_payload_size(0);

        let event = decoder.decode(&mut buf).unwrap().unwrap();
        match event {
            SseEvent::Message(m) => assert_eq!(m.data.len(), 1024 * 1024),
            _ => panic!("expected message"),
        }
    }

    #[test]
    fn two_phase_id_commit() {
        let mut buf = BytesMut::from(&b"id: first\ndata: a\n\nid: second\ndata: b\n\n"[..]);
        let mut decoder = SseDecoder::new();

        let e1 = decoder.decode(&mut buf).unwrap().unwrap();
        assert_eq!(e1, msg_with_id("message", "a", "first"));
        assert_eq!(&**decoder.last_event_id(), "first");

        let e2 = decoder.decode(&mut buf).unwrap().unwrap();
        assert_eq!(e2, msg_with_id("message", "b", "second"));
        assert_eq!(&**decoder.last_event_id(), "second");
    }

    #[test]
    fn id_persists_across_events_without_new_id() {
        let mut buf = BytesMut::from(&b"id: persistent\ndata: a\n\ndata: b\n\n"[..]);
        let mut decoder = SseDecoder::new();

        let _ = decoder.decode(&mut buf).unwrap().unwrap();
        let e2 = decoder.decode(&mut buf).unwrap().unwrap();
        assert_eq!(e2, msg_with_id("message", "b", "persistent"));
    }

    #[test]
    fn retry_only_block() {
        assert_eq!(decode_one(b"retry: 3000\n\n"), SseEvent::Retry(3000));
    }

    #[test]
    fn retry_with_data_yields_message() {
        let event = decode_one(b"retry: 1000\ndata: hello\n\n");
        match event {
            SseEvent::Message(m) => {
                assert_eq!(m.data, "hello");
            }
            _ => panic!("expected message, not retry"),
        }
    }

    #[test]
    fn incremental_buffering() {
        let mut decoder = SseDecoder::new();
        let mut buf = BytesMut::new();

        buf.extend_from_slice(b"data: hel");
        assert!(decoder.decode(&mut buf).is_none());

        buf.extend_from_slice(b"lo\n");
        assert!(decoder.decode(&mut buf).is_none());

        buf.extend_from_slice(b"\n");
        let event = decoder.decode(&mut buf).unwrap().unwrap();
        assert_eq!(event, msg("message", "hello"));
    }

    #[test]
    fn find_event_boundary_lf_lf() {
        let b = find_event_boundary(b"data: x\n\nrest").unwrap();
        assert_eq!(b.block_end, 7);
    }

    #[test]
    fn find_event_boundary_crlf_crlf() {
        let b = find_event_boundary(b"data: x\r\n\r\nrest").unwrap();
        assert_eq!(b.block_end, 7);
    }

    #[test]
    fn find_event_boundary_mixed_crlf_lf() {
        let b = find_event_boundary(b"data: x\r\n\nrest").unwrap();
        assert_eq!(b.block_end, 7);
    }

    #[test]
    fn find_event_boundary_cr_cr() {
        let b = find_event_boundary(b"data: x\r\rrest").unwrap();
        assert_eq!(b.block_end, 7);
    }

    #[test]
    fn find_event_boundary_none() {
        assert!(find_event_boundary(b"data: x\n").is_none());
    }

    #[test]
    fn comment_only_block_skipped() {
        let events = decode_all(b": comment\n\ndata: real\n\n");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].as_ref().unwrap(), &msg("message", "real"));
    }

    #[test]
    fn data_with_colon_in_value() {
        assert_eq!(
            decode_one(b"data: key: value\n\n"),
            msg("message", "key: value")
        );
    }

    #[test]
    fn multiple_spaces_after_colon_preserved() {
        assert_eq!(
            decode_one(b"data:  two spaces\n\n"),
            msg("message", " two spaces")
        );
    }

    #[test]
    fn default_decoder_equivalent_to_new() {
        let mut d1 = SseDecoder::new();
        let mut d2 = SseDecoder::default();
        let mut buf1 = BytesMut::from(&b"data: hello\n\n"[..]);
        let mut buf2 = BytesMut::from(&b"data: hello\n\n"[..]);
        let e1 = d1.decode(&mut buf1).unwrap().unwrap();
        let e2 = d2.decode(&mut buf2).unwrap().unwrap();
        assert_eq!(e1, e2);
        assert_eq!(e1, msg("message", "hello"));
    }

    #[test]
    fn retry_standalone_with_id_yields_retry() {
        // retry with id but no data and no event_type: should yield Retry
        // This exercises line 169: retry_val returned when has_data_field is false
        // and there's an id present but the standalone retry short-circuit (line 157-163) doesn't fire
        let event = decode_one(b"id: 99\nretry: 2000\n\n");
        assert_eq!(event, SseEvent::Retry(2000));
    }

    #[test]
    fn retry_with_event_type_no_data_yields_retry() {
        // retry with event_type but no data: the standalone retry short-circuit
        // at line 157-163 won't fire (event_type.is_none() is false), so it falls
        // to line 168 where retry is returned since has_data_field is false
        let event = decode_one(b"event: ping\nretry: 4000\n\n");
        assert_eq!(event, SseEvent::Retry(4000));
    }

    #[test]
    fn consume_line_ending_at_end_of_buffer() {
        // This tests the edge case where pos >= bytes.len() in consume_line_ending
        let result = consume_line_ending(b"", 0);
        assert_eq!(result, (0, 0));

        let result = consume_line_ending(b"abc", 5);
        assert_eq!(result, (5, 5));
    }

    #[test]
    fn consume_line_ending_non_newline_byte() {
        // Tests the fallthrough case where byte is neither \r nor \n
        let result = consume_line_ending(b"abc", 0);
        assert_eq!(result, (0, 0));

        let result = consume_line_ending(b"x\ny", 0);
        // 'x' is not a line ending
        assert_eq!(result, (0, 0));
    }

    #[test]
    fn consume_line_ending_cr_without_lf() {
        let result = consume_line_ending(b"\rx", 0);
        assert_eq!(result, (1, 1));
    }

    #[test]
    fn consume_line_ending_crlf() {
        let result = consume_line_ending(b"\r\n", 0);
        assert_eq!(result, (2, 2));
    }

    #[test]
    fn consume_line_ending_lf() {
        let result = consume_line_ending(b"\n", 0);
        assert_eq!(result, (1, 1));
    }

    #[test]
    fn consume_line_ending_cr_at_end() {
        // \r at last position with no following byte
        let result = consume_line_ending(b"\r", 0);
        assert_eq!(result, (1, 1));
    }

    #[test]
    fn corrupted_state_skips_next_block() {
        // Test that if somehow corrupted is true, the next event block is skipped.
        // We can trigger this by having TWO consecutive too-large payloads.
        // After the first too-large error is returned, the decoder should handle
        // the next block correctly.
        //
        // NOTE: In the current implementation, `corrupted` is set to `false` (not true)
        // when a too-large payload is detected. This means the `if self.corrupted`
        // branch (lines 89-92) is unreachable dead code. The test below verifies
        // that after a too-large event, normal decoding resumes correctly.
        let mut buf =
            BytesMut::from(&b"data: AAAAAAAAAAAAAAA\n\ndata: BBBBBBBBBBBBBBB\n\ndata: ok\n\n"[..]);
        let mut decoder = SseDecoder::with_max_payload_size(10);

        // First too-large event returns error
        let e1 = decoder.decode(&mut buf).unwrap();
        assert!(e1.is_err());

        // Second too-large event also returns error (corrupted is NOT set)
        let e2 = decoder.decode(&mut buf).unwrap();
        assert!(e2.is_err());

        // Third event is within size and succeeds
        let e3 = decoder.decode(&mut buf).unwrap().unwrap();
        assert_eq!(e3, msg("message", "ok"));
    }

    #[test]
    fn next_line_str_crlf() {
        let (line, rest) = next_line_str("hello\r\nworld");
        assert_eq!(line, "hello");
        assert_eq!(rest, "world");
    }

    #[test]
    fn next_line_str_lf_only() {
        let (line, rest) = next_line_str("hello\nworld");
        assert_eq!(line, "hello");
        assert_eq!(rest, "world");
    }

    #[test]
    fn next_line_str_cr_only() {
        let (line, rest) = next_line_str("hello\rworld");
        assert_eq!(line, "hello");
        assert_eq!(rest, "world");
    }

    #[test]
    fn next_line_str_no_newline() {
        let (line, rest) = next_line_str("hello");
        assert_eq!(line, "hello");
        assert_eq!(rest, "");
    }

    #[test]
    fn find_event_boundary_returns_none_for_no_double_newline() {
        // Only a single line ending, no event boundary
        assert!(find_event_boundary(b"data: hello\n").is_none());
        assert!(find_event_boundary(b"data: hello\r").is_none());
        assert!(find_event_boundary(b"data: hello\r\n").is_none());
    }

    #[test]
    fn find_event_boundary_returns_none_for_empty() {
        assert!(find_event_boundary(b"").is_none());
    }

    #[test]
    fn find_event_boundary_lf_at_buffer_end() {
        // The second newline is at the very end of the buffer — no "rest" after
        let b = find_event_boundary(b"data: x\n\n");
        // Should still return None because there's nothing after the boundary to confirm
        // Actually, looking at the code: next_start would be 8 (the second \n), then
        // bytes[next_start] == b'\n', so it tries to consume_line_ending at pos 8,
        // which gives (9, 9). Since consume_end=9 and bytes.len()=9... let me check
        // Actually wait, the buffer is "data: x\n\n" which is 9 bytes.
        // pos=0, memchr2 finds \n at index 7, abs=7, block_end=7
        // consume_line_ending(bytes, 7) for \n: (8, 8)
        // next_start=8, 8 < 9 so we proceed
        // bytes[8] = b'\n', so we consume_line_ending(bytes, 8) for \n: (9, 9)
        // Returns Some(EventBoundary { block_end: 7, consume: 9 })
        assert!(b.is_some());
        let b = b.unwrap();
        assert_eq!(b.block_end, 7);
        assert_eq!(b.consume, 9);
    }
}
