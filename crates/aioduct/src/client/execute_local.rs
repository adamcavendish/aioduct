use bytes::Bytes;
use http::header::{AUTHORIZATION, HeaderMap};
use http::{Method, StatusCode, Uri};
use http_body_util::BodyExt;
use std::time::Duration;

use super::{HttpEngineCore, HttpEngineLocal};
use crate::body::RequestBody;
use crate::body::RequestBodyLocal;
use crate::clock::Instant;
use crate::error::Error;
use crate::observer::{self, RequestPhase};
use crate::pool::PooledConnection;
use crate::response::Response;
use crate::runtime::{ConnectorLocal, RuntimeLocal, SocketConfig};
#[allow(deprecated)]
use crate::timing::TimingCollector;

use super::execute::{CacheLookupOutcome, PostExecuteAction};

use super::extract_headers;

// ── Local path (RuntimeLocal + ConnectorLocal) ────────────────────────────────────

impl<R: RuntimeLocal, C: ConnectorLocal + Clone> HttpEngineLocal<R, C> {
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn execute_local(
        &self,
        method: Method,
        original_uri: Uri,
        headers: http::HeaderMap,
        body: Option<RequestBody>,
        version: Option<http::Version>,
        connect_timeout: Option<Duration>,
        force_addr: Option<std::net::SocketAddr>,
    ) -> Result<Response<crate::body::ResponseBodyLocal>, Error> {
        if self.core.https_only && original_uri.scheme() != Some(&http::uri::Scheme::HTTPS) {
            return Err(Error::HttpsOnly(
                original_uri.scheme_str().unwrap_or("none").to_owned(),
            ));
        }

        let site_for_cookies: String = original_uri
            .authority()
            .map(|a| a.host().to_owned())
            .unwrap_or_default();

        let mut current_uri = self.core.maybe_upgrade_hsts(original_uri);
        let mut current_method = method;
        let mut current_body = body;
        let mut current_headers = headers;

        self.core.apply_default_headers(&mut current_headers);

        for _ in 0..=self.core.redirect_policy.max_redirects() {
            self.core.prepare_request_headers(
                &current_uri,
                Some(&site_for_cookies),
                &mut current_headers,
            );

            let (req_body, body_for_replay) = match current_body.take() {
                Some(RequestBody::Buffered(b)) => {
                    let body_clone = RequestBody::Buffered(b.clone());
                    (RequestBody::Buffered(b).into_local_body(), Some(body_clone))
                }
                Some(rb @ RequestBody::Streaming(_)) => (rb.into_local_body(), None),
                None => {
                    let empty: RequestBodyLocal = Box::pin(
                        http_body_util::Full::new(Bytes::new()).map_err(|never| match never {}),
                    );
                    (empty, None)
                }
            };

            let (cache_state, stale_if_error) =
                self.core
                    .cache_lookup(&current_method, &current_uri, &mut current_headers);
            let mut cache_entry = match cache_state {
                CacheLookupOutcome::Fresh(resp) => {
                    let mut resp = *resp;
                    if !self.core.middleware.is_empty() {
                        resp.apply_middleware(&self.core.middleware, &current_uri);
                    }
                    self.core
                        .attach_observer(&mut resp, &current_method, &current_uri);
                    return Ok(resp.into_local());
                }
                CacheLookupOutcome::Stale(entry) => Some(entry),
                CacheLookupOutcome::Miss => None,
            };

            let req_uri: Uri = match current_uri.path_and_query() {
                Some(pq) => Uri::from(pq.clone()),
                None => Uri::from_static("/"),
            };

            let mut builder = http::Request::builder()
                .method(current_method.clone())
                .uri(req_uri);

            if let Some(ver) = version {
                builder = builder.version(ver);
            }

            let mut request = builder.body(req_body)?;
            *request.headers_mut() = current_headers.clone();

            if !self.core.middleware.is_empty() {
                self.core
                    .middleware
                    .apply_request_local(&mut request, &current_uri);
            }

            // Strip user-supplied framing headers to prevent request smuggling.
            // Runs AFTER middleware so middleware cannot re-inject them.
            request
                .headers_mut()
                .remove(http::header::TRANSFER_ENCODING);
            request.headers_mut().remove(http::header::CONTENT_LENGTH);

            let replay_bytes_for_stale = match body_for_replay.as_ref() {
                Some(RequestBody::Buffered(b)) => Some(b.clone()),
                _ => None,
            };

            let resp = match self
                .execute_single_local(
                    request,
                    &current_uri,
                    replay_bytes_for_stale,
                    connect_timeout,
                    force_addr,
                )
                .await
            {
                Ok(resp) => {
                    if resp.status().is_server_error()
                        && let Some(sie_duration) = stale_if_error
                        && let Some(ref cached) = cache_entry
                        && cached.age <= sie_duration
                    {
                        let _ = resp.bytes().await;
                        // SAFETY: cache_entry is guaranteed Some by the let-chain
                        // guard above. Use take() to move ownership out.
                        if let Some(cached) = cache_entry.take() {
                            let http_resp = cached.into_http_response();
                            return Ok(Response::from_boxed(http_resp, current_uri).into_local());
                        }
                        // Unreachable: if the guard matched, cache_entry was Some.
                        return Err(Error::Other(
                            "stale cache entry unexpectedly missing".into(),
                        ));
                    }
                    resp
                }
                Err(e) => {
                    if let Some(sie_duration) = stale_if_error
                        && let Some(cached) = cache_entry
                        && cached.age <= sie_duration
                    {
                        let http_resp = cached.into_http_response();
                        return Ok(Response::from_boxed(http_resp, current_uri).into_local());
                    }
                    return Err(e);
                }
            };

            let replay_bytes = match body_for_replay.as_ref() {
                Some(RequestBody::Buffered(b)) => Some(b.clone()),
                _ => None,
            };
            let resp = self
                .maybe_retry_digest_local(
                    resp,
                    &current_method,
                    &current_uri,
                    &mut current_headers,
                    replay_bytes,
                    connect_timeout,
                    force_addr,
                )
                .await?;

            match self.core.post_execute(
                &resp,
                &current_method,
                &current_uri,
                &mut current_headers,
                body_for_replay,
            )? {
                PostExecuteAction::Done => {
                    if resp.status() == StatusCode::NOT_MODIFIED
                        && let Some(cached) = cache_entry
                    {
                        let http_resp = cached.into_http_response();
                        return Ok(Response::from_boxed(http_resp, current_uri).into_local());
                    }
                    return self
                        .finalize_response_local(
                            resp,
                            &current_method,
                            current_uri,
                            &current_headers,
                        )
                        .await;
                }
                PostExecuteAction::Redirect { uri, method, body } => {
                    let _ = resp.bytes().await;
                    current_uri = uri;
                    current_method = method;
                    current_body = body;
                }
            }
        }

        Err(Error::TooManyRedirects(
            self.core.redirect_policy.max_redirects(),
        ))
    }

