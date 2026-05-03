use bytes::Bytes;
use http::header::{
    AUTHORIZATION, CONTENT_ENCODING, CONTENT_LENGTH, CONTENT_TYPE, COOKIE, HOST, HeaderMap,
    HeaderValue, LOCATION, PROXY_AUTHORIZATION, REFERER,
};
use http::{Method, StatusCode, Uri};
use http_body_util::BodyExt;

use super::Client;
use crate::body::RequestBody;
use crate::error::{AioductBody, Error};
use crate::redirect::{RedirectAction, RedirectPolicy};
use crate::response::Response;
use crate::runtime::Runtime;

pub(crate) enum CacheLookupOutcome {
    Fresh(Box<Response>),
    Stale(crate::cache::CachedResponse),
    Miss,
}

impl<R: Runtime> Client<R> {
    pub(crate) async fn execute(
        &self,
        method: Method,
        original_uri: Uri,
        headers: http::HeaderMap,
        body: Option<RequestBody>,
        version: Option<http::Version>,
    ) -> Result<Response, Error> {
        if self.https_only && original_uri.scheme() != Some(&http::uri::Scheme::HTTPS) {
            return Err(Error::HttpsOnly(
                original_uri.scheme_str().unwrap_or("none").to_owned(),
            ));
        }

        let mut current_uri = self.maybe_upgrade_hsts(original_uri);
        let mut current_method = method;
        let mut current_body = body;
        let mut current_headers = headers;

        self.apply_default_headers(&mut current_headers);

        for _ in 0..=self.redirect_policy.max_redirects() {
            if let Some(jar) = &self.cookie_jar
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
                    let empty: AioductBody = http_body_util::Full::new(Bytes::new())
                        .map_err(|never| match never {})
                        .boxed();
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
                self.cache_lookup(&current_method, &current_uri, &mut current_headers);
            let cache_entry = match cache_state {
                CacheLookupOutcome::Fresh(resp) => return Ok(*resp),
                CacheLookupOutcome::Stale(entry) => Some(entry),
                CacheLookupOutcome::Miss => None,
            };

            let path_and_query = current_uri
                .path_and_query()
                .map(|pq| pq.as_str())
                .unwrap_or("/");
            let req_uri: Uri = path_and_query
                .parse()
                .map_err(|e| Error::Other(Box::new(e)))?;

            let mut builder = http::Request::builder()
                .method(current_method.clone())
                .uri(req_uri);

            if let Some(ver) = version {
                builder = builder.version(ver);
            }

            for (name, value) in &current_headers {
                builder = builder.header(name, value);
            }

            let mut request = builder.body(req_body)?;

            if !self.middleware.is_empty() {
                self.middleware.apply_request(&mut request, &current_uri);
            }

            let resp = match self.execute_single(request, &current_uri).await {
                Ok(resp) => {
                    if resp.status().is_server_error()
                        && let Some(sie_duration) = stale_if_error
                        && let Some(ref cached) = cache_entry
                        && cached.age <= sie_duration
                    {
                        let _ = resp.bytes().await;
                        let http_resp = cache_entry.unwrap().into_http_response();
                        return Ok(Response::from_boxed(http_resp, current_uri));
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

            let resp = self
                .maybe_retry_digest(
                    resp,
                    &current_method,
                    &current_uri,
                    &mut current_headers,
                    body_for_replay.as_ref(),
                    version,
                )
                .await?;

            if resp.status() == StatusCode::NOT_MODIFIED
                && let Some(cached) = cache_entry
            {
                let http_resp = cached.into_http_response();
                return Ok(Response::from_boxed(http_resp, current_uri));
            }

            if let Some(ref cache) = self.cache {
                cache.invalidate(&current_method, &current_uri);
            }

            if let Some(jar) = &self.cookie_jar
                && let Some(authority) = current_uri.authority()
            {
                jar.store_from_response(authority.host(), resp.headers());
            }

            if let Some(ref hsts) = self.hsts
                && current_uri.scheme() == Some(&http::uri::Scheme::HTTPS)
                && let Some(authority) = current_uri.authority()
            {
                hsts.store_from_response(authority.host(), resp.headers());
            }

            if !resp.status().is_redirection()
                || resp.status() == StatusCode::NOT_MODIFIED
                || matches!(self.redirect_policy, RedirectPolicy::None)
            {
                return self
                    .finalize_response(resp, &current_method, current_uri)
                    .await;
            }

            let redirect = self.process_redirect(
                &resp,
                &current_uri,
                current_method.clone(),
                body_for_replay,
                &mut current_headers,
            )?;
            let Some((next_uri, next_method, next_body)) = redirect else {
                // Redirect policy said Stop — return the actual response (with body,
                // middleware, decompression) rather than a synthetic empty-body response.
                return self
                    .finalize_response(resp, &current_method, current_uri)
                    .await;
            };

            let _ = resp.bytes().await;

            current_uri = next_uri;
            current_method = next_method;
            current_body = next_body;
        }

        Err(Error::TooManyRedirects(
            self.redirect_policy.max_redirects(),
        ))
    }

    fn maybe_upgrade_hsts(&self, uri: Uri) -> Uri {
        if let Some(ref hsts) = self.hsts
            && uri.scheme() == Some(&http::uri::Scheme::HTTP)
            && let Some(authority) = uri.authority()
            && hsts.should_upgrade(authority.host())
        {
            let upgraded = format!(
                "https://{}{}",
                authority,
                uri.path_and_query().map(|pq| pq.as_str()).unwrap_or("/")
            );
            if let Ok(new_uri) = upgraded.parse() {
                return new_uri;
            }
        }
        uri
    }

    fn apply_default_headers(&self, headers: &mut HeaderMap) {
        for (name, value) in &self.default_headers {
            if !headers.contains_key(name) {
                headers.insert(name, value.clone());
            }
        }
        crate::decompress::set_accept_encoding(headers, &self.accept_encoding);
    }

    fn cache_lookup(
        &self,
        method: &Method,
        uri: &Uri,
        headers: &mut HeaderMap,
    ) -> (CacheLookupOutcome, Option<std::time::Duration>) {
        if let Some(ref cache) = self.cache {
            match cache.lookup(method, uri) {
                crate::cache::CacheLookup::Fresh(cached) => {
                    let http_resp = cached.into_http_response();
                    (
                        CacheLookupOutcome::Fresh(Box::new(Response::from_boxed(
                            http_resp,
                            uri.clone(),
                        ))),
                        None,
                    )
                }
                crate::cache::CacheLookup::Stale {
                    validators,
                    cached,
                    stale_if_error,
                } => {
                    validators.apply_to_request(headers);
                    (CacheLookupOutcome::Stale(cached), stale_if_error)
                }
                crate::cache::CacheLookup::Miss => (CacheLookupOutcome::Miss, None),
            }
        } else {
            (CacheLookupOutcome::Miss, None)
        }
    }

    async fn maybe_retry_digest(
        &self,
        resp: Response,
        method: &Method,
        uri: &Uri,
        headers: &mut HeaderMap,
        body_for_replay: Option<&RequestBody>,
        version: Option<http::Version>,
    ) -> Result<Response, Error> {
        let Some(ref digest) = self.digest_auth else {
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

        let retry_body = match body_for_replay.and_then(RequestBody::try_clone) {
            Some(rb) => rb.into_hyper_body(),
            None => http_body_util::Full::new(Bytes::new())
                .map_err(|never| match never {})
                .boxed(),
        };

        let retry_uri: Uri = uri
            .path_and_query()
            .map(|pq| pq.as_str())
            .unwrap_or("/")
            .parse()
            .map_err(|e| Error::Other(Box::new(e)))?;
        let mut retry_builder = http::Request::builder()
            .method(method.clone())
            .uri(retry_uri);
        if let Some(ver) = version {
            retry_builder = retry_builder.version(ver);
        }
        for (name, value) in headers.iter() {
            retry_builder = retry_builder.header(name, value);
        }
        let mut retry_request = retry_builder.body(retry_body)?;
        if !self.middleware.is_empty() {
            self.middleware.apply_request(&mut retry_request, uri);
        }
        self.execute_single(retry_request, uri).await
    }

    pub(super) async fn finalize_response(
        &self,
        resp: Response,
        method: &Method,
        uri: Uri,
    ) -> Result<Response, Error> {
        #[cfg(all(feature = "http3", feature = "rustls"))]
        if self.h3_endpoint.is_some() {
            self.cache_alt_svc(&uri, resp.headers());
        }
        let mut resp = resp;
        if !self.middleware.is_empty() {
            resp.apply_middleware(&self.middleware, &uri);
        }

        let resp = if !self.accept_encoding.is_empty() {
            resp.decompress(&self.accept_encoding)
        } else {
            resp
        };
        let resp = if let Some(read_timeout) = self.read_timeout {
            resp.apply_read_timeout::<R>(read_timeout)
        } else {
            resp
        };

        if let Some(ref cache) = self.cache {
            let status = resp.status();
            let headers = resp.headers().clone();
            if crate::cache::is_response_cacheable(status, &headers) {
                let body_bytes = resp.bytes().await?;
                cache.store(method, &uri, status, &headers, &body_bytes);
                let cached_resp = super::boxed_response_from_bytes(status, &headers, body_bytes);
                return Ok(Response::from_boxed(cached_resp, uri));
            }
        }

        Ok(resp)
    }

    fn process_redirect(
        &self,
        resp: &Response,
        current_uri: &Uri,
        current_method: Method,
        body_for_replay: Option<RequestBody>,
        headers: &mut HeaderMap,
    ) -> Result<Option<(Uri, Method, Option<RequestBody>)>, Error> {
        let status = resp.status();
        let location = resp
            .headers()
            .get(LOCATION)
            .ok_or_else(|| Error::Redirect("missing Location header".into()))?
            .to_str()
            .map_err(|e| Error::Other(Box::new(e)))?
            .to_owned();

        let next_uri = super::resolve_redirect(current_uri, &location)?;

        if self
            .redirect_policy
            .check(current_uri, &next_uri, status, &current_method)
            == RedirectAction::Stop
        {
            return Ok(None);
        }

        if !self.middleware.is_empty() {
            self.middleware
                .apply_redirect(status, current_uri, &next_uri);
        }

        let (next_method, next_body) = match status {
            StatusCode::MOVED_PERMANENTLY | StatusCode::FOUND | StatusCode::SEE_OTHER => {
                headers.remove(CONTENT_TYPE);
                headers.remove(CONTENT_LENGTH);
                headers.remove(CONTENT_ENCODING);
                (Method::GET, None)
            }
            StatusCode::TEMPORARY_REDIRECT | StatusCode::PERMANENT_REDIRECT => {
                (current_method, body_for_replay)
            }
            _ => return Err(Error::Redirect("unexpected redirect status".into())),
        };

        if let Some(authority) = next_uri.authority()
            && let Ok(host_value) = authority.as_str().parse()
        {
            headers.insert(HOST, host_value);
        }

        let same_origin = current_uri.authority() == next_uri.authority()
            && current_uri.scheme() == next_uri.scheme();
        if !same_origin {
            headers.remove(AUTHORIZATION);
            headers.remove(COOKIE);
            headers.remove(PROXY_AUTHORIZATION);
        }

        if self.referer
            && let Ok(val) = HeaderValue::from_str(&current_uri.to_string())
        {
            headers.insert(REFERER, val);
        }

        Ok(Some((next_uri, next_method, next_body)))
    }
}
