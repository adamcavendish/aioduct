use super::*;
use aioduct::observer::{Instant, NegotiatedProtocol, RetryKind};
use http::{Method, StatusCode, Uri};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::Instant as StdInstant;

fn make_event(phase: RequestPhase) -> RequestEvent {
    RequestEvent {
        method: Method::GET,
        uri: Uri::from_static("http://example.com"),
        phase,
        at: Instant::now(),
    }
}

fn key(code: KeyCode) -> crossterm::event::KeyEvent {
    crossterm::event::KeyEvent::new(code, KeyModifiers::NONE)
}

fn ctrl_key(code: KeyCode) -> crossterm::event::KeyEvent {
    crossterm::event::KeyEvent::new(code, KeyModifiers::CONTROL)
}

#[test]
fn state_tracks_phases() {
    let mut state = TuiState::new();
    state.apply(&make_event(RequestPhase::Started));
    state.apply(&make_event(RequestPhase::DnsResolved {
        addrs: vec![SocketAddr::new(IpAddr::V4(Ipv4Addr::new(1, 2, 3, 4)), 443)],
        duration: Duration::from_millis(10),
    }));
    assert_eq!(state.phases.len(), 2);
    assert!(!state.done);
}

#[test]
fn state_marks_done_on_response_complete() {
    let mut state = TuiState::new();
    state.apply(&make_event(RequestPhase::ResponseComplete {
        status: StatusCode::OK,
        protocol: NegotiatedProtocol::Http2,
        total_duration: Duration::from_millis(200),
    }));
    assert!(state.done);
    assert!(state.status_line.contains("200"));
}

#[test]
fn state_marks_done_on_failure() {
    let mut state = TuiState::new();
    state.apply(&make_event(RequestPhase::Failed {
        error: "timeout".into(),
        retry: RetryKind::None,
        elapsed: Duration::from_millis(5000),
    }));
    assert!(state.done);
    assert!(state.status_line.contains("timeout"));
}

#[test]
fn request_headers_stored() {
    let mut state = TuiState::new();
    state.apply(&make_event(RequestPhase::RequestSent {
        duration: Duration::from_millis(2),
        headers: vec![
            ("host".into(), "example.com".into()),
            ("content-type".into(), "application/json".into()),
        ],
    }));
    assert_eq!(state.request_headers.len(), 2);
}

#[test]
fn trailers_received_updates_headers_trace_and_events() {
    let mut state = TuiState::new();
    state.apply(&make_event(RequestPhase::TrailersReceived {
        headers: vec![
            ("grpc-status".into(), "0".into()),
            ("server-timing".into(), "app;dur=12".into()),
        ],
    }));

    assert!(state.trailers_observable);
    assert_eq!(state.trailers.len(), 2);
    assert_eq!(trailer_summary(&state), "2 received");
    assert!(
        state
            .event_lines
            .back()
            .unwrap()
            .text
            .contains("trailers received")
    );
    assert_eq!(state.phases.last().unwrap().label, "TRAILERS 2");
}

#[test]
fn empty_trailers_are_observable_and_report_none() {
    let mut state = TuiState::new();
    state.apply(&make_event(RequestPhase::TrailersReceived {
        headers: vec![],
    }));

    assert!(state.trailers_observable);
    assert!(state.trailers.is_empty());
    assert_eq!(trailer_summary(&state), "none received");
    assert_eq!(
        state.event_lines.back().unwrap().text,
        "trailers received: none"
    );
}

#[test]
fn declared_trailers_show_waiting_state() {
    let mut state = TuiState::new();
    state.active_page = 2;
    state.response_headers = vec![
        ("Trailer".into(), "grpc-status, grpc-message".into()),
        ("trailer".into(), "server-timing".into()),
    ];

    assert_eq!(
        declared_trailer_names(&state),
        vec!["grpc-status", "grpc-message", "server-timing"]
    );
    assert!(state.headers_show_trailers());
    assert_eq!(state.focus_count(), 3);
    assert_eq!(trailer_summary(&state), "declared, waiting");
    assert_eq!(
        trailer_text_lines(&state),
        vec![
            "declared, waiting after body",
            "expected: grpc-status, grpc-message, server-timing"
        ]
    );
}

#[test]
fn trailer_focus_has_dedicated_label() {
    let mut state = TuiState::new();
    state.active_page = 2;
    state.focus_index = 2;
    state.response_headers = vec![("Trailer".into(), "grpc-status".into())];

    assert_eq!(http_focus_label(&state), "trailers");
}