    #[allow(clippy::too_many_arguments)]
    async fn maybe_retry_digest_local(
        &self,
        resp: Response,
        method: &Method,
        uri: &Uri,
        headers: &mut HeaderMap,
        body_for_replay: Option<Bytes>,
        connect_timeout: Option<Duration>,
        force_addr: Option<std::net::SocketAddr>,
    ) -> Result<Response, Error> {
        let Some(ref digest) = self.core.digest_auth else {
            return Ok(resp);
        };
        if !digest.needs_retry(resp.status(), resp.headers()) {
            return Ok(resp);
        }
        let Some(auth_value) = digest.authorize(method, uri, resp.headers()) else {
            return Ok(resp);
        };

        let version = resp.version();
        let _ = resp.bytes().await;
        headers.insert(AUTHORIZATION, auth_value);

        let replay_for_stale = body_for_replay.clone();

        let retry_body: RequestBodyLocal = match body_for_replay {
            Some(b) => Box::pin(http_body_util::Full::new(b).map_err(|never| match never {})),
            None => {
                Box::pin(http_body_util::Full::new(Bytes::new()).map_err(|never| match never {}))
            }
        };

        let retry_uri: Uri = match uri.path_and_query() {
            Some(pq) => Uri::from(pq.clone()),
            None => Uri::from_static("/"),
        };
        let mut retry_builder = http::Request::builder()
            .method(method.clone())
            .uri(retry_uri);
        retry_builder = retry_builder.version(version);
        let mut retry_request = retry_builder.body(retry_body)?;
        *retry_request.headers_mut() = headers.clone();
        if !self.core.middleware.is_empty() {
            self.core
                .middleware
                .apply_request_local(&mut retry_request, uri);
        }
        // Strip framing headers on retry — after middleware.
        retry_request
            .headers_mut()
            .remove(http::header::TRANSFER_ENCODING);
        retry_request
            .headers_mut()
            .remove(http::header::CONTENT_LENGTH);
        self.execute_single_local(
            retry_request,
            uri,
            replay_for_stale,
            connect_timeout,
            force_addr,
        )
        .await
    }

