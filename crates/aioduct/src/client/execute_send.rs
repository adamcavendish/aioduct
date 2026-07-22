use bytes::Bytes;
use http::header::{AUTHORIZATION, HeaderMap};
use http::{Method, StatusCode, Uri};
use http_body_util::BodyExt;
use std::time::Duration;

use super::replay::{
    FinalizedCacheState, FinalizedRequestSnapshot, RetryEligibilityPermit, audit_send_body,
};
use super::{BodyReplayability, FinalizedRequestState, HttpEngineSend};
use crate::body::{RequestBody, RequestBodySend};
use crate::digest_fields::ContentDigestBody;
use crate::error::Error;
use crate::observer::RequestPhase;
use crate::response::Response;
use crate::runtime::{ConnectorSend, RuntimePoll};

use super::request_flow::{CacheLookupOutcome, PostExecuteAction};

impl<R: RuntimePoll, C: ConnectorSend> HttpEngineSend<R, C> {
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn execute_send(
        &self,
        method: Method,
        original_uri: Uri,
        headers: http::HeaderMap,
        body: Option<RequestBody>,
        version: Option<http::Version>,
        connect_timeout: Option<Duration>,
        write_timeout: Option<Duration>,
        read_timeout: Option<Duration>,
        no_decompression: bool,
        force_addr: Option<std::net::SocketAddr>,
        protocol_hint: crate::pool::ProtocolHint,
        automatic_content_digest: bool,
        mut original_fragment: Option<String>,
        finalized_request: Option<&std::sync::Mutex<FinalizedRequestState>>,
    ) -> Result<Response, Error> {
        let mut replay_snapshot = finalized_request.and_then(|state| {
            state
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .take_pending_replay()
        });
        if let (Some(snapshot), Some(digest)) =
            (replay_snapshot.as_mut(), self.core.digest_auth.as_ref())
        {
            snapshot.refresh_digest_authorization(digest);
        }
        if let Some(snapshot) = replay_snapshot.as_ref() {
            original_fragment = snapshot.fragment().map(str::to_owned);
        }
        let first_uri = replay_snapshot
            .as_ref()
            .map(FinalizedRequestSnapshot::effective_uri)
            .unwrap_or(&original_uri);
        if self.core.https_only && first_uri.scheme() != Some(&http::uri::Scheme::HTTPS) {
            return Err(Error::HttpsOnly(
                first_uri.scheme_str().unwrap_or("none").to_owned(),
            ));
        }

        let site_for_cookies: String = original_uri
            .authority()
            .map(|a| a.host().to_owned())
            .unwrap_or_default();

        let mut current_uri = replay_snapshot
            .as_ref()
            .map(|snapshot| snapshot.effective_uri().clone())
            .unwrap_or_else(|| self.core.maybe_upgrade_hsts(original_uri));
        let mut current_method = replay_snapshot
            .as_ref()
            .map(|snapshot| snapshot.method().clone())
            .unwrap_or(method);
        let mut current_body = body;
        let mut current_headers = replay_snapshot
            .as_ref()
            .map(|snapshot| snapshot.headers().clone())
            .unwrap_or(headers);

        if replay_snapshot.is_none() {
            self.core
                .apply_default_headers_with(&mut current_headers, no_decompression);
        }

        for _ in 0..=self.core.redirect_policy.max_redirects() {
            let (
                mut request,
                body_for_replay,
                body_replayability,
                body_audit,
                replay_bytes_for_snapshot,
                mut cache_entry,
                stale_if_error,
                mut cache_request_headers,
                finalized_cache_state,
            ) = if let Some(snapshot) = replay_snapshot.take() {
                current_uri = snapshot.effective_uri().clone();
                current_headers = snapshot.headers().clone();
                let applied_cookie_header = self.core.refresh_replay_headers(
                    &current_uri,
                    Some(&site_for_cookies),
                    snapshot.applied_cookie_header(),
                    &mut current_headers,
                );

                let replay_body = snapshot.body_bytes();
                let req_body: RequestBodySend = http_body_util::Full::new(replay_body)
                    .map_err(|never| match never {})
                    .boxed_unsync();
                let req_body = match write_timeout {
                    Some(duration) => {
                        crate::timeout::WriteTimeoutBody::<_, R>::new(req_body, duration)
                            .map_err(|error| error)
                            .boxed_unsync()
                    }
                    None => req_body,
                };
                let finalized_cache_state = snapshot.cache_state().clone();
                let cache_entry = finalized_cache_state.cache_entry();
                let stale_if_error = finalized_cache_state.stale_if_error();
                let mut cache_request_headers = finalized_cache_state.request_headers().clone();
                match current_headers.get(http::header::COOKIE).cloned() {
                    Some(cookie) => {
                        cache_request_headers.insert(http::header::COOKIE, cookie);
                    }
                    None => {
                        cache_request_headers.remove(http::header::COOKIE);
                    }
                }
                let mut request = snapshot.to_request(req_body);
                *request.headers_mut() = current_headers.clone();
                if let Some(applied_cookie_header) = applied_cookie_header {
                    request
                        .extensions_mut()
                        .insert(super::replay::AppliedCookieHeader(applied_cookie_header));
                }
                let body_for_replay = snapshot.stale_replay_bytes().map(RequestBody::Buffered);
                let replay_bytes_for_snapshot = snapshot.stale_replay_bytes();

                (
                    request,
                    body_for_replay,
                    snapshot.body_replayability(),
                    None,
                    replay_bytes_for_snapshot,
                    cache_entry,
                    stale_if_error,
                    cache_request_headers,
                    finalized_cache_state,
                )
            } else {
                if let Some(finalized_request) = finalized_request {
                    finalized_request
                        .lock()
                        .unwrap_or_else(|error| error.into_inner())
                        .clear_replay_snapshot();
                }
                let applied_cookie_header = self.core.prepare_request_headers_tracking(
                    &current_uri,
                    Some(&site_for_cookies),
                    &mut current_headers,
                );

                let (req_body, mut body_for_replay, mut digest_body, mut body_replayability) =
                    match current_body.take() {
                        Some(RequestBody::Buffered(body)) => {
                            let body_clone = RequestBody::Buffered(body.clone());
                            let digest_body = ContentDigestBody::Buffered(body.clone());
                            (
                                RequestBody::Buffered(body).into_hyper_body(),
                                Some(body_clone),
                                digest_body,
                                BodyReplayability::Replayable,
                            )
                        }
                        Some(body @ RequestBody::Streaming(_)) => (
                            body.into_hyper_body(),
                            None,
                            ContentDigestBody::Unavailable,
                            BodyReplayability::OneShot,
                        ),
                        None => {
                            let empty: RequestBodySend = http_body_util::Full::new(Bytes::new())
                                .map_err(|never| match never {})
                                .boxed_unsync();
                            (
                                empty,
                                None,
                                ContentDigestBody::None,
                                BodyReplayability::Empty,
                            )
                        }
                    };

                let req_body = match write_timeout {
                    Some(duration) => {
                        crate::timeout::WriteTimeoutBody::<_, R>::new(req_body, duration)
                            .map_err(|error| error)
                            .boxed_unsync()
                    }
                    None => req_body,
                };
                let req_uri: Uri = match current_uri.path_and_query() {
                    Some(path_and_query) => Uri::from(path_and_query.clone()),
                    None => Uri::from_static("/"),
                };
                let mut builder = http::Request::builder()
                    .method(current_method.clone())
                    .uri(req_uri);
                if let Some(version) = version {
                    builder = builder.version(version);
                }

                let mut request = builder.body(req_body)?;
                *request.headers_mut() = current_headers.clone();
                if let Some(applied_cookie_header) = applied_cookie_header {
                    request
                        .extensions_mut()
                        .insert(super::replay::AppliedCookieHeader(applied_cookie_header));
                }
                let middleware_replay_body = match body_for_replay.as_ref() {
                    Some(RequestBody::Buffered(body)) => Some(body.clone()),
                    _ => None,
                };
                let replay_bytes_for_snapshot = middleware_replay_body.clone();
                let mut body_audit = None;
                if !self.core.middleware.is_empty() {
                    self.core
                        .middleware
                        .apply_request(&mut request, &current_uri);
                    digest_body = ContentDigestBody::Unavailable;
                    body_for_replay = None;
                    if let Some(expected) = middleware_replay_body {
                        let placeholder = http_body_util::Full::new(Bytes::new())
                            .map_err(|never| match never {})
                            .boxed_unsync();
                        let body = std::mem::replace(request.body_mut(), placeholder);
                        let (body, audit) = audit_send_body(body, expected);
                        *request.body_mut() = body;
                        body_audit = Some(audit);
                        body_replayability = BodyReplayability::OneShot;
                    } else {
                        body_replayability =
                            BodyReplayability::after_middleware(body_replayability, request.body());
                    }
                }
                current_method = request.method().clone();
                if let Some(finalized_request) = finalized_request {
                    finalized_request
                        .lock()
                        .unwrap_or_else(|error| error.into_inner())
                        .record_finalized(current_method.clone(), body_replayability, None);
                }

                request
                    .headers_mut()
                    .remove(http::header::TRANSFER_ENCODING);
                request.headers_mut().remove(http::header::CONTENT_LENGTH);
                self.core.apply_automatic_content_digest(
                    automatic_content_digest,
                    request.headers_mut(),
                    &digest_body,
                )?;

                let (cache_state, stale_if_error) =
                    self.core
                        .cache_lookup(&current_method, &current_uri, request.headers_mut());
                let cache_entry = match cache_state {
                    CacheLookupOutcome::Fresh(response) => {
                        let mut response = *response;
                        if !self.core.middleware.is_empty() {
                            response.apply_middleware(&self.core.middleware, &current_uri);
                        }
                        self.core
                            .attach_observer(&mut response, &current_method, &current_uri);
                        response.set_fragment(original_fragment.clone());
                        return Ok(response);
                    }
                    CacheLookupOutcome::Stale(entry) => Some(entry),
                    CacheLookupOutcome::Miss => None,
                };
                sync_cache_validators(request.headers(), &mut current_headers);
                let cache_request_headers = request.headers().clone();
                let finalized_cache_state = FinalizedCacheState::new(
                    cache_entry.clone(),
                    stale_if_error,
                    cache_request_headers.clone(),
                );

                (
                    request,
                    body_for_replay,
                    body_replayability,
                    body_audit,
                    replay_bytes_for_snapshot,
                    cache_entry,
                    stale_if_error,
                    cache_request_headers,
                    finalized_cache_state,
                )
            };

            if let Some(signature) = self
                .core
                .prepare_final_request_signature(&current_uri, &mut request)?
            {
                let signature_headers = signature.sign_send().await?;
                signature_headers.insert_into(request.headers_mut())?;
            }

            current_method = request.method().clone();
            let replay_bytes_for_stale = match body_for_replay.as_ref() {
                Some(RequestBody::Buffered(b)) => Some(b.clone()),
                _ => None,
            };
            let finalized_snapshot = FinalizedRequestSnapshot::capture(
                &request,
                &current_uri,
                replay_bytes_for_snapshot,
                body_replayability,
                body_audit,
                finalized_cache_state,
                original_fragment.clone(),
            );
            if let Some(finalized_request) = finalized_request {
                finalized_request
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .record_finalized(
                        current_method.clone(),
                        body_replayability,
                        finalized_snapshot.clone(),
                    );
            }

            let resp = match self
                .execute_single_with_hint_send(
                    request,
                    &current_uri,
                    protocol_hint,
                    replay_bytes_for_stale,
                    connect_timeout,
                    write_timeout,
                    None,
                    force_addr,
                    true,
                    body_replayability,
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
                            let mut response = Response::from_boxed(http_resp, current_uri);
                            response.set_fragment(original_fragment.clone());
                            return Ok(response);
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
                        let mut response = Response::from_boxed(http_resp, current_uri);
                        response.set_fragment(original_fragment.clone());
                        return Ok(response);
                    }
                    return Err(e);
                }
            };

