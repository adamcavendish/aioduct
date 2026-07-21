use crate::clock::Instant;

use http::Uri;

use crate::error::Error;
use crate::observer::{self, RequestEvent, RequestPhase, RetryKind};
use crate::pool::{HttpConnection, PoolKey, PooledConnection};
use crate::response::{BodyObserverCtx, Response};

use super::replay::ReplayReason;

pub(super) struct H2ConnectGuard<'a, B: 'static> {
    pub(super) pool: &'a crate::pool::ConnectionPool<B>,
    pub(super) key: &'a crate::pool::PoolKey,
    pub(super) active: bool,
}

impl<B: 'static> Drop for H2ConnectGuard<'_, B> {
    fn drop(&mut self) {
        if self.active {
            self.pool.unmark_connecting_h2(self.key);
        }
    }
}

pub(super) enum PooledSendError<B> {
    Recovered {
        error: Error,
        request: Box<http::Request<B>>,
    },
    Failed(Error),
}

impl<B> PooledSendError<B> {
    pub(super) fn into_error(self) -> Error {
        match self {
            Self::Recovered { error, .. } | Self::Failed(error) => error,
        }
    }
}

impl<B> From<Error> for PooledSendError<B> {
    fn from(error: Error) -> Self {
        Self::Failed(error)
    }
}

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

    /// Returns true if the response indicates the connection should not be reused.
    pub(super) fn should_skip_checkin(resp: &Response) -> bool {
        if resp.status() == http::StatusCode::SWITCHING_PROTOCOLS {
            return true;
        }
        resp.headers()
            .get(http::header::CONNECTION)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.eq_ignore_ascii_case("close"))
    }

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

    /// Check in a connection, deferring for H1 until the response body is
    /// fully consumed so the connection is genuinely ready for reuse.
    ///
    /// For H2/H3 (multiplexed) connections, check-in is immediate since they
    /// can handle concurrent streams. For H1, a background task polls
    /// `poll_ready` and only returns the connection to the pool once it is
    /// ready. This prevents concurrent checkouts from finding (and destroying)
    /// not-ready connections in the pool.
    ///
    /// The background task times out after the pool's idle timeout — if the
    /// body isn't consumed by then, the connection is dropped.
    pub(super) fn checkin_when_ready<R, F, S>(
        &self,
        key: crate::pool::PoolKey,
        mut conn: PooledConnection<B>,
        spawn: F,
        sleep: S,
    ) where
        R: crate::runtime::RuntimePoll,
        F: FnOnce(std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>),
        S: std::future::Future<Output = ()> + Send + 'static,
        B: Send + 'static,
    {
        self.pool.ensure_reaper::<R>();

        Self::populate_sans(&mut conn);
        if conn.is_multiplex_clone {
            self.fire_connection_metrics(&conn, false);
            return;
        }
        self.fire_connection_metrics(&conn, false);

        if !conn.is_h1() || conn.is_ready() {
            self.pool.checkin(key, conn);
            return;
        }

        let pool = self.pool.clone();
        spawn(Box::pin(async move {
            let ready_fut = std::future::poll_fn(|cx| conn.poll_ready(cx));
            let result = crate::timeout::race_deadline(ready_fut, sleep).await;
            if let Some(true) = result {
                pool.checkin(key, conn);
            }
        }));
    }

    /// Like [`checkin_when_ready`](Self::checkin_when_ready) but for the Local
    /// (`!Send`) path. The spawn closure accepts a non-Send future.
    pub(super) fn checkin_when_ready_local<R, F, S>(
        &self,
        key: crate::pool::PoolKey,
        mut conn: PooledConnection<B>,
        spawn: F,
        sleep: S,
    ) where
        R: crate::runtime::RuntimeLocal,
        F: FnOnce(std::pin::Pin<Box<dyn std::future::Future<Output = ()> + 'static>>),
        S: std::future::Future<Output = ()> + 'static,
        B: 'static,
    {
        self.pool.ensure_reaper_local::<R>();

        Self::populate_sans(&mut conn);
        if conn.is_multiplex_clone {
            self.fire_connection_metrics(&conn, false);
            return;
        }
        self.fire_connection_metrics(&conn, false);

        if !conn.is_h1() || conn.is_ready() {
            self.pool.checkin(key, conn);
            return;
        }

        let pool = self.pool.clone();
        spawn(Box::pin(async move {
            let ready_fut = std::future::poll_fn(|cx| conn.poll_ready(cx));
            let result = crate::timeout::race_deadline(ready_fut, sleep).await;
            if let Some(true) = result {
                pool.checkin(key, conn);
            }
        }));
    }

    pub(super) fn fire_connection_metrics(&self, conn: &PooledConnection<B>, closed: bool) {
        if let Some(ref obs) = self.observer
            && let Some(remote_addr) = conn.remote_addr
        {
            obs.on_connection_event(&observer::ConnectionEvent {
                phase: observer::ConnectionPhase::Metrics {
                    remote_addr,
                    protocol: Self::connection_protocol(conn),
                    bytes_sent: conn.bytes_sent(),
                    bytes_received: conn.bytes_received(),
                    connection_age: conn.created_at.elapsed(),
                    requests_served: conn.requests_served(),
                    closed,
                },
                at: observer::Instant::now(),
            });
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn record_exact_pooled_recovery(
        &self,
        conn: &PooledConnection<B>,
        evict_key: &PoolKey,
        method: &http::Method,
        uri: &Uri,
        error: &Error,
        request_start: Instant,
        pool_checkout_start: Instant,
    ) {
        #[cfg(feature = "tracing")]
        tracing::debug!(
            host = uri.host().unwrap_or(""),
            error = %error,
            recovery = "exact_request",
            "connection.pool.rejected_before_serialization"
        );
        if conn.is_h2_or_h3() {
            self.pool.evict(evict_key);
        }
        self.pool.record_stale_reuse_retry();
        self.fire_connection_metrics(conn, true);
        self.notify(
            method,
            uri,
            RequestPhase::Failed {
                error: error.to_string(),
                retry: RetryKind::StaleConnection,
                elapsed: request_start.elapsed(),
            },
        );
        self.notify(
            method,
            uri,
            RequestPhase::PoolCheckoutComplete {
                outcome: observer::PoolOutcome::StaleRetry,
                blocked_duration: pool_checkout_start.elapsed(),
            },
        );
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

    pub(super) fn stale_replay_reason(
        conn: &PooledConnection<B>,
        err: &Error,
    ) -> Option<ReplayReason> {
        if matches!(conn.conn, HttpConnection::H2(_)) && h2_proves_request_was_unprocessed(err) {
            return Some(ReplayReason::ProvenUnprocessed);
        }
        Self::is_stale_connection_error(err).then_some(ReplayReason::AmbiguousTransportFailure)
    }

    #[cfg(test)]
    pub(crate) fn is_stale_connection_error_pub(err: &Error) -> bool {
        Self::is_stale_connection_error(err)
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

        let body_size = request
            .headers()
            .get(http::header::CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<u64>().ok())
            .or_else(|| http_body::Body::size_hint(request.body()).exact())
            .unwrap_or(0);
        conn.record_request(body_size);

        let result = match &mut conn.conn {
            HttpConnection::H1(sender) => {
                let resp = sender.send_request(request).await?;
                let resp = resp.map(crate::response::ResponseBodySend::from_incoming);
                Ok(Response::new(resp, url))
            }
            HttpConnection::H2(sender) => {
                let resp = sender.send_request(request).await?;
                let resp = resp.map(crate::response::ResponseBodySend::from_incoming);
                Ok(Response::new(resp, url))
            }
            #[cfg(all(feature = "http3", feature = "rustls"))]
            HttpConnection::H3(_) => Err(Error::Unsupported(
                "HTTP/3 dispatch requires a Send runtime".to_owned(),
            )),
        };

        if let Ok(ref resp) = result
            && let Some(len) = resp.content_length()
        {
            conn.record_bytes_received(len);
        }

        #[cfg(feature = "tracing")]
        if let Ok(ref resp) = result {
            tracing::trace!(status = resp.status().as_u16(), "http.send.done");
        }

        result
    }

    pub(super) async fn try_send_on_pooled_connection(
        conn: &mut PooledConnection<B>,
        request: http::Request<B>,
        url: Uri,
    ) -> Result<Response, PooledSendError<B>>
    where
        B: http_body::Body<Data = bytes::Bytes, Error = crate::error::Error>,
    {
        #[cfg(all(feature = "http3", feature = "rustls"))]
        if matches!(&conn.conn, HttpConnection::H3(_)) {
            return Self::send_on_connection(conn, request, url)
                .await
                .map_err(PooledSendError::Failed);
        }

        let body_size = request
            .headers()
            .get(http::header::CONTENT_LENGTH)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<u64>().ok())
            .or_else(|| http_body::Body::size_hint(request.body()).exact())
            .unwrap_or(0);

        let result = match &mut conn.conn {
            HttpConnection::H1(sender) => match sender.try_send_request(request).await {
                Ok(response) => Ok(response),
                Err(mut error) => {
                    let request = error.take_message();
                    let error = Error::Hyper(error.into_error());
                    match request {
                        Some(request) => Err(PooledSendError::Recovered {
                            error,
                            request: Box::new(request),
                        }),
                        None => Err(PooledSendError::Failed(error)),
                    }
                }
            },
            HttpConnection::H2(sender) => match sender.try_send_request(request).await {
                Ok(response) => Ok(response),
                Err(mut error) => {
                    let request = error.take_message();
                    let error = Error::Hyper(error.into_error());
                    match request {
                        Some(request) => Err(PooledSendError::Recovered {
                            error,
                            request: Box::new(request),
                        }),
                        None => Err(PooledSendError::Failed(error)),
                    }
                }
            },
            #[cfg(all(feature = "http3", feature = "rustls"))]
            HttpConnection::H3(_) => unreachable!("HTTP/3 is dispatched above"),
        };

        if !matches!(result, Err(PooledSendError::Recovered { .. })) {
            conn.record_request(body_size);
        }
        let response = result.map(|response| {
            let response = response.map(crate::response::ResponseBodySend::from_incoming);
            Response::new(response, url)
        });
        if let Ok(ref response) = response
            && let Some(len) = response.content_length()
        {
            conn.record_bytes_received(len);
        }
        response
    }
}

#[cfg(all(feature = "http3", feature = "rustls"))]
impl HttpEngineCore<crate::body::RequestBodySend> {
    pub(super) async fn send_on_connection_send<R>(
        conn: &mut PooledConnection<crate::body::RequestBodySend>,
        request: http::Request<crate::body::RequestBodySend>,
        url: Uri,
    ) -> Result<Response, Error>
    where
        R: crate::runtime::RuntimePoll,
    {
        if !matches!(conn.conn, HttpConnection::H3(_)) {
            return Self::send_on_connection(conn, request, url).await;
        }

        #[cfg(feature = "tracing")]
        tracing::trace!(
            protocol = "h3",
            host = url.host().unwrap_or(""),
            "http.send.start"
        );

        let body_size = request
            .headers()
            .get(http::header::CONTENT_LENGTH)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<u64>().ok())
            .or_else(|| http_body::Body::size_hint(request.body()).exact())
            .unwrap_or(0);
        conn.record_request(body_size);

        let HttpConnection::H3(sender) = &mut conn.conn else {
            unreachable!("HTTP/3 connection changed during dispatch")
        };
        let result = crate::h3_transport::send_on_h3::<R>(sender, request, url).await;

        if let Ok(ref response) = result
            && let Some(length) = response.content_length()
        {
            conn.record_bytes_received(length);
        }

        #[cfg(feature = "tracing")]
        if let Ok(ref response) = result {
            tracing::trace!(status = response.status().as_u16(), "http.send.done");
        }

        result
    }

    pub(super) async fn try_send_on_pooled_connection_send<R>(
        conn: &mut PooledConnection<crate::body::RequestBodySend>,
        request: http::Request<crate::body::RequestBodySend>,
        url: Uri,
    ) -> Result<Response, PooledSendError<crate::body::RequestBodySend>>
    where
        R: crate::runtime::RuntimePoll,
    {
        if matches!(conn.conn, HttpConnection::H3(_)) {
            return Self::send_on_connection_send::<R>(conn, request, url)
                .await
                .map_err(PooledSendError::Failed);
        }
        Self::try_send_on_pooled_connection(conn, request, url).await
    }
}

fn h2_proves_request_was_unprocessed(err: &Error) -> bool {
    let mut source: Option<&(dyn std::error::Error + 'static)> = Some(err);
    while let Some(error) = source {
        if let Some(h2_error) = error.downcast_ref::<h2::Error>() {
            // h2 only assigns a received GOAWAY to request streams whose IDs
            // are above the peer's last processed stream boundary.
            return h2_error.is_remote()
                && ((h2_error.is_reset()
                    && h2_error.reason() == Some(h2::Reason::REFUSED_STREAM))
                    || h2_error.is_go_away());
        }
        source = error.source();
    }
    false
}

#[cfg(test)]
#[path = "dispatch/tests.rs"]
mod tests;

#[cfg(all(test, feature = "tokio"))]
#[path = "dispatch/recovery_tests.rs"]
mod recovery_tests;
