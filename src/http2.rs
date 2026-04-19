use std::time::Duration;

/// Configuration for HTTP/2 connections.
///
/// These settings are applied during the HTTP/2 handshake when the client
/// negotiates an h2 connection (e.g., via ALPN over TLS).
#[derive(Clone, Debug)]
pub struct Http2Config {
    pub(crate) initial_stream_window_size: Option<u32>,
    pub(crate) initial_connection_window_size: Option<u32>,
    pub(crate) max_frame_size: Option<u32>,
    pub(crate) adaptive_window: Option<bool>,
    pub(crate) keep_alive_interval: Option<Duration>,
    pub(crate) keep_alive_timeout: Option<Duration>,
    pub(crate) keep_alive_while_idle: Option<bool>,
    pub(crate) max_header_list_size: Option<u32>,
    pub(crate) max_send_buf_size: Option<usize>,
    pub(crate) max_concurrent_reset_streams: Option<usize>,
}

impl Default for Http2Config {
    fn default() -> Self {
        Self::new()
    }
}

impl Http2Config {
    pub fn new() -> Self {
        Self {
            initial_stream_window_size: None,
            initial_connection_window_size: None,
            max_frame_size: None,
            adaptive_window: None,
            keep_alive_interval: None,
            keep_alive_timeout: None,
            keep_alive_while_idle: None,
            max_header_list_size: None,
            max_send_buf_size: None,
            max_concurrent_reset_streams: None,
        }
    }

    /// Set the initial stream-level flow control window size (bytes).
    pub fn initial_stream_window_size(mut self, size: u32) -> Self {
        self.initial_stream_window_size = Some(size);
        self
    }

    /// Set the initial connection-level flow control window size (bytes).
    pub fn initial_connection_window_size(mut self, size: u32) -> Self {
        self.initial_connection_window_size = Some(size);
        self
    }

    /// Set the max HTTP/2 frame size (bytes). Must be between 16,384 and 16,777,215.
    pub fn max_frame_size(mut self, size: u32) -> Self {
        self.max_frame_size = Some(size);
        self
    }

    /// Enable adaptive flow-control window sizing.
    pub fn adaptive_window(mut self, enabled: bool) -> Self {
        self.adaptive_window = Some(enabled);
        self
    }

    /// Set the interval for HTTP/2 PING frames to keep the connection alive.
    pub fn keep_alive_interval(mut self, interval: Duration) -> Self {
        self.keep_alive_interval = Some(interval);
        self
    }

    /// Set the timeout for HTTP/2 PING acknowledgements (default: 20s in hyper).
    pub fn keep_alive_timeout(mut self, timeout: Duration) -> Self {
        self.keep_alive_timeout = Some(timeout);
        self
    }

    /// Send keep-alive PINGs even when there are no open streams.
    pub fn keep_alive_while_idle(mut self, enabled: bool) -> Self {
        self.keep_alive_while_idle = Some(enabled);
        self
    }

    /// Set the max size of received header list (bytes).
    pub fn max_header_list_size(mut self, size: u32) -> Self {
        self.max_header_list_size = Some(size);
        self
    }

    /// Set the max write buffer size per stream (bytes).
    pub fn max_send_buf_size(mut self, size: usize) -> Self {
        self.max_send_buf_size = Some(size);
        self
    }

    /// Set the max number of concurrent locally-reset streams.
    pub fn max_concurrent_reset_streams(mut self, max: usize) -> Self {
        self.max_concurrent_reset_streams = Some(max);
        self
    }
}
