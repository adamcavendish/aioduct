use http::Uri;

use crate::error::Error;
use crate::pool::{HttpConnection, PoolKey, PooledConnection};
use crate::response::Response;

use super::replay::ReplayReason;

// A request recovered before serialization can be retried on another protocol.
// Track only the transport-generated Host; explicit and signed fields stay intact.
#[derive(Clone)]
pub(super) struct AutomaticHost(http::HeaderValue);

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

#[derive(Clone, Copy)]
struct ObservationAttemptStarted;

impl<B: 'static> HttpEngineCore<B> {
    pub(crate) fn start_observed_attempt(
        request: &mut http::Request<B>,
        uri: &http::Uri,
        local_observer: &impl crate::observer::AttemptStartObserver,
    ) {
        if request
            .extensions()
            .get::<ObservationAttemptStarted>()
            .is_some()
        {
            return;
        }
        let Some(attempt) = request
            .extensions()
            .get::<crate::observer::RequestAttemptCounter>()
            .map(crate::observer::RequestAttemptCounter::started)
        else {
            return;
        };
        request.extensions_mut().insert(ObservationAttemptStarted);
        if let Some(observer) = request
            .extensions()
            .get::<crate::observer::SendAttemptObserver>()
        {
            observer.started(attempt, request.method(), uri);
        }
        if let Some(at) = local_observer.start_time() {
            local_observer.started(attempt, request.method(), uri, at);
        }
    }
    pub(super) fn finalize_deferred_request_headers<ReqBody>(
        request: &mut http::Request<ReqBody>,
        connection: &PooledConnection<B>,
    ) -> Result<(), Error> {
        if let Some(target) = request
            .extensions()
            .get::<crate::forward::dispatch_plan::DeferredForwardTarget>()
            .cloned()
        {
            target.apply(request, connection.version());
        }

        if let Some(deferred_te) = request
            .extensions()
            .get::<crate::forward::dispatch_plan::DeferredTe>()
            .copied()
        {
            request.headers_mut().remove(http::header::TE);
            if connection.is_h2_or_h3() {
                match deferred_te {
                    crate::forward::dispatch_plan::DeferredTe::Trailers
                    | crate::forward::dispatch_plan::DeferredTe::TrailersForH2OrH3 => {
                        request
                            .headers_mut()
                            .insert(http::header::TE, http::HeaderValue::from_static("trailers"));
                    }
                    crate::forward::dispatch_plan::DeferredTe::InvalidForH2OrH3 => {
                        return Err(Error::InvalidHeader(
                            "HTTP/2 and HTTP/3 TE fields may contain only `trailers`".to_owned(),
                        ));
                    }
                }
            } else {
                if deferred_te == crate::forward::dispatch_plan::DeferredTe::TrailersForH2OrH3
                    && request.headers().contains_key("signature-input")
                {
                    return Err(Error::Unsupported(
                        "a negotiated HTTP/1.1 forward cannot retain a signature generated over an HTTP/2-only TE field"
                            .to_owned(),
                    ));
                }
                if deferred_te == crate::forward::dispatch_plan::DeferredTe::Trailers {
                    crate::forward::restore_h1_te_trailers(request.headers_mut());
                }
            }
        }
        if let Some(framing) = request
            .extensions()
            .get::<crate::forward::dispatch_plan::DeferredForwardFraming>()
            .copied()
        {
            framing.apply(request.headers_mut(), connection.version())?;
        }
        if let Some(trailers) = request
            .extensions()
            .get::<crate::forward::dispatch_plan::DeferredForwardTrailers>()
        {
            trailers.apply(connection.version());
        }
        crate::forward::validate_final_forward_request_headers(
            connection.version(),
            request.headers_mut(),
        )?;
        Ok(())
    }

    pub(super) fn prepare_request_target_for_connection<ReqBody>(
        request: &mut http::Request<ReqBody>,
        connection: &PooledConnection<B>,
        full_uri: &Uri,
    ) -> Result<(), Error> {
        prepare_automatic_host(request, full_uri, connection.is_h1())?;
        match &connection.conn {
            HttpConnection::H1(_) => prepare_h1_request_target(request),
            HttpConnection::H2(_) => {
                prepare_h2_or_h3_request_target(request, full_uri, http::Version::HTTP_2)
            }
            #[cfg(all(feature = "http3", feature = "rustls"))]
            HttpConnection::H3(_) => {
                prepare_h2_or_h3_request_target(request, full_uri, http::Version::HTTP_3)
            }
        }
    }

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
    pub(super) fn should_skip_checkin(resp: &Response, method: &http::Method) -> bool {
        if resp.status() == http::StatusCode::SWITCHING_PROTOCOLS {
            return true;
        }
        if *method == http::Method::CONNECT
            && resp.status().is_success()
            && resp.version() != http::Version::HTTP_2
        {
            return true;
        }
        resp.headers()
            .get(http::header::CONNECTION)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.eq_ignore_ascii_case("close"))
    }

    pub(super) fn retain_connect_stream_permit(
        response: &mut Response,
        method: &http::Method,
        connection: &mut PooledConnection<B>,
    ) {
        if *method == http::Method::CONNECT
            && response.status().is_success()
            && response.version() == http::Version::HTTP_2
            && let Some(permit) = connection.take_active_stream_permit()
        {
            response.hold_active_stream_permit(permit);
        }
    }

    pub(super) fn checkin_connection(
        &self,
        key: crate::pool::PoolKey,
        mut conn: PooledConnection<B>,
    ) {
        Self::populate_sans(&mut conn);
        if conn.is_multiplex_clone {
            return;
        }
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
            return;
        }

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
            return;
        }

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

    #[allow(clippy::too_many_arguments)]
    pub(super) fn record_exact_pooled_recovery(
        &self,
        conn: &PooledConnection<B>,
        evict_key: &PoolKey,
        _uri: &Uri,
        _error: &Error,
    ) {
        #[cfg(feature = "tracing")]
        tracing::debug!(
            host = _uri.host().unwrap_or(""),
            error = %_error,
            recovery = "exact_request",
            "connection.pool.rejected_before_serialization"
        );
        if conn.is_h2_or_h3() {
            self.pool.evict(evict_key);
        }
        self.pool.record_stale_reuse_retry();
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
        #[cfg(all(feature = "http3", feature = "rustls"))]
        if matches!(conn.conn, HttpConnection::H3(_)) {
            return match crate::h3_transport::replay_evidence(err) {
                Some(crate::h3_transport::H3ReplayEvidence::ProvenUnprocessed) => {
                    Some(ReplayReason::ProvenUnprocessed)
                }
                Some(crate::h3_transport::H3ReplayEvidence::VersionFallback) => {
                    Some(ReplayReason::VersionFallback)
                }
                // RemoteClosing remains ambiguous until upstream h3 exposes a
                // validated GOAWAY/request-stream boundary.
                Some(crate::h3_transport::H3ReplayEvidence::Ambiguous) | None => None,
            };
        }
        Self::is_stale_connection_error(err).then_some(ReplayReason::AmbiguousTransportFailure)
    }

    pub(super) fn should_evict_after_send_failure(conn: &PooledConnection<B>, err: &Error) -> bool {
        if !conn.is_h2_or_h3() {
            return false;
        }
        if Self::stale_replay_reason(conn, err).is_some() {
            return true;
        }
        #[cfg(all(feature = "http3", feature = "rustls"))]
        if matches!(conn.conn, HttpConnection::H3(_)) {
            return crate::h3_transport::connection_is_unusable(err);
        }
        false
    }

    #[cfg(all(feature = "http3", feature = "rustls"))]
    pub(super) fn h3_failure_invalidates_alt_svc(conn: &PooledConnection<B>, err: &Error) -> bool {
        matches!(conn.conn, HttpConnection::H3(_)) && crate::h3_transport::is_endpoint_failure(err)
    }

    #[cfg(test)]
    pub(crate) fn is_stale_connection_error_pub(err: &Error) -> bool {
        Self::is_stale_connection_error(err)
    }

    pub(super) async fn send_on_connection<A: crate::observer::AttemptStartObserver>(
        conn: &mut PooledConnection<B>,
        mut request: http::Request<B>,
        url: Uri,
        attempt_observer: A,
    ) -> Result<Response, Error>
    where
        B: http_body::Body<Data = bytes::Bytes, Error = crate::error::Error>,
    {
        Self::finalize_deferred_request_headers(&mut request, conn)?;
        Self::prepare_request_target_for_connection(&mut request, conn, &url)?;

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

        Self::start_observed_attempt(&mut request, &url, &attempt_observer);

        let observed_handoff_request = crate::upgrade::HandoffRequest::capture(&request);
        let mut result = match &mut conn.conn {
            HttpConnection::H1(sender) => {
                let resp = sender.send_request(request).await?;
                let resp = resp.map(crate::response::ResponseBodySend::from_incoming);
                Ok(Response::new(resp, url))
            }
            HttpConnection::H2(sender) => {
                let request_method = request.method().clone();
                let resp = sender.send_request(request).await?;
                if let Err(error) = validate_h2_connect_tunnel_response(&request_method, &resp) {
                    conn.retire_multiplex_transport();
                    Err(error)
                } else {
                    let resp = resp.map(crate::response::ResponseBodySend::from_incoming);
                    Ok(Response::new(resp, url))
                }
            }
            #[cfg(all(feature = "http3", feature = "rustls"))]
            HttpConnection::H3(_) => Err(Error::Unsupported(
                "HTTP/3 dispatch requires a Send runtime".to_owned(),
            )),
        };

        if let (Ok(response), Some(request)) = (&mut result, observed_handoff_request)
            && let Some(metadata) = crate::upgrade::HandoffMetadata::selected(
                &request,
                response.version(),
                response.status(),
                response.headers(),
                || conn.metrics.association(),
            )
        {
            response.extensions_mut().insert(metadata);
        }

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

    pub(super) async fn try_send_on_pooled_connection<A: crate::observer::AttemptStartObserver>(
        conn: &mut PooledConnection<B>,
        mut request: http::Request<B>,
        url: Uri,
        attempt_observer: A,
    ) -> Result<Response, PooledSendError<B>>
    where
        B: http_body::Body<Data = bytes::Bytes, Error = crate::error::Error>,
    {
        Self::finalize_deferred_request_headers(&mut request, conn)
            .map_err(PooledSendError::Failed)?;
        Self::prepare_request_target_for_connection(&mut request, conn, &url)
            .map_err(PooledSendError::Failed)?;

        #[cfg(all(feature = "http3", feature = "rustls"))]
        if matches!(&conn.conn, HttpConnection::H3(_)) {
            return Self::send_on_connection(conn, request, url, attempt_observer)
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

        Self::start_observed_attempt(&mut request, &url, &attempt_observer);
        let observed_handoff_request = crate::upgrade::HandoffRequest::capture(&request);
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
            HttpConnection::H2(sender) => {
                let request_method = request.method().clone();
                match sender.try_send_request(request).await {
                    Ok(response) => {
                        if let Err(error) =
                            validate_h2_connect_tunnel_response(&request_method, &response)
                        {
                            conn.retire_multiplex_transport();
                            Err(PooledSendError::Failed(error))
                        } else {
                            Ok(response)
                        }
                    }
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
                }
            }
            #[cfg(all(feature = "http3", feature = "rustls"))]
            HttpConnection::H3(_) => unreachable!("HTTP/3 is dispatched above"),
        };

        if !matches!(result, Err(PooledSendError::Recovered { .. })) {
            conn.record_request(body_size);
        }
        let response = result.map(|mut response| {
            if let Some(request) = observed_handoff_request
                && let Some(metadata) = crate::upgrade::HandoffMetadata::selected(
                    &request,
                    response.version(),
                    response.status(),
                    response.headers(),
                    || conn.metrics.association(),
                )
            {
                response.extensions_mut().insert(metadata);
            }
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
        mut request: http::Request<crate::body::RequestBodySend>,
        url: Uri,
        write_timeout: Option<std::time::Duration>,
    ) -> Result<Response, Error>
    where
        R: crate::runtime::RuntimePoll,
    {
        if !matches!(conn.conn, HttpConnection::H3(_)) {
            return Self::send_on_connection(
                conn,
                request,
                url,
                crate::observer::NoopAttemptStartObserver,
            )
            .await;
        }
        Self::finalize_deferred_request_headers(&mut request, conn)?;
        Self::prepare_request_target_for_connection(&mut request, conn, &url)?;

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

        Self::start_observed_attempt(
            &mut request,
            &url,
            &crate::observer::NoopAttemptStartObserver,
        );

        let HttpConnection::H3(sender) = &mut conn.conn else {
            unreachable!("HTTP/3 connection changed during dispatch")
        };
        let result =
            crate::h3_transport::send_on_h3::<R>(sender, request, url, write_timeout).await;

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
        write_timeout: Option<std::time::Duration>,
    ) -> Result<Response, PooledSendError<crate::body::RequestBodySend>>
    where
        R: crate::runtime::RuntimePoll,
    {
        if matches!(conn.conn, HttpConnection::H3(_)) {
            return Self::send_on_connection_send::<R>(conn, request, url, write_timeout)
                .await
                .map_err(PooledSendError::Failed);
        }
        Self::try_send_on_pooled_connection(
            conn,
            request,
            url,
            crate::observer::NoopAttemptStartObserver,
        )
        .await
    }
}

fn validate_h2_connect_tunnel_response<B>(
    method: &http::Method,
    response: &http::Response<B>,
) -> Result<(), Error> {
    if *method == http::Method::CONNECT
        && response.status().is_success()
        && response.status() != http::StatusCode::OK
    {
        return Err(Error::Unsupported(
            "HTTP/2 CONNECT tunnel handoff requires status 200 because the HTTP/2 transport does not expose an upgrade stream for other successful statuses"
                .to_owned(),
        ));
    }
    Ok(())
}

fn prepare_automatic_host<B>(
    request: &mut http::Request<B>,
    full_uri: &Uri,
    is_h1: bool,
) -> Result<(), Error> {
    if let Some(AutomaticHost(previous)) = request.extensions_mut().remove::<AutomaticHost>()
        && request.headers().get(http::header::HOST) == Some(&previous)
        && request.headers().get_all(http::header::HOST).iter().count() == 1
    {
        request.headers_mut().remove(http::header::HOST);
    }
    // Forwarding owns its authority/Host policy, including authority-omitted
    // targets. Do not synthesize headers on top of that policy.
    if is_h1
        && !request.headers().contains_key(http::header::HOST)
        && request
            .extensions()
            .get::<crate::forward::dispatch_plan::ForwardSigningTarget>()
            .is_none()
        && let Some(authority) = full_uri.authority()
    {
        let host = http::HeaderValue::from_str(authority.as_str())
            .map_err(|error| Error::InvalidHeader(format!("invalid Host authority: {error}")))?;
        request
            .headers_mut()
            .insert(http::header::HOST, host.clone());
        request.extensions_mut().insert(AutomaticHost(host));
    }
    Ok(())
}

fn prepare_h1_request_target<B>(request: &mut http::Request<B>) -> Result<(), Error> {
    if request.method() != http::Method::CONNECT && request.uri().scheme().is_some() {
        let target = request
            .uri()
            .path_and_query()
            .map(|value| value.as_str())
            .unwrap_or("/")
            .parse::<Uri>()
            .map_err(|error| {
                Error::InvalidUrl(format!("invalid HTTP/1 request target: {error}"))
            })?;
        *request.uri_mut() = target;
    }
    Ok(())
}

fn prepare_h2_or_h3_request_target<B>(
    request: &mut http::Request<B>,
    full_uri: &Uri,
    version: http::Version,
) -> Result<(), Error> {
    if request.method() == http::Method::CONNECT {
        // Extended CONNECT retains :scheme and :path (RFC 8441 §4).
        if request.extensions().get::<hyper::ext::Protocol>().is_some() {
            return Ok(());
        }
        // HTTP/2 and HTTP/3 CONNECT use authority-form.  Leaving the
        // user-facing absolute URI in place encodes forbidden :scheme/:path
        // pseudo-headers, which peers reject with PROTOCOL_ERROR (RFC 9113
        // §8.5). The origin URI remains available to the caller;
        // only the wire request target is normalized here.
        let authority = request
            .uri()
            .authority()
            .or_else(|| full_uri.authority())
            .ok_or_else(|| Error::InvalidUrl("CONNECT request is missing authority".to_owned()))?;
        *request.uri_mut() = Uri::builder()
            .authority(authority.as_str())
            .build()
            .map_err(|error| Error::InvalidUrl(format!("invalid CONNECT authority: {error}")))?;
        return Ok(());
    }
    if request.uri().scheme().is_some() {
        return Ok(());
    }
    if request.uri().path() == "*" {
        if request.method() == http::Method::OPTIONS
            && version == http::Version::HTTP_2
            && request.version() == http::Version::HTTP_11
        {
            if full_uri.scheme() != Some(&http::uri::Scheme::HTTP) {
                return Err(Error::Unsupported(
                    "authority-omitted OPTIONS * requires an HTTP scheme for HTTP/2 translation"
                        .to_owned(),
                ));
            }
            // h2's HTTP/1 translation mode supplies `:scheme = http` while
            // preserving `:path = *` and omitting `:authority`.
            return Ok(());
        }
        return Err(Error::Unsupported(format!(
            "OPTIONS * cannot be represented with complete pseudo-headers by the {version:?} transport"
        )));
    }

    let mut parts = full_uri.clone().into_parts();
    parts.path_and_query = request.uri().path_and_query().cloned();
    *request.uri_mut() = Uri::from_parts(parts).map_err(|error| {
        Error::InvalidUrl(format!("invalid {version:?} request target: {error}"))
    })?;
    Ok(())
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
