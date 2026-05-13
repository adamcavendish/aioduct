use http::header::{
    CONTENT_ENCODING, CONTENT_LENGTH, CONTENT_TYPE, COOKIE, HOST, HeaderMap, HeaderValue, LOCATION,
    PROXY_AUTHORIZATION, REFERER,
};
use http::{Method, StatusCode, Uri};

use super::HttpEngine;
use crate::body::RequestBody;
use crate::error::Error;
use crate::redirect::RedirectAction;
use crate::response::Response;

pub(crate) enum CacheLookupOutcome {
    Fresh(Box<Response>),
    Stale(crate::cache::CachedResponse),
    Miss,
}

// ── Shared helpers (no runtime/connector bounds) ─────────────────────────────

impl<R, C> HttpEngine<R, C> {
    pub(super) fn maybe_upgrade_hsts(&self, uri: Uri) -> Uri {
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

    pub(super) fn apply_default_headers(&self, headers: &mut HeaderMap) {
        for (name, value) in &self.default_headers {
            if !headers.contains_key(name) {
                headers.insert(name, value.clone());
            }
        }
        crate::decompress::set_accept_encoding(headers, &self.accept_encoding);
    }

    pub(super) fn cache_lookup(
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

    pub(super) fn process_redirect(
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
            headers.remove(http::header::AUTHORIZATION);
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