            let resp = self
                .maybe_retry_digest(
                    resp,
                    &mut current_headers,
                    finalized_snapshot.as_ref(),
                    connect_timeout,
                    write_timeout,
                    force_addr,
                    protocol_hint,
                    finalized_request,
                )
                .await?;
            if let Some(value) = current_headers.get(AUTHORIZATION).cloned() {
                cache_request_headers.insert(AUTHORIZATION, value);
            }

            let body_for_redirect = finalized_snapshot
                .as_ref()
                .and_then(FinalizedRequestSnapshot::stale_replay_bytes)
                .map(RequestBody::Buffered)
                .or(body_for_replay);
            match self.core.post_execute(
                &resp,
                &current_method,
                &current_uri,
                &mut current_headers,
                body_for_redirect,
                original_fragment.as_deref(),
            )? {
                PostExecuteAction::Done => {
                    if resp.status() == StatusCode::NOT_MODIFIED
                        && let Some(cached) = cache_entry
                    {
                        let http_resp = cached.into_http_response();
                        let mut final_resp = Response::from_boxed(http_resp, current_uri);
                        final_resp.set_fragment(original_fragment);
                        return Ok(final_resp);
                    }
                    let mut final_resp = self
                        .finalize_response_send(
                            resp,
                            &current_method,
                            current_uri,
                            &cache_request_headers,
                            read_timeout,
                            no_decompression,
                        )
                        .await?;
                    final_resp.set_fragment(original_fragment);
                    return Ok(final_resp);
                }
                PostExecuteAction::Redirect {
                    uri,
                    method,
                    body,
                    fragment,
                } => {
                    let _ = resp.bytes().await;
                    current_uri = uri;
                    current_method = method;
                    current_body = body;
                    // Update effective fragment for next redirect hop.
                    // Location fragment takes priority; original fragment inherited
                    // when Location has none (handled in resolve_redirect).
                    original_fragment = fragment;
                }
            }
        }

        Err(Error::TooManyRedirects(
            self.core.redirect_policy.max_redirects(),
        ))
    }

    #[allow(clippy::too_many_arguments)]
    async fn maybe_retry_digest(
        &self,
        resp: Response,
        headers: &mut HeaderMap,
        snapshot: Option<&FinalizedRequestSnapshot>,
        connect_timeout: Option<Duration>,
        write_timeout: Option<Duration>,
        force_addr: Option<std::net::SocketAddr>,
        protocol_hint: crate::pool::ProtocolHint,
        finalized_request: Option<&std::sync::Mutex<FinalizedRequestState>>,
    ) -> Result<Response, Error> {
        let Some(ref digest) = self.core.digest_auth else {
            return Ok(resp);
        };
        if !digest.needs_retry(resp.status(), resp.headers()) {
            return Ok(resp);
        }
        let Some(snapshot) = snapshot else {
            return Ok(resp);
        };
        if !snapshot.is_replayable() {
            return Ok(resp);
        }

        let challenge_headers = resp.headers().clone();
        let Some(challenge) = digest.prepare(&challenge_headers) else {
            return Ok(resp);
        };
        if !digest.can_authorize_prepared(snapshot.method(), snapshot.request_uri(), &challenge) {
            return Ok(resp);
        }
        let retry_eligibility = if let Some(finalized_request) = finalized_request {
            let Some(permit) = RetryEligibilityPermit::try_new(finalized_request) else {
                return Ok(resp);
            };
            Some(permit)
        } else {
            None
        };

        resp.bytes().await?;
        let auth_value = digest
            .authorize_prepared(snapshot.method(), snapshot.request_uri(), &challenge)
            .ok_or_else(|| {
                Error::Other("digest authorization failed after replay quiescence".into())
            })?;
        let (attempt, max_retries) = match retry_eligibility {
            Some(permit) => permit.commit(),
            None => (1, 1),
        };

        let retry_reason = Error::Other("digest authentication challenge".into());
        self.core.notify(
            snapshot.method(),
            snapshot.effective_uri(),
            RequestPhase::Retrying {
                reason: retry_reason.to_string(),
                attempt,
                max_retries,
                backoff: Duration::ZERO,
            },
        );
        if !self.core.middleware.is_empty() {
            self.core.middleware.apply_retry(
                &retry_reason,
                snapshot.effective_uri(),
                snapshot.method(),
                attempt,
            );
        }

        let authenticated = snapshot.with_digest_authorization(auth_value, challenge);
        let replay_for_stale = authenticated.stale_replay_bytes();
        let retry_body: RequestBodySend = http_body_util::Full::new(authenticated.body_bytes())
            .map_err(|never| match never {})
            .boxed_unsync();
        let retry_body = match write_timeout {
            Some(duration) => crate::timeout::WriteTimeoutBody::<_, R>::new(retry_body, duration)
                .map_err(|error| error)
                .boxed_unsync(),
            None => retry_body,
        };
        let mut retry_request = authenticated.to_request(retry_body);
        *headers = authenticated.headers().clone();
        if let Some(signature) = self
            .core
            .prepare_final_request_signature(authenticated.effective_uri(), &mut retry_request)?
        {
            let signature_headers = signature.sign_send().await?;
            signature_headers.insert_into(retry_request.headers_mut())?;
        }
        let authenticated = authenticated.with_request_head_from(&retry_request);
        if let Some(finalized_request) = finalized_request {
            finalized_request
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .record_finalized(
                    authenticated.method().clone(),
                    authenticated.body_replayability(),
                    Some(authenticated.clone()),
                );
        }

        let effective_uri = authenticated.effective_uri().clone();
        let body_replayability = authenticated.body_replayability();
        if let Some(finalized_request) = finalized_request {
            finalized_request
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .commit_retry_reservation();
        }
        self.execute_single_with_hint_send(
            retry_request,
            &effective_uri,
            protocol_hint,
            replay_for_stale,
            connect_timeout,
            write_timeout,
            None,
            force_addr,
            true,
            body_replayability,
        )
        .await
    }

    pub(super) async fn finalize_response_send(
        &self,
        resp: Response,
        method: &Method,
        uri: Uri,
        request_headers: &HeaderMap,
        read_timeout: Option<Duration>,
        no_decompression: bool,
    ) -> Result<Response, Error> {
        #[cfg(all(feature = "http3", feature = "rustls"))]
        if self.core.h3_endpoint.is_some() {
            self.core.cache_alt_svc(&uri, resp.headers());
        }
        let mut resp = resp;
        if !self.core.middleware.is_empty() {
            resp.apply_middleware(&self.core.middleware, &uri);
        }

        let resp = if !no_decompression && !self.core.accept_encoding.is_empty() {
            resp.decompress(&self.core.accept_encoding)
        } else {
            resp
        };

        let resp = if let Some(read_timeout) = read_timeout {
            resp.apply_read_timeout::<R>(read_timeout)
        } else {
            resp
        };

        let resp = if let Some(ref limiter) = self.core.bandwidth_limiter {
            resp.apply_bandwidth_limit::<R>(limiter.clone())
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
                return Ok(Response::from_boxed(cached_resp, uri));
            }
        }

        Ok(resp)
    }
}

fn sync_cache_validators(source: &HeaderMap, target: &mut HeaderMap) {
    for name in [http::header::IF_NONE_MATCH, http::header::IF_MODIFIED_SINCE] {
        if let Some(value) = source.get(&name).cloned() {
            target.insert(name, value);
        }
    }
}