    pub(super) async fn finalize_response_local(
        &self,
        resp: Response,
        method: &Method,
        uri: Uri,
        request_headers: &HeaderMap,
    ) -> Result<Response<crate::body::ResponseBodyLocal>, Error> {
        let mut resp = resp;
        if !self.core.middleware.is_empty() {
            resp.apply_middleware(&self.core.middleware, &uri);
        }

        let resp = if !self.core.accept_encoding.is_empty() {
            resp.decompress(&self.core.accept_encoding)
        } else {
            resp
        };

        let resp = if let Some(read_timeout) = self.core.read_timeout {
            resp.into_local_with_read_timeout::<R>(read_timeout)
        } else {
            resp.into_local()
        };

        let resp = if let Some(ref limiter) = self.core.bandwidth_limiter {
            resp.apply_bandwidth_limit_local::<R>(limiter.clone())
        } else {
            resp
        };

        if let Some(ref cache) = self.core.cache {
            let status = resp.status();
            let headers = resp.headers().clone();
            if crate::cache::is_response_cacheable(status, &headers) {
                let body_bytes = resp.bytes().await?;
                cache.store(method, &uri, status, &headers, &body_bytes, request_headers);
                let cached_resp = super::boxed_response_from_bytes(status, &headers, body_bytes);
                return Ok(Response::from_boxed(cached_resp, uri).into_local());
            }
        }

        Ok(resp)
    }