#[test]
fn event_timestamp_uses_wall_clock_shape() {
    let time = Time::from_hms_milli(8, 14, 12, 531).unwrap();

    assert_eq!(format_event_timestamp(time), "08:14:12.531");
}

#[test]
fn min_terminal_size_fallback() {
    let backend = ratatui::backend::TestBackend::new(20, 5);
    let mut terminal = ratatui::Terminal::new(backend).unwrap();
    let mut state = TuiState::new();
    terminal.draw(|f| render(f, &mut state)).unwrap();
}

#[test]
fn body_chunk_splits_lines() {
    let mut state = TuiState::new();
    state.apply_body_chunk("line1\nline2\n", StdInstant::now());
    assert_eq!(state.body_lines, vec!["line1", "line2"]);
    assert!(!state.body_done);
}

#[test]
fn body_chunk_buffers_partial_line() {
    let mut state = TuiState::new();
    state.apply_body_chunk("hel", StdInstant::now());
    assert!(state.body_lines.is_empty());
    state.apply_body_chunk("lo\nworld\n", StdInstant::now());
    assert_eq!(state.body_lines, vec!["hello", "world"]);
}

#[test]
fn body_done_flushes_partial_line() {
    let mut state = TuiState::new();
    state.apply_body_chunk("no trailing newline", StdInstant::now());
    assert!(state.body_lines.is_empty());
    state.apply_body_done();
    assert_eq!(state.body_lines, vec!["no trailing newline"]);
    assert!(state.body_done);
}

#[test]
fn body_rolling_buffer_evicts_old_lines() {
    let mut state = TuiState::new();
    // Each line is ~1024 bytes → 70 lines ≈ 70KB > 64KB cap
    let line = "x".repeat(1024);
    for _ in 0..70 {
        state.apply_body_chunk(&format!("{line}\n"), StdInstant::now());
    }
    assert!(state.body_cap_bytes <= 64 * 1024 + 1024); // Allow slight overshoot from last line
    assert!(state.body_lines.len() < 70);
}

#[test]
fn sse_detected_from_response_headers() {
    let headers = [
        ("content-type".into(), "text/event-stream".into()),
        ("transfer-encoding".into(), "chunked".into()),
    ];
    let mut state = TuiState::new();
    assert!(!state.body_is_sse);
    // Simulate what the event loop does
    state.body_is_sse = headers.iter().any(|(k, v): &(String, String)| {
        k.eq_ignore_ascii_case("content-type") && v.starts_with("text/event-stream")
    });
    assert!(state.body_is_sse);
}

#[test]
fn sse_not_detected_for_plain_response() {
    let headers = [("content-type".into(), "application/json".into())];
    let mut state = TuiState::new();
    state.body_is_sse = headers.iter().any(|(k, v): &(String, String)| {
        k.eq_ignore_ascii_case("content-type") && v.starts_with("text/event-stream")
    });
    assert!(!state.body_is_sse);
}

#[test]
fn scroll_down_moves_offset() {
    let mut state = TuiState::new();
    state.body_scroll = 0;
    state.body_auto_scroll = false;
    state.scroll_down(5);
    assert_eq!(state.body_scroll, 5);
    assert!(!state.body_auto_scroll);
}

#[test]
fn scroll_up_clamps_at_zero() {
    let mut state = TuiState::new();
    state.body_scroll = 3;
    state.scroll_up(10);
    assert_eq!(state.body_scroll, 0);
}

#[test]
fn scroll_to_bottom_enables_auto_scroll() {
    let mut state = TuiState::new();
    state.body_auto_scroll = false;
    state.scroll_to_bottom();
    assert!(state.body_auto_scroll);
}

#[test]
fn help_toggle() {
    let mut state = TuiState::new();
    assert!(!state.show_help);
    state.show_help = !state.show_help;
    assert!(state.show_help);
}

#[test]
fn esc_closes_help_without_quitting() {
    let mut state = TuiState::new();
    state.show_help = true;
    assert!(!handle_key_event(key(KeyCode::Esc), &mut state));
    assert!(!state.show_help);
}

#[test]
fn esc_without_overlay_does_not_quit() {
    let mut state = TuiState::new();
    assert!(!handle_key_event(key(KeyCode::Esc), &mut state));
    assert!(!state.show_help);
}

#[test]
fn q_quits_even_when_help_is_open() {
    let mut state = TuiState::new();
    state.show_help = true;
    assert!(handle_key_event(key(KeyCode::Char('q')), &mut state));
}

