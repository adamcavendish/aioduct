use bytes::Bytes;
use http::header::{AUTHORIZATION, HeaderMap};
use http::{Method, StatusCode, Uri};
use http_body_util::BodyExt;
use std::time::Duration;

use super::{BodyReplayability, HttpEngineLocal};
use crate::body::RequestBody;
use crate::body::RequestBodyLocal;
use crate::digest_fields::ContentDigestBody;
use crate::error::Error;
use crate::response::Response;
use crate::runtime::{ConnectorLocal, RuntimeLocal};

use super::request_flow::{CacheLookupOutcome, PostExecuteAction};

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
        write_timeout: Option<Duration>,
        read_timeout: Option<Duration>,
        no_decompression: bool,
        force_addr: Option<std::net::SocketAddr>,
        protocol_hint: crate::pool::ProtocolHint,
        automatic_content_digest: bool,
        mut original_fragment: Option<String>,
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

        self.core
            .apply_default_headers_with(&mut current_headers, no_decompression);

        for _ in 0..=self.core.redirect_policy.max_redirects() {
            self.core.prepare_request_headers(
                &current_uri,
                Some(&site_for_cookies),
                &mut current_headers,
            );

            let (req_body, body_for_replay, mut digest_body, body_replayability) =
                match current_body.take() {
                    Some(RequestBody::Buffered(b)) => {
                        let body_clone = RequestBody::Buffered(b.clone());
                        let digest_body = ContentDigestBody::Buffered(b.clone());
                        (
                            RequestBody::Buffered(b).into_local_body(),
                            Some(body_clone),
                            digest_body,
                            BodyReplayability::Replayable,
                        )
                    }
                    Some(rb @ RequestBody::Streaming(_)) => (
                        rb.into_local_body(),
                        None,
                        ContentDigestBody::Unavailable,
                        BodyReplayability::OneShot,
                    ),
                    None => {
                        let empty: RequestBodyLocal = Box::pin(
                            http_body_util::Full::new(Bytes::new()).map_err(|never| match never {}),
                        );
                        (
                            empty,
                            None,
                            ContentDigestBody::None,
                            BodyReplayability::Empty,
                        )
                    }
                };

            // Apply write timeout to the request body if configured.
            let req_body = match write_timeout {
                Some(duration) => {
                    let timeout_body =
                        crate::timeout::WriteTimeoutBody::<_, R>::new(req_body, duration);
                    Box::pin(timeout_body)
                }
                None => req_body,
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

            if !self.core.middleware.is_empty()
                && self
                    .core
                    .middleware
                    .apply_request_local(&mut request, &current_uri)
            {
                digest_body = ContentDigestBody::Unavailable;
            }

            // Strip user-supplied framing headers to prevent request smuggling.
            // Runs AFTER middleware so middleware cannot re-inject them.
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
            sync_cache_validators(request.headers(), &mut current_headers);
            let mut cache_request_headers = request.headers().clone();
            if let Some(signature) = self
                .core
                .prepare_final_request_signature(&current_uri, &mut request)?
            {
                let signature_headers = signature.sign_local().await?;
                signature_headers.insert_into(request.headers_mut())?;
            }

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
                    write_timeout,
                    None,
                    force_addr,
                    protocol_hint,
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
                    write_timeout,
                    force_addr,
                    protocol_hint,
                    automatic_content_digest,
                    digest_body.clone(),
                )
                .await?;
            if let Some(value) = current_headers.get(AUTHORIZATION).cloned() {
                cache_request_headers.insert(AUTHORIZATION, value);
            }

            match self.core.post_execute(
                &resp,
                &current_method,
                &current_uri,
                &mut current_headers,
                body_for_replay,
                original_fragment.as_deref(),
            )? {
                PostExecuteAction::Done => {
                    if resp.status() == StatusCode::NOT_MODIFIED
                        && let Some(cached) = cache_entry
                    {
                        let http_resp = cached.into_http_response();
                        let mut final_resp =
                            Response::from_boxed(http_resp, current_uri).into_local();
                        final_resp.set_fragment(original_fragment);
                        return Ok(final_resp);
                    }
                    let mut final_resp = self
                        .finalize_response_local(
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
                    original_fragment = fragment;
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
        write_timeout: Option<Duration>,
        force_addr: Option<std::net::SocketAddr>,
        protocol_hint: crate::pool::ProtocolHint,
        automatic_content_digest: bool,
        mut digest_body: ContentDigestBody,
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
        let body_replayability = if replay_for_stale.is_some() {
            BodyReplayability::Replayable
        } else {
            BodyReplayability::Empty
        };

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
        if !self.core.middleware.is_empty()
            && self
                .core
                .middleware
                .apply_request_local(&mut retry_request, uri)
        {
            digest_body = ContentDigestBody::Unavailable;
        }
        // Strip framing headers on retry — after middleware.
        retry_request
            .headers_mut()
            .remove(http::header::TRANSFER_ENCODING);
        retry_request
            .headers_mut()
            .remove(http::header::CONTENT_LENGTH);
        self.core.apply_automatic_content_digest(
            automatic_content_digest,
            retry_request.headers_mut(),
            &digest_body,
        )?;
        if let Some(signature) = self
            .core
            .prepare_final_request_signature(uri, &mut retry_request)?
        {
            let signature_headers = signature.sign_local().await?;
            signature_headers.insert_into(retry_request.headers_mut())?;
        }
        self.execute_single_local(
            retry_request,
            uri,
            replay_for_stale,
            connect_timeout,
            write_timeout,
            None,
            force_addr,
            protocol_hint,
            true,
            body_replayability,
        )
        .await
    }

    pub(super) async fn finalize_response_local(
        &self,
        resp: Response,
        method: &Method,
        uri: Uri,
        request_headers: &HeaderMap,
        read_timeout: Option<Duration>,
        no_decompression: bool,
    ) -> Result<Response<crate::body::ResponseBodyLocal>, Error> {
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
}

fn sync_cache_validators(source: &HeaderMap, target: &mut HeaderMap) {
    for name in [http::header::IF_NONE_MATCH, http::header::IF_MODIFIED_SINCE] {
        if let Some(value) = source.get(&name).cloned() {
            target.insert(name, value);
        }
    }
}
