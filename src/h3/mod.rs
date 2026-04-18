// HTTP/3 transport via h3 + h3-quinn
//
// This module is gated behind `feature = "http3"` and is experimental.
// It provides a QUIC-based transport that integrates with the same
// connection pool and client API as the h1/h2 paths.

// TODO: Phase 3 implementation
// - Quinn endpoint setup with shared rustls config
// - h3::client::SendRequest integration
// - Connection pool entry variant for QUIC connections
// - Alt-Svc header parsing for protocol upgrade discovery