#[test]
fn ctrl_c_quits() {
    let mut state = TuiState::new();
    assert!(handle_key_event(ctrl_key(KeyCode::Char('c')), &mut state));
}

#[test]
fn tab_moves_focus_without_changing_page() {
    let mut state = TuiState::new();
    assert_eq!(state.active_page, 0);
    assert_eq!(state.focus_index, 0);
    assert!(!handle_key_event(key(KeyCode::Tab), &mut state));
    assert_eq!(state.active_page, 0);
    assert_eq!(state.focus_index, 1);
}

#[test]
fn number_keys_jump_pages_and_reset_focus() {
    let mut state = TuiState::new();
    state.focus_index = 1;
    assert!(!handle_key_event(key(KeyCode::Char('4')), &mut state));
    assert_eq!(state.active_page, 3);
    assert_eq!(state.focus_index, 0);
}

#[test]
fn slash_filter_captures_text_until_escape() {
    let mut state = TuiState::new();
    assert!(!handle_key_event(key(KeyCode::Char('/')), &mut state));
    assert!(state.editing_filter);
    assert!(!handle_key_event(key(KeyCode::Char('x')), &mut state));
    assert_eq!(state.filter_query, "x");
    assert!(!handle_key_event(key(KeyCode::Esc), &mut state));
    assert!(!state.editing_filter);
    assert_eq!(state.filter_query, "x");
}

#[test]
fn space_toggles_body_autoscroll_on_body_page() {
    let mut state = TuiState::new();
    state.active_page = 3;
    assert!(state.body_auto_scroll);
    assert!(!handle_key_event(key(KeyCode::Char(' ')), &mut state));
    assert!(!state.body_auto_scroll);
}

#[test]
fn multiple_body_chunks_accumulate() {
    let mut state = TuiState::new();
    state.apply_body_chunk("a\nb\nc\n", StdInstant::now());
    state.apply_body_chunk("d\ne\n", StdInstant::now());
    state.apply_body_done();
    assert_eq!(state.body_lines, vec!["a", "b", "c", "d", "e"]);
}

// ── sanitize_event_text tests ──

#[test]
fn sanitize_passes_plain_text() {
    assert_eq!(
        super::sanitize_event_text("hello world".into()),
        "hello world"
    );
}

#[test]
fn sanitize_collapses_newlines() {
    assert_eq!(
        super::sanitize_event_text("line1\nline2\r\nline3".into()),
        "line1 line2 line3"
    );
}

#[test]
fn sanitize_collapses_multiple_newlines() {
    assert_eq!(super::sanitize_event_text("a\n\nb".into()), "a b");
}

#[test]
fn sanitize_removes_ansi_escapes() {
    assert_eq!(
        super::sanitize_event_text("ok \x1b[31mred\x1b[0m text".into()),
        "ok red text"
    );
}

#[test]
fn sanitize_removes_ansi_escapes_with_sgr() {
    assert_eq!(
        super::sanitize_event_text("prefix \x1b[1;32mbold green\x1b[m suffix".into()),
        "prefix bold green suffix"
    );
}

#[test]
fn sanitize_replaces_control_chars() {
    assert_eq!(
        super::sanitize_event_text("text\x00null\x08bs".into()),
        "text null bs"
    );
}

#[test]
fn sanitize_preserves_tabs() {
    assert_eq!(
        super::sanitize_event_text("col1\tcol2".into()),
        "col1\tcol2"
    );
}

#[test]
fn sanitize_empty_string() {
    assert_eq!(super::sanitize_event_text("".into()), "");
}

#[test]
fn sanitize_error_message_with_newlines() {
    let text = "connection error:\n  caused by timeout\n  request id: abc123";
    let result = super::sanitize_event_text(text.into());
    assert!(!result.contains('\n'));
    assert!(result.contains("connection error:"));
}

#[test]
fn log_event_sanitizes_text() {
    let mut state = TuiState::new();
    state.log_event("error\nwith\nnewlines", Color::Red);
    let last = state.event_lines.back().unwrap();
    assert!(!last.text.contains('\n'));
    assert_eq!(last.text, "error with newlines");
}

#[test]
fn log_event_removes_ansi_from_error() {
    let mut state = TuiState::new();
    state.log_event("failed: \x1b[31mtimeout\x1b[0m after 5s", Color::Red);
    let last = state.event_lines.back().unwrap();
    assert!(!last.text.contains('\x1b'));
    assert_eq!(last.text, "failed: timeout after 5s");
}