    #[allow(deprecated)]
    pub(crate) async fn execute_single_local(
        &self,
        mut request: http::Request<RequestBodyLocal>,
        original_uri: &Uri,
        replay_body: Option<Bytes>,
        connect_timeout: Option<Duration>,
        force_addr: Option<std::net::SocketAddr>,
    ) -> Result<Response, Error> {
        let request_start = Instant::now();
        #[allow(deprecated)]
        let timing_start = std::time::Instant::now();

        if let Some(ref limiter) = self.core.rate_limiter {
            while !limiter.try_acquire() {
                let wait = limiter.wait_duration();
                R::sleep(wait).await;
            }
        }

        self.core
            .notify(request.method(), original_uri, RequestPhase::Started);
        let pool_checkout_start = Instant::now();

        let scheme = original_uri
            .scheme()
            .ok_or_else(|| Error::InvalidUrl("missing scheme".into()))?;
        let authority = original_uri
            .authority()
            .ok_or_else(|| Error::InvalidUrl("missing authority".into()))?;

        let is_https = scheme == &http::uri::Scheme::HTTPS;

        let pool_key = crate::pool::PoolKey::new(scheme.clone(), authority.clone());
        let may_h2 = is_https || self.core.http2_prior_knowledge;

        let can_stale_retry = !self.core.no_connection_reuse
            && (http_body::Body::is_end_stream(request.body()) || replay_body.is_some());

        if !self.core.no_connection_reuse
            && let Some(mut conn) = self.core.pool.checkout(&pool_key)
        {
            self.core.notify(
                request.method(),
                original_uri,
                RequestPhase::PoolCheckoutComplete {
                    outcome: observer::PoolOutcome::Hit,
                    blocked_duration: pool_checkout_start.elapsed(),
                },
            );

            let saved_parts = if can_stale_retry {
                Some((
                    request.method().clone(),
                    request.uri().clone(),
                    request.headers().clone(),
                    request.version(),
                ))
            } else {
                None
            };

            let req_method = request.method().clone();
            let transfer_start = Instant::now();
            self.core.notify(
                &req_method,
                original_uri,
                RequestPhase::RequestSent {
                    duration: transfer_start.duration_since(pool_checkout_start),
                    headers: extract_headers(request.headers()),
                },
            );
            match HttpEngineCore::send_on_connection(&mut conn, request, original_uri.clone()).await
            {
                Ok(mut resp) => {
                    let transfer = transfer_start.elapsed();
                    self.core.notify(
                        &req_method,
                        original_uri,
                        RequestPhase::ResponseStarted {
                            waiting_duration: transfer,
                        },
                    );
                    let protocol = HttpEngineCore::connection_protocol(&conn);
                    self.core.notify(
                        &req_method,
                        original_uri,
                        RequestPhase::ResponseComplete {
                            status: resp.status(),
                            protocol,
                            total_duration: request_start.elapsed(),
                        },
                    );
                    resp.set_remote_addr(conn.remote_addr);
                    resp.set_tls_info(conn.tls_info.clone());
                    resp.set_timings(Some(
                        TimingCollector::default()
                            .into_timings(Some(transfer), timing_start.elapsed()),
                    ));
                    self.core
                        .attach_observer(&mut resp, &req_method, original_uri);
                    if let Some(handle) = conn.upgrade_handle_local.take() {
                        resp.extensions_mut().insert(handle);
                    }
                    if !HttpEngineCore::<RequestBodyLocal>::should_skip_checkin(&resp) {
                        self.core.checkin_when_ready_local::<R, _, _>(
                            pool_key,
                            conn,
                            R::spawn_local,
                            R::sleep(self.core.pool.idle_timeout()),
                            req_method == http::Method::HEAD,
                        );
                    }
                    return Ok(resp);
                }
                Err(e)
                    if saved_parts.is_some()
                        && HttpEngineCore::<RequestBodyLocal>::is_stale_connection_error(&e) =>
                {
                    if conn.is_h2_or_h3() {
                        self.core.pool.evict(&pool_key);
                    }
                    self.core.fire_connection_metrics(&conn, true);
                    // saved_parts is guaranteed Some by the match arm guard.
                    let Some((method, uri, headers, version)) = saved_parts else {
                        return Err(e);
                    };
                    let retry_body_bytes = replay_body
                        .as_ref()
                        .cloned()
                        .unwrap_or_else(bytes::Bytes::new);
                    let body: RequestBodyLocal = Box::pin(
                        http_body_util::Full::new(retry_body_bytes).map_err(|never| match never {}),
                    );
                    let mut retry_req = http::Request::new(body);
                    *retry_req.method_mut() = method;
                    *retry_req.uri_mut() = uri;
                    *retry_req.headers_mut() = headers;
                    *retry_req.version_mut() = version;
                    request = retry_req;
                }
                Err(e) => return Err(e),
            }
        }

        self.core.notify(
            request.method(),
            original_uri,
            RequestPhase::PoolCheckoutComplete {
                outcome: observer::PoolOutcome::Miss,
                blocked_duration: pool_checkout_start.elapsed(),
            },
        );

        if may_h2 && !self.core.no_connection_reuse && self.core.pool.mark_connecting_h2(&pool_key)
        {
            let wait_budget = connect_timeout.unwrap_or(std::time::Duration::from_secs(5));
            let poll_interval = std::time::Duration::from_millis(5);
            let max_polls =
                (wait_budget.as_millis() / poll_interval.as_millis().max(1)).clamp(1, 200);
            for _ in 0..max_polls {
                R::sleep(poll_interval).await;
                if let Some(mut conn) = self.core.pool.checkout(&pool_key) {
                    self.core.notify(
                        request.method(),
                        original_uri,
                        RequestPhase::PoolCheckoutComplete {
                            outcome: observer::PoolOutcome::Hit,
                            blocked_duration: pool_checkout_start.elapsed(),
                        },
                    );
                    let saved_parts = if can_stale_retry {
                        Some((
                            request.method().clone(),
                            request.uri().clone(),
                            request.headers().clone(),
                            request.version(),
                        ))
                    } else {
                        None
                    };
                    let req_method = request.method().clone();
                    let transfer_start = Instant::now();
                    self.core.notify(
                        &req_method,
                        original_uri,
                        RequestPhase::RequestSent {
                            duration: transfer_start.duration_since(pool_checkout_start),
                            headers: extract_headers(request.headers()),
                        },
                    );
                    match HttpEngineCore::send_on_connection(
                        &mut conn,
                        request,
                        original_uri.clone(),
                    )
                    .await
                    {
                        Ok(mut resp) => {
                            let transfer = transfer_start.elapsed();
                            self.core.notify(
                                &req_method,
                                original_uri,
                                RequestPhase::ResponseStarted {
                                    waiting_duration: transfer,
                                },
                            );
                            let protocol = HttpEngineCore::connection_protocol(&conn);
                            self.core.notify(
                                &req_method,
                                original_uri,
                                RequestPhase::ResponseComplete {
                                    status: resp.status(),
                                    protocol,
                                    total_duration: request_start.elapsed(),
                                },
                            );
                            resp.set_remote_addr(conn.remote_addr);
                            resp.set_tls_info(conn.tls_info.clone());
                            #[allow(deprecated)]
                            resp.set_timings(Some(
                                TimingCollector::default()
                                    .into_timings(Some(transfer), timing_start.elapsed()),
                            ));
                            self.core
                                .attach_observer(&mut resp, &req_method, original_uri);
                            if let Some(handle) = conn.upgrade_handle_local.take() {
                                resp.extensions_mut().insert(handle);
                            }
                            if !HttpEngineCore::<RequestBodyLocal>::should_skip_checkin(&resp) {
                                self.core
                                    .checkin_when_ready_local::<R, _, _>(pool_key, conn, R::spawn_local, R::sleep(self.core.pool.idle_timeout()), req_method == http::Method::HEAD);
                            }
                            return Ok(resp);
                        }
                        Err(e)
                            if saved_parts.is_some()
                                && HttpEngineCore::<RequestBodyLocal>::is_stale_connection_error(
                                    &e,
                                ) =>
                        {
                            if conn.is_h2_or_h3() {
                                self.core.pool.evict(&pool_key);
                            }
                            self.core.fire_connection_metrics(&conn, true);
                            let Some((method, uri, headers, version)) = saved_parts else {
                                return Err(e);
                            };
                            let retry_body_bytes = replay_body
                                .as_ref()
                                .cloned()
                                .unwrap_or_else(bytes::Bytes::new);
                            let body: RequestBodyLocal = Box::pin(
                                http_body_util::Full::new(retry_body_bytes)
                                    .map_err(|never| match never {}),
                            );
                            let mut retry_req = http::Request::new(body);
                            *retry_req.method_mut() = method;
                            *retry_req.uri_mut() = uri;
                            *retry_req.headers_mut() = headers;
                            *retry_req.version_mut() = version;
                            request = retry_req;
                            break;
                        }
                        Err(e) => return Err(e),
                    }
                }
            }
            // Timed out waiting — connect ourselves.
            // Just ensure the mark is set; don't unmark first (avoids TOCTOU race).
            self.core.pool.mark_connecting_h2(&pool_key);
        }

        let mut h2_guard = super::dispatch::H2ConnectGuard {
            pool: &self.core.pool,
            key: &pool_key,
            active: may_h2,
        };

        let proxy = self
            .core
            .proxy
            .as_ref()
            .and_then(|settings| settings.proxy_for(original_uri));

        let mut timing = TimingCollector::default();

        let mut pooled = if let Some(ref chain) = self.core.proxy_chain {
            self.connect_via_proxy_chain_local(chain, authority, is_https, connect_timeout)
                .await?
        } else if let Some(ref proxy) = proxy {
            self.connect_via_proxy_local(proxy, authority, is_https, connect_timeout)
                .await?
        } else {
            let default_port = if is_https { 443 } else { 80 };
            let host = authority.host();
            let port = authority.port_u16().unwrap_or(default_port);

            let dns_start = Instant::now();
            let addrs = if let Some(addr) = force_addr {
                vec![addr]
            } else {
                self.core.resolve_all_authority_raw(host, port).await?
            };
            timing.dns = Some(dns_start.elapsed());
            self.core.notify(
                request.method(),
                original_uri,
                RequestPhase::DnsResolved {
                    addrs: addrs.clone(),
                    duration: dns_start.elapsed(),
                },
            );

            let tcp_start = Instant::now();
            let connect_fut = async {
                let local_address = self.core.local_address;
                let (tcp_stream, addr) = if addrs.len() > 1 {
                    #[cfg(feature = "tower")]
                    let _ = original_uri;
                    crate::happy_eyeballs::connect_happy_eyeballs_local::<R, C>(
                        &self.connector,
                        &addrs,
                        local_address,
                    )
                    .await
                    .map_err(Error::Io)?
                } else {
                    let addr = addrs[0];
                    let stream = if let Some(local_addr) = local_address {
                        self.connector
                            .connect_bound(addr, local_addr)
                            .await
                            .map_err(Error::Io)?
                    } else {
                        #[cfg(feature = "tower")]
                        if let Some(ref tower_slot) = self.tower_connector_local {
                            let tower_conn = tower_slot.get::<C>();
                            let info = crate::connector::ConnectInfo {
                                uri: original_uri.clone(),
                                addr,
                            };
                            tower_conn.connect(info).await.map_err(Error::Io)?
                        } else {
                            self.connector.connect(addr).await.map_err(Error::Io)?
                        }
                        #[cfg(not(feature = "tower"))]
                        self.connector.connect(addr).await.map_err(Error::Io)?
                    };
                    (stream, addr)
                };

                if let Some(time) = self.core.tcp_keepalive {
                    tcp_stream
                        .set_keepalive(
                            time,
                            self.core.tcp_keepalive_interval,
                            self.core.tcp_keepalive_retries,
                        )
                        .map_err(Error::Io)?;
                }
                if self.core.tcp_fast_open {
                    let _ = tcp_stream.set_fast_open();
                }

                let mut conn = if is_https {
                    self.connect_tls_local(tcp_stream, authority.host()).await?
                } else {
                    self.connect_plaintext_local(tcp_stream).await?
                };
                conn.remote_addr = Some(addr);
                Ok::<(PooledConnection<RequestBodyLocal>, Instant), Error>((conn, Instant::now()))
            };

            let (conn, connect_done) = match connect_timeout {
                Some(duration) => {
                    crate::timeout::Timeout::WithTimeout {
                        future: connect_fut,
                        sleep: R::sleep(duration),
                    }
                    .await?
                }
                None => connect_fut.await?,
            };
            let tcp_tls_elapsed = connect_done.duration_since(tcp_start);
            if is_https {
                if let Some(tls_dur) = conn.tls_handshake_duration {
                    timing.tls_handshake = Some(tls_dur);
                    timing.tcp_connect = Some(tcp_tls_elapsed.saturating_sub(tls_dur));
                    let tcp_dur = tcp_tls_elapsed.saturating_sub(tls_dur);
                    if let Some(addr) = conn.remote_addr {
                        self.core.notify(
                            request.method(),
                            original_uri,
                            RequestPhase::TcpConnected {
                                remote_addr: addr,
                                duration: tcp_dur,
                                protocol: HttpEngineCore::connection_protocol(&conn),
                            },
                        );
                    }
                    self.core.notify(
                        request.method(),
                        original_uri,
                        RequestPhase::TlsHandshakeComplete {
                            duration: tls_dur,
                            alpn_protocol: match &conn.conn {
                                crate::pool::HttpConnection::H2(_) => Some("h2".into()),
                                crate::pool::HttpConnection::H1(_) => Some("http/1.1".into()),
                                #[cfg(all(feature = "http3", feature = "rustls"))]
                                crate::pool::HttpConnection::H3(_) => Some("h3".into()),
                            },
                            peer_certificate_der: conn
                                .tls_info
                                .as_ref()
                                .and_then(|t| t.peer_certificate())
                                .map(|c| c.to_vec()),
                        },
                    );
                } else {
                    timing.tcp_connect = Some(tcp_tls_elapsed);
                    if let Some(addr) = conn.remote_addr {
                        self.core.notify(
                            request.method(),
                            original_uri,
                            RequestPhase::TcpConnected {
                                remote_addr: addr,
                                duration: tcp_tls_elapsed,
                                protocol: HttpEngineCore::connection_protocol(&conn),
                            },
                        );
                    }
                }
            } else {
                timing.tcp_connect = Some(tcp_tls_elapsed);
                if let Some(addr) = conn.remote_addr {
                    self.core.notify(
                        request.method(),
                        original_uri,
                        RequestPhase::TcpConnected {
                            remote_addr: addr,
                            duration: tcp_tls_elapsed,
                            protocol: HttpEngineCore::connection_protocol(&conn),
                        },
                    );
                }
            }
            conn
        };

        // H2 multiplexing: deactivate the guard and handle connection sharing
        h2_guard.active = false;
        drop(h2_guard);

        let is_multiplex = pooled.is_h2_or_h3() && !self.core.no_connection_reuse;
        if is_multiplex {
            if let Some(existing) = self.core.pool.checkout(&pool_key) {
                drop(pooled);
                pooled = existing;
            } else if let Some(cloned) = pooled
                .clone_for_multiplex_with_limit(self.core.pool.max_active_streams_per_connection())
            {
                self.core.checkin_connection(pool_key.clone(), pooled);
                pooled = cloned;
            }
            self.core.pool.unmark_connecting_h2(&pool_key);
        } else if may_h2 {
            self.core.pool.unmark_connecting_h2(&pool_key);
        }

        if let Some(ref proxy) = proxy
            && !is_https
            && proxy.scheme == crate::proxy::ProxyScheme::Http
            && let Some(auth_value) = proxy.connect_header("")
        {
            request.headers_mut().insert(
                http::header::PROXY_AUTHORIZATION,
                http::header::HeaderValue::from_str(&auth_value)
                    .unwrap_or_else(|_| http::header::HeaderValue::from_static("")),
            );
        }

        let req_method = request.method().clone();
        let transfer_start = Instant::now();
        self.core.notify(
            &req_method,
            original_uri,
            RequestPhase::RequestSent {
                duration: transfer_start.duration_since(pool_checkout_start),
                headers: extract_headers(request.headers()),
            },
        );
        let mut resp =
            HttpEngineCore::send_on_connection(&mut pooled, request, original_uri.clone()).await?;
        let transfer = transfer_start.elapsed();
        self.core.notify(
            &req_method,
            original_uri,
            RequestPhase::ResponseStarted {
                waiting_duration: transfer,
            },
        );
        let resp_protocol = HttpEngineCore::connection_protocol(&pooled);
        self.core.notify(
            &req_method,
            original_uri,
            RequestPhase::ResponseComplete {
                status: resp.status(),
                protocol: resp_protocol,
                total_duration: request_start.elapsed(),
            },
        );
        resp.set_remote_addr(pooled.remote_addr);
        resp.set_tls_info(pooled.tls_info.clone());
        resp.set_timings(Some(
            timing.into_timings(Some(transfer), timing_start.elapsed()),
        ));
        self.core
            .attach_observer(&mut resp, &req_method, original_uri);
        if let Some(handle) = pooled.upgrade_handle_local.take() {
            resp.extensions_mut().insert(handle);
        }
        if !self.core.no_connection_reuse
            && !HttpEngineCore::<RequestBodyLocal>::should_skip_checkin(&resp)
        {
            self.core.checkin_when_ready_local::<R, _, _>(
                pool_key,
                pooled,
                R::spawn_local,
                R::sleep(self.core.pool.idle_timeout()),
                req_method == http::Method::HEAD,
            );
        }

        Ok(resp)
    }
}
