use bytes::Bytes;
use http::header::{AUTHORIZATION, HOST, HeaderMap};
use http::{Method, StatusCode, Uri};
use http_body_util::BodyExt;

use super::HttpEngineSend;
use crate::body::{RequestBody, RequestBoxBody};
use crate::error::Error;
use crate::redirect::RedirectPolicy;
use crate::response::Response;
use crate::runtime::{ConnectorSend, RuntimePoll};

use super::execute::CacheLookupOutcome;

impl<R: RuntimePoll, C: ConnectorSend> HttpEngineSend<R, C> {
    pub(crate) async fn execute(
        &self,
        method: Method,
        original_uri: Uri,
        headers: http::HeaderMap,
        body: Option<RequestBody>,
        version: Option<http::Version>,
    ) -> Result<Response, Error> {
        if self.core.https_only && original_uri.scheme() != Some(&http::uri::Scheme::HTTPS) {
            return Err(Error::HttpsOnly(
                original_uri.scheme_str().unwrap_or("none").to_owned(),
            ));
        }

        let mut current_uri = self.core.maybe_upgrade_hsts(original_uri);
        let mut current_method = method;
        let mut current_body = body;
        let mut current_headers = headers;

        self.core.apply_default_headers(&mut current_headers);

        for _ in 0..=self.core.redirect_policy.max_redirects() {
            if let Some(jar) = &self.core.cookie_jar
                && let Some(authority) = current_uri.authority()
            {
                let is_secure = current_uri.scheme() == Some(&http::uri::Scheme::HTTPS);
                let path = current_uri.path();
                jar.apply_to_request(authority.host(), is_secure, path, &mut current_headers);
            }

            let (req_body, body_for_replay) = match current_body.take() {
                Some(RequestBody::Buffered(b)) => {
                    let body_clone = RequestBody::Buffered(b.clone());
                    (RequestBody::Buffered(b).into_hyper_body(), Some(body_clone))
                }
                Some(rb @ RequestBody::Streaming(_)) => (rb.into_hyper_body(), None),
                None => {
                    let empty: RequestBoxBody = http_body_util::Full::new(Bytes::new())
                        .map_err(|never| match never {})
                        .boxed_unsync();
                    (empty, None)
                }
            };

            if !current_headers.contains_key(HOST)
                && let Some(authority) = current_uri.authority()
                && let Ok(host_value) = authority.as_str().parse()
            {
                current_headers.insert(HOST, host_value);
            }

            let (cache_state, stale_if_error) =
                self.core
                    .cache_lookup(&current_method, &current_uri, &mut current_headers);
            let mut cache_entry = match cache_state {
                CacheLookupOutcome::Fresh(resp) => return Ok(*resp),
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
                    .apply_request(&mut request, &current_uri);
            }

            let replay_bytes_for_stale = match body_for_replay.as_ref() {
                Some(RequestBody::Buffered(b)) => Some(b.clone()),
                _ => None,
            };

            let stale_headers = if !self.core.no_connection_reuse {
                Some(&current_headers)
            } else {
                None
            };

            let resp = match self
                .execute_single(request, &current_uri, replay_bytes_for_stale, stale_headers)
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
                            return Ok(Response::from_boxed(http_resp, current_uri));
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
                        return Ok(Response::from_boxed(http_resp, current_uri));
                    }
                    return Err(e);
                }
            };

            let replay_bytes = match body_for_replay.as_ref() {
                Some(RequestBody::Buffered(b)) => Some(b.clone()),
                _ => None,
            };
            let resp = self
                .maybe_retry_digest(
                    resp,
                    &current_method,
                    &current_uri,
                    &mut current_headers,
                    replay_bytes,
                    version,
                )
                .await?;

            if resp.status() == StatusCode::NOT_MODIFIED
                && let Some(cached) = cache_entry
            {
                let http_resp = cached.into_http_response();
                return Ok(Response::from_boxed(http_resp, current_uri));
            }

            if let Some(ref cache) = self.core.cache {
                cache.invalidate(&current_method, &current_uri);
            }

            if let Some(jar) = &self.core.cookie_jar
                && let Some(authority) = current_uri.authority()
            {
                jar.store_from_response(authority.host(), resp.headers());
            }

            if let Some(ref hsts) = self.core.hsts
                && current_uri.scheme() == Some(&http::uri::Scheme::HTTPS)
                && let Some(authority) = current_uri.authority()
            {
                hsts.store_from_response(authority.host(), resp.headers());
            }

            if !resp.status().is_redirection()
                || resp.status() == StatusCode::NOT_MODIFIED
                || matches!(self.core.redirect_policy, RedirectPolicy::None)
            {
                return self
                    .finalize_response(resp, &current_method, current_uri, &current_headers)
                    .await;
            }

            let redirect = self.core.process_redirect(
                &resp,
                &current_uri,
                current_method.clone(),
                body_for_replay,
                &mut current_headers,
            )?;
            let Some((next_uri, next_method, next_body)) = redirect else {
                return self
                    .finalize_response(resp, &current_method, current_uri, &current_headers)
                    .await;
            };

            let _ = resp.bytes().await;

            current_uri = next_uri;
            current_method = next_method;
            current_body = next_body;
        }

        Err(Error::TooManyRedirects(
            self.core.redirect_policy.max_redirects(),
        ))
    }

    async fn maybe_retry_digest(
        &self,
        resp: Response,
        method: &Method,
        uri: &Uri,
        headers: &mut HeaderMap,
        body_for_replay: Option<Bytes>,
        version: Option<http::Version>,
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

        let _ = resp.bytes().await;
        headers.insert(AUTHORIZATION, auth_value);

        let replay_for_stale = body_for_replay.clone();

        let retry_body: RequestBoxBody = match body_for_replay {
            Some(b) => http_body_util::Full::new(b)
                .map_err(|never| match never {})
                .boxed_unsync(),
            None => http_body_util::Full::new(Bytes::new())
                .map_err(|never| match never {})
                .boxed_unsync(),
        };

        let retry_uri: Uri = match uri.path_and_query() {
            Some(pq) => Uri::from(pq.clone()),
            None => Uri::from_static("/"),
        };
        let mut retry_builder = http::Request::builder()
            .method(method.clone())
            .uri(retry_uri);
        if let Some(ver) = version {
            retry_builder = retry_builder.version(ver);
        }
        let mut retry_request = retry_builder.body(retry_body)?;
        *retry_request.headers_mut() = headers.clone();
        if !self.core.middleware.is_empty() {
            self.core.middleware.apply_request(&mut retry_request, uri);
        }
        self.execute_single(retry_request, uri, replay_for_stale, Some(headers))
            .await
    }

    pub(super) async fn finalize_response(
        &self,
        resp: Response,
        method: &Method,
        uri: Uri,
        request_headers: &HeaderMap,
    ) -> Result<Response, Error> {
        #[cfg(all(feature = "http3", feature = "rustls"))]
        if self.core.h3_endpoint.is_some() {
            self.core.cache_alt_svc(&uri, resp.headers());
        }
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
