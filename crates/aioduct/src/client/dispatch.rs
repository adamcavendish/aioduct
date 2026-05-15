use crate::clock::Instant;

use http::Uri;

#[cfg(all(feature = "http3", feature = "rustls"))]
use crate::body::RequestBoxBody;
use crate::error::Error;
use crate::observer::{self, RequestEvent, RequestPhase};
use crate::pool::{HttpConnection, PooledConnection};
use crate::response::{BodyObserverCtx, Response};

use super::HttpEngineCore;

// ── Shared helpers (no runtime/connector bounds) ─────────────────────────────

impl<B: 'static> HttpEngineCore<B> {
    #[cfg(feature = "rustls")]
    fn populate_sans(conn: &mut PooledConnection<B>) {
        if conn.is_h2_or_h3()
            && conn.sans.is_empty()
            && let Some(der) = conn.tls_info.as_ref().and_then(|t| t.peer_certificate())
        {
            conn.sans = crate::tls::extract_sans_from_der(der);
        }
    }

    #[cfg(not(feature = "rustls"))]
    fn populate_sans(_conn: &mut PooledConnection<B>) {}

    pub(super) fn checkin_connection(
        &self,
        key: crate::pool::PoolKey,
        mut conn: PooledConnection<B>,
    ) {
        Self::populate_sans(&mut conn);
        if conn.is_multiplex_clone {
            self.fire_connection_metrics(&conn, false);
            return;
        }
        self.fire_connection_metrics(&conn, false);
        self.pool.checkin(key, conn);
    }

    pub(super) fn fire_connection_metrics(&self, conn: &PooledConnection<B>, closed: bool) {
        if let Some(ref obs) = self.observer
            && let Some(remote_addr) = conn.remote_addr
        {
            obs.on_connection_event(&observer::ConnectionEvent {
                phase: observer::ConnectionPhase::Metrics {
                    remote_addr,
                    protocol: Self::connection_protocol(conn),
                    bytes_sent: conn.bytes_sent,
                    bytes_received: conn.bytes_received,
                    connection_age: conn.created_at.elapsed(),
                    requests_served: conn.requests_served,
                    closed,
                },
                at: observer::Instant::now(),
            });
        }
    }

    #[inline]
    pub(super) fn notify(&self, method: &http::Method, uri: &Uri, phase: RequestPhase) {
        if let Some(ref obs) = self.observer {
            obs.on_event(&RequestEvent {
                method: method.clone(),
                uri: uri.clone(),
                phase,
                at: observer::Instant::now(),
            });
        }
    }

    pub(super) fn attach_observer(&self, resp: &mut Response, method: &http::Method, uri: &Uri) {
        if let Some(ref obs) = self.observer {
            resp.set_observer_ctx(BodyObserverCtx {
                observer: obs.clone(),
                method: method.clone(),
                uri: uri.clone(),
                response_started: Instant::now(),
            });
        }
    }

    pub(super) fn connection_protocol(conn: &PooledConnection<B>) -> observer::NegotiatedProtocol {
        match &conn.conn {
            HttpConnection::H1(_) => observer::NegotiatedProtocol::Http1,
            HttpConnection::H2(_) => observer::NegotiatedProtocol::Http2,
            #[cfg(all(feature = "http3", feature = "rustls"))]
            HttpConnection::H3(_) => observer::NegotiatedProtocol::Http3,
        }
    }

    pub(super) fn is_stale_connection_error(err: &Error) -> bool {
        match err {
            Error::Hyper(e) => {
                if e.is_canceled() || e.is_closed() || e.is_incomplete_message() {
                    return true;
                }
                use std::error::Error as _;
                if let Some(io_err) = e.source().and_then(|s| s.downcast_ref::<std::io::Error>()) {
                    return matches!(
                        io_err.kind(),
                        std::io::ErrorKind::ConnectionReset
                            | std::io::ErrorKind::BrokenPipe
                            | std::io::ErrorKind::ConnectionAborted
                    );
                }
                false
            }
            Error::Io(e) => matches!(
                e.kind(),
                std::io::ErrorKind::ConnectionReset
                    | std::io::ErrorKind::BrokenPipe
                    | std::io::ErrorKind::ConnectionAborted
            ),
            _ => false,
        }
    }

    pub(super) async fn send_on_connection(
        conn: &mut PooledConnection<B>,
        request: http::Request<B>,
        url: Uri,
    ) -> Result<Response, Error>
    where
        B: http_body::Body<Data = bytes::Bytes, Error = crate::error::Error>,
    {
        #[cfg(feature = "tracing")]
        let proto = match &conn.conn {
            HttpConnection::H1(_) => "h1",
            HttpConnection::H2(_) => "h2",
            #[cfg(all(feature = "http3", feature = "rustls"))]
            HttpConnection::H3(_) => "h3",
        };
        #[cfg(feature = "tracing")]
        tracing::trace!(
            protocol = proto,
            host = url.host().unwrap_or(""),
            "http.send.start"
        );

        let body_size = http_body::Body::size_hint(request.body())
            .exact()
            .unwrap_or(0);
        conn.bytes_sent += body_size;
        conn.requests_served += 1;

        let result = match &mut conn.conn {
            HttpConnection::H1(sender) => {
                let resp = sender.send_request(request).await?;
                let resp = resp.map(crate::response::ResponseBoxSendBody::from_incoming);
                Ok(Response::new(resp, url))
            }
            HttpConnection::H2(sender) => {
                let resp = sender.send_request(request).await?;
                let resp = resp.map(crate::response::ResponseBoxSendBody::from_incoming);
                Ok(Response::new(resp, url))
            }
            #[cfg(all(feature = "http3", feature = "rustls"))]
            HttpConnection::H3(sender) => {
                use http_body_util::BodyExt as _;
                let (parts, body) = request.into_parts();
                let collected = body.collect().await?;
                let boxed: RequestBoxBody = http_body_util::Full::new(collected.to_bytes())
                    .map_err(|never| match never {})
                    .boxed_unsync();
                let request = http::Request::from_parts(parts, boxed);
                crate::h3_transport::send_on_h3(sender, request, url).await
            }
        };

        if let Ok(ref resp) = result
            && let Some(len) = resp.content_length()
        {
            conn.bytes_received += len;
        }

        #[cfg(feature = "tracing")]
        if let Ok(ref resp) = result {
            tracing::trace!(status = resp.status().as_u16(), "http.send.done");
        }

        result
    }
}
