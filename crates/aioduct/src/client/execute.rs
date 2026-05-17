use http::header::{
    CONTENT_ENCODING, CONTENT_LENGTH, CONTENT_TYPE, COOKIE, HOST, HeaderMap, HeaderValue, LOCATION,
    PROXY_AUTHORIZATION, REFERER,
};
use http::{Method, StatusCode, Uri};

use super::HttpEngineCore;
use crate::body::RequestBody;
use crate::error::Error;
use crate::redirect::RedirectAction;
use crate::response::Response;

pub(crate) enum CacheLookupOutcome {
    Fresh(Box<Response>),
    Stale(crate::cache::CachedResponse),
    Miss,
}

pub(super) enum PostExecuteAction {
    Done,
    Redirect {
        uri: Uri,
        method: Method,
        body: Option<RequestBody>,
    },
}

impl std::fmt::Debug for PostExecuteAction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Done => write!(f, "Done"),
            Self::Redirect { uri, method, .. } => f
                .debug_struct("Redirect")
                .field("uri", uri)
                .field("method", method)
                .finish(),
        }
    }
}

// ── Shared helpers (no runtime/connector bounds) ─────────────────────────────

impl<B> HttpEngineCore<B> {
    pub(super) fn maybe_upgrade_hsts(&self, uri: Uri) -> Uri {
        if let Some(ref hsts) = self.hsts
            && uri.scheme() == Some(&http::uri::Scheme::HTTP)
            && let Some(authority) = uri.authority()
            && hsts.should_upgrade(authority.host())
        {
            let host = authority.host();
            let port = authority.port_u16();
            let authority_str = match port {
                Some(80) | None => host.to_owned(),
                Some(p) => format!("{host}:{p}"),
            };
            let upgraded = format!(
                "https://{}{}",
                authority_str,
                uri.path_and_query().map(|pq| pq.as_str()).unwrap_or("/")
            );
            if let Ok(new_uri) = upgraded.parse() {
                return new_uri;
            }
        }
        uri
    }

    pub(super) fn apply_default_headers(&self, headers: &mut HeaderMap) {
        for (name, value) in self.default_headers.iter() {
            if !headers.contains_key(name) {
                headers.insert(name, value.clone());
            }
        }
        if let Some(ref val) = self.accept_encoding_header
            && !headers.contains_key(http::header::ACCEPT_ENCODING)
        {
            headers.insert(http::header::ACCEPT_ENCODING, val.clone());
        }
    }

    pub(super) fn cache_lookup(
        &self,
        method: &Method,
        uri: &Uri,
        headers: &mut HeaderMap,
    ) -> (CacheLookupOutcome, Option<std::time::Duration>) {
        if let Some(ref cache) = self.cache {
            match cache.lookup(method, uri, headers) {
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

    pub(super) fn prepare_request_headers(&self, uri: &Uri, headers: &mut HeaderMap) {
        if let Some(jar) = &self.cookie_jar
            && let Some(authority) = uri.authority()
        {
            let is_secure = uri.scheme() == Some(&http::uri::Scheme::HTTPS);
            let path = uri.path();
            jar.apply_to_request(authority.host(), is_secure, path, headers);
        }

        if !headers.contains_key(HOST)
            && let Some(authority) = uri.authority()
            && let Ok(host_value) = authority.as_str().parse()
        {
            headers.insert(HOST, host_value);
        }
    }

    pub(super) fn post_execute(
        &self,
        resp: &Response,
        current_method: &Method,
        current_uri: &Uri,
        headers: &mut HeaderMap,
        body_for_replay: Option<RequestBody>,
    ) -> Result<PostExecuteAction, Error> {
        if let Some(ref cache) = self.cache {
            cache.invalidate(current_method, current_uri);
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
            || matches!(self.redirect_policy, crate::redirect::RedirectPolicy::None)
        {
            return Ok(PostExecuteAction::Done);
        }

        let redirect = self.process_redirect(
            resp,
            current_uri,
            current_method.clone(),
            body_for_replay,
            headers,
        )?;
        let Some((next_uri, next_method, next_body)) = redirect else {
            return Ok(PostExecuteAction::Done);
        };

        let next_uri = self.maybe_upgrade_hsts(next_uri);

        if self.https_only && next_uri.scheme() != Some(&http::uri::Scheme::HTTPS) {
            return Err(Error::HttpsOnly(
                next_uri.scheme_str().unwrap_or("none").to_owned(),
            ));
        }

        Ok(PostExecuteAction::Redirect {
            uri: next_uri,
            method: next_method,
            body: next_body,
        })
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
                if current_method == Method::GET || current_method == Method::HEAD {
                    (current_method, None)
                } else {
                    headers.remove(CONTENT_TYPE);
                    headers.remove(CONTENT_LENGTH);
                    headers.remove(CONTENT_ENCODING);
                    (Method::GET, None)
                }
            }
            StatusCode::TEMPORARY_REDIRECT | StatusCode::PERMANENT_REDIRECT => {
                match body_for_replay {
                    Some(body) => (current_method, Some(body)),
                    None if current_method == Method::GET || current_method == Method::HEAD => {
                        (current_method, None)
                    }
                    None => {
                        return Err(Error::Redirect(
                            "cannot replay streaming body for 307/308 redirect".into(),
                        ));
                    }
                }
            }
            _ => return Err(Error::Redirect("unexpected redirect status".into())),
        };

        if let Some(authority) = next_uri.authority()
            && let Ok(host_value) = authority.as_str().parse()
        {
            headers.insert(HOST, host_value);
        }

        let same_origin = same_origin(current_uri, &next_uri);
        if !same_origin {
            headers.remove(http::header::AUTHORIZATION);
            headers.remove(COOKIE);
            headers.remove(PROXY_AUTHORIZATION);
            for name in &self.sensitive_headers {
                headers.remove(name);
            }
        }

        if self.referer
            && !(current_uri.scheme() == Some(&http::uri::Scheme::HTTPS)
                && next_uri.scheme() != Some(&http::uri::Scheme::HTTPS))
            && let Ok(val) = HeaderValue::from_str(&current_uri.to_string())
        {
            headers.insert(REFERER, val);
        }

        Ok(Some((next_uri, next_method, next_body)))
    }
}

fn effective_port(uri: &Uri) -> u16 {
    uri.port_u16().unwrap_or_else(|| {
        if uri.scheme() == Some(&http::uri::Scheme::HTTPS) {
            443
        } else {
            80
        }
    })
}

fn same_origin(a: &Uri, b: &Uri) -> bool {
    a.scheme() == b.scheme()
        && a.host()
            .map(|h| h.eq_ignore_ascii_case(b.host().unwrap_or("")))
            == Some(true)
        && effective_port(a) == effective_port(b)
}
