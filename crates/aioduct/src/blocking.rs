use std::marker::PhantomData;
use std::time::Duration;

use bytes::Bytes;
use http::{HeaderMap, Method, StatusCode};

use crate::error::Error;
use crate::runtime::RuntimeCompletion;
use crate::traits::{HttpClient, RequestBuilderExt, ResponseExt};

/// A blocking HTTP client that wraps any async [`HttpClient`] implementor.
///
/// Uses [`RuntimeCompletion::block_on`] to execute async operations synchronously.
///
/// # Type aliases
///
/// For convenience, use the pre-configured type aliases:
/// - [`BlockingTokioClient`](crate::BlockingTokioClient)
/// - [`BlockingSmolClient`](crate::BlockingSmolClient)
/// - [`BlockingCompioClient`](crate::BlockingCompioClient)
pub struct BlockingClient<C: HttpClient, R: RuntimeCompletion> {
    inner: C,
    _runtime: PhantomData<R>,
}

impl<C: HttpClient, R: RuntimeCompletion> Clone for BlockingClient<C, R> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            _runtime: PhantomData,
        }
    }
}

impl<C: HttpClient, R: RuntimeCompletion> BlockingClient<C, R> {
    /// Create a blocking client wrapping the given async client.
    pub fn new(inner: C) -> Self {
        Self {
            inner,
            _runtime: PhantomData,
        }
    }

    /// Get a reference to the inner async client.
    pub fn inner(&self) -> &C {
        &self.inner
    }

    /// Start a GET request.
    pub fn get(&self, uri: &str) -> Result<BlockingRequestBuilder<C, R>, Error> {
        Ok(BlockingRequestBuilder {
            inner: self.inner.get(uri)?,
            _runtime: PhantomData,
        })
    }

    /// Start a HEAD request.
    pub fn head(&self, uri: &str) -> Result<BlockingRequestBuilder<C, R>, Error> {
        Ok(BlockingRequestBuilder {
            inner: self.inner.head(uri)?,
            _runtime: PhantomData,
        })
    }

    /// Start a POST request.
    pub fn post(&self, uri: &str) -> Result<BlockingRequestBuilder<C, R>, Error> {
        Ok(BlockingRequestBuilder {
            inner: self.inner.post(uri)?,
            _runtime: PhantomData,
        })
    }

    /// Start a PUT request.
    pub fn put(&self, uri: &str) -> Result<BlockingRequestBuilder<C, R>, Error> {
        Ok(BlockingRequestBuilder {
            inner: self.inner.put(uri)?,
            _runtime: PhantomData,
        })
    }

    /// Start a PATCH request.
    pub fn patch(&self, uri: &str) -> Result<BlockingRequestBuilder<C, R>, Error> {
        Ok(BlockingRequestBuilder {
            inner: self.inner.patch(uri)?,
            _runtime: PhantomData,
        })
    }

    /// Start a DELETE request.
    pub fn delete(&self, uri: &str) -> Result<BlockingRequestBuilder<C, R>, Error> {
        Ok(BlockingRequestBuilder {
            inner: self.inner.delete(uri)?,
            _runtime: PhantomData,
        })
    }

    /// Start a request with a custom method.
    pub fn request(
        &self,
        method: Method,
        uri: &str,
    ) -> Result<BlockingRequestBuilder<C, R>, Error> {
        Ok(BlockingRequestBuilder {
            inner: self.inner.request(method, uri)?,
            _runtime: PhantomData,
        })
    }
}

/// A blocking request builder.
pub struct BlockingRequestBuilder<C: HttpClient, R: RuntimeCompletion> {
    inner: C::RequestBuilder,
    _runtime: PhantomData<R>,
}

impl<C: HttpClient, R: RuntimeCompletion> BlockingRequestBuilder<C, R> {
    /// Add a typed header to the request.
    pub fn header(
        mut self,
        name: http::header::HeaderName,
        value: http::header::HeaderValue,
    ) -> Self {
        self.inner = self.inner.header(name, value);
        self
    }

    /// Add multiple headers to the request.
    pub fn headers(mut self, headers: HeaderMap) -> Self {
        self.inner = self.inner.headers(headers);
        self
    }

    /// Set a Bearer token Authorization header.
    pub fn bearer_auth(mut self, token: &str) -> Self {
        self.inner = self.inner.bearer_auth(token);
        self
    }

    /// Set a buffered request body.
    pub fn body(mut self, body: impl Into<Bytes>) -> Self {
        self.inner = self.inner.body(body);
        self
    }

    /// Set a timeout for this request.
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.inner = self.inner.timeout(timeout);
        self
    }

    /// Set a timeout for establishing this request's connection.
    pub fn connect_timeout(mut self, timeout: Duration) -> Self {
        self.inner = self.inner.connect_timeout(timeout);
        self
    }

    /// Set a timeout for gaps between response body data chunks.
    pub fn read_timeout(mut self, timeout: Duration) -> Self {
        self.inner = self.inner.read_timeout(timeout);
        self
    }

    /// Set HTTP Basic auth credentials.
    pub fn basic_auth(mut self, username: &str, password: Option<&str>) -> Self {
        self.inner = self.inner.basic_auth(username, password);
        self
    }

    /// Append query parameters to the URL.
    pub fn query(mut self, params: &[(&str, &str)]) -> Self {
        self.inner = self.inner.query(params);
        self
    }

    /// Force a specific HTTP version.
    pub fn version(mut self, version: http::Version) -> Self {
        self.inner = self.inner.version(version);
        self
    }

    /// Send the request and block until the response is received.
    pub fn send(
        self,
    ) -> Result<BlockingResponse<<C::RequestBuilder as RequestBuilderExt>::Response, R>, Error>
    {
        let resp = R::block_on(self.inner.send())?.map_err(|e| e.into_error())?;
        Ok(BlockingResponse {
            inner: resp,
            _runtime: PhantomData,
        })
    }
}

/// A blocking HTTP response.
pub struct BlockingResponse<Resp: ResponseExt, R: RuntimeCompletion> {
    inner: Resp,
    _runtime: PhantomData<R>,
}

impl<Resp: ResponseExt, R: RuntimeCompletion> BlockingResponse<Resp, R> {
    /// Returns the HTTP status code.
    pub fn status(&self) -> StatusCode {
        self.inner.status()
    }

    /// Returns the response headers.
    pub fn headers(&self) -> &HeaderMap {
        self.inner.headers()
    }

    /// Returns the Content-Length header value, if present.
    pub fn content_length(&self) -> Option<u64> {
        self.inner
            .headers()
            .get(http::header::CONTENT_LENGTH)?
            .to_str()
            .ok()?
            .parse()
            .ok()
    }

    /// Returns an error if the status is 4xx or 5xx, consuming the response.
    pub fn error_for_status(self) -> Result<Self, Error> {
        let status = self.inner.status();
        if status.is_client_error() || status.is_server_error() {
            Err(Error::Status(status))
        } else {
            Ok(self)
        }
    }

    /// Returns an error if the status is 4xx or 5xx, without consuming the response.
    pub fn error_for_status_ref(&self) -> Result<&Self, Error> {
        let status = self.inner.status();
        if status.is_client_error() || status.is_server_error() {
            Err(Error::Status(status))
        } else {
            Ok(self)
        }
    }

    /// Consume the response body and return it as bytes.
    pub fn bytes(self) -> Result<Bytes, Error> {
        R::block_on(self.inner.bytes())?
    }

    /// Consume the response body and return it as a UTF-8 string.
    pub fn text(self) -> Result<String, Error> {
        R::block_on(self.inner.text())?
    }
}

impl<Resp: ResponseExt, R: RuntimeCompletion> std::fmt::Debug for BlockingResponse<Resp, R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BlockingResponse")
            .field("status", &self.inner.status())
            .finish_non_exhaustive()
    }
}

/// Additional methods available when the inner response is [`Response`](crate::Response).
#[cfg(not(target_arch = "wasm32"))]
impl<R: RuntimeCompletion> BlockingResponse<crate::Response, R> {
    /// Returns the final URL of this response, after any redirects.
    pub fn url(&self) -> &http::Uri {
        self.inner.url()
    }

    /// Returns the remote socket address of the server.
    pub fn remote_addr(&self) -> Option<std::net::SocketAddr> {
        self.inner.remote_addr()
    }

    /// Returns the HTTP version.
    pub fn version(&self) -> http::Version {
        self.inner.version()
    }

    /// Returns TLS handshake info, if the connection used TLS.
    pub fn tls_info(&self) -> Option<&crate::tls::TlsInfo> {
        self.inner.tls_info()
    }

    /// Returns a mutable reference to the response headers.
    pub fn headers_mut(&mut self) -> &mut HeaderMap {
        self.inner.headers_mut()
    }

    /// Returns a reference to the response extensions.
    pub fn extensions(&self) -> &http::Extensions {
        self.inner.extensions()
    }

    /// Returns a mutable reference to the response extensions.
    pub fn extensions_mut(&mut self) -> &mut http::Extensions {
        self.inner.extensions_mut()
    }

    /// Consume the response body and deserialize it as JSON.
    #[cfg(feature = "json")]
    pub fn json<T: serde::de::DeserializeOwned>(self) -> Result<T, Error> {
        R::block_on(self.inner.json())?
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "tokio")]
    #[test]
    fn blocking_tokio_default_headers() {
        use crate::runtime::tokio_rt::TcpConnector;
        let engine = crate::HttpEngineSend::<crate::runtime::TokioRuntime, TcpConnector>::builder()
            .user_agent("blocking-test/1.0")
            .build()
            .unwrap();
        let client = BlockingClient::<_, crate::runtime::TokioRuntime>::new(engine);
        let result = client.get("http://127.0.0.1:1/nonexistent");
        assert!(result.is_ok());
    }

    #[cfg(feature = "tokio")]
    #[test]
    fn blocking_tokio_wraps_http2_keep_alive_config() {
        use crate::runtime::tokio_rt::TcpConnector;

        let engine = crate::HttpEngineSend::<crate::runtime::TokioRuntime, TcpConnector>::builder()
            .http2_keep_alive_interval(Duration::from_secs(30))
            .http2_keep_alive_timeout(Duration::from_secs(10))
            .http2_keep_alive_while_idle(true)
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap();

        let client = BlockingClient::<_, crate::runtime::TokioRuntime>::new(engine);
        let config = client.inner.core.http2.as_ref().expect("http2 config");
        assert_eq!(client.inner.core.timeout, Some(Duration::from_secs(5)));
        assert_eq!(config.keep_alive_interval, Some(Duration::from_secs(30)));
        assert_eq!(config.keep_alive_timeout, Some(Duration::from_secs(10)));
        assert_eq!(config.keep_alive_while_idle, Some(true));
    }

    #[cfg(feature = "smol")]
    #[test]
    fn blocking_smol_wraps_http2_keep_alive_config() {
        use crate::runtime::smol_rt::TcpConnector;

        let engine = crate::HttpEngineSend::<crate::runtime::SmolRuntime, TcpConnector>::builder()
            .http2_keep_alive_interval(Duration::from_secs(30))
            .http2_keep_alive_timeout(Duration::from_secs(10))
            .http2_keep_alive_while_idle(true)
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap();

        let client = BlockingClient::<_, crate::runtime::SmolRuntime>::new(engine);
        let config = client.inner.core.http2.as_ref().expect("http2 config");
        assert_eq!(client.inner.core.timeout, Some(Duration::from_secs(5)));
        assert_eq!(config.keep_alive_interval, Some(Duration::from_secs(30)));
        assert_eq!(config.keep_alive_timeout, Some(Duration::from_secs(10)));
        assert_eq!(config.keep_alive_while_idle, Some(true));
    }

    #[cfg(feature = "compio")]
    #[test]
    fn blocking_compio_wraps_http2_keep_alive_config() {
        use crate::runtime::compio_rt::TcpConnector;

        let engine =
            crate::HttpEngineLocal::<crate::runtime::CompioRuntime, TcpConnector>::builder()
                .http2_keep_alive_interval(Duration::from_secs(30))
                .http2_keep_alive_timeout(Duration::from_secs(10))
                .http2_keep_alive_while_idle(true)
                .timeout(Duration::from_secs(5))
                .build_local()
                .unwrap();

        let client = BlockingClient::<_, crate::runtime::CompioRuntime>::new(engine);
        let config = client.inner.core.http2.as_ref().expect("http2 config");
        assert_eq!(client.inner.core.timeout, Some(Duration::from_secs(5)));
        assert_eq!(config.keep_alive_interval, Some(Duration::from_secs(30)));
        assert_eq!(config.keep_alive_timeout, Some(Duration::from_secs(10)));
        assert_eq!(config.keep_alive_while_idle, Some(true));
    }

    #[cfg(feature = "tokio")]
    #[test]
    fn blocking_client_all_methods() {
        use crate::runtime::tokio_rt::TcpConnector;
        let engine = crate::HttpEngineSend::<crate::runtime::TokioRuntime, TcpConnector>::new();
        let client = BlockingClient::<_, crate::runtime::TokioRuntime>::new(engine);
        assert!(client.get("http://127.0.0.1:1/").is_ok());
        assert!(client.head("http://127.0.0.1:1/").is_ok());
        assert!(client.post("http://127.0.0.1:1/").is_ok());
        assert!(client.put("http://127.0.0.1:1/").is_ok());
        assert!(client.patch("http://127.0.0.1:1/").is_ok());
        assert!(client.delete("http://127.0.0.1:1/").is_ok());
        assert!(
            client
                .request(Method::OPTIONS, "http://127.0.0.1:1/")
                .is_ok()
        );
    }

    #[cfg(feature = "tokio")]
    #[test]
    fn blocking_client_invalid_url() {
        use crate::runtime::tokio_rt::TcpConnector;
        let engine = crate::HttpEngineSend::<crate::runtime::TokioRuntime, TcpConnector>::new();
        let client = BlockingClient::<_, crate::runtime::TokioRuntime>::new(engine);
        assert!(client.get("not a valid url\n").is_err());
    }

    #[cfg(feature = "tokio")]
    #[test]
    fn blocking_client_invalid_url_all_methods() {
        use crate::runtime::tokio_rt::TcpConnector;
        let engine = crate::HttpEngineSend::<crate::runtime::TokioRuntime, TcpConnector>::new();
        let client = BlockingClient::<_, crate::runtime::TokioRuntime>::new(engine);
        let bad_url = "not a valid url\n";
        assert!(client.head(bad_url).is_err());
        assert!(client.post(bad_url).is_err());
        assert!(client.put(bad_url).is_err());
        assert!(client.patch(bad_url).is_err());
        assert!(client.delete(bad_url).is_err());
        assert!(client.request(Method::OPTIONS, bad_url).is_err());
    }

    #[cfg(feature = "tokio")]
    #[test]
    fn blocking_response_error_for_status_ok() {
        use crate::runtime::tokio_rt::TcpConnector;

        let addr = aioduct_test_server::h1::spawn_h1_server();
        let engine = crate::HttpEngineSend::<crate::runtime::TokioRuntime, TcpConnector>::new();
        let client = BlockingClient::<_, crate::runtime::TokioRuntime>::new(engine);
        let resp = client
            .get(&format!("http://{}/", addr))
            .unwrap()
            .send()
            .unwrap();
        assert!(resp.error_for_status().is_ok());
    }

    #[cfg(feature = "tokio")]
    #[test]
    fn blocking_response_error_for_status_ref_ok() {
        use crate::runtime::tokio_rt::TcpConnector;

        let addr = aioduct_test_server::h1::spawn_h1_server();
        let engine = crate::HttpEngineSend::<crate::runtime::TokioRuntime, TcpConnector>::new();
        let client = BlockingClient::<_, crate::runtime::TokioRuntime>::new(engine);
        let resp = client
            .get(&format!("http://{}/", addr))
            .unwrap()
            .send()
            .unwrap();
        assert!(resp.error_for_status_ref().is_ok());
    }

    #[cfg(feature = "tokio")]
    #[test]
    fn blocking_response_debug() {
        use crate::runtime::tokio_rt::TcpConnector;

        let addr = aioduct_test_server::h1::spawn_h1_server();
        let engine = crate::HttpEngineSend::<crate::runtime::TokioRuntime, TcpConnector>::new();
        let client = BlockingClient::<_, crate::runtime::TokioRuntime>::new(engine);
        let resp = client
            .get(&format!("http://{}/", addr))
            .unwrap()
            .send()
            .unwrap();
        let dbg = format!("{:?}", resp);
        assert!(dbg.contains("BlockingResponse"));
    }

    #[cfg(feature = "tokio")]
    #[test]
    fn blocking_response_bytes_and_text() {
        use crate::runtime::tokio_rt::TcpConnector;

        let addr = aioduct_test_server::h1::spawn_h1_server();
        let engine = crate::HttpEngineSend::<crate::runtime::TokioRuntime, TcpConnector>::new();
        let client = BlockingClient::<_, crate::runtime::TokioRuntime>::new(engine);
        let resp = client
            .get(&format!("http://{}/", addr))
            .unwrap()
            .send()
            .unwrap();
        let body = resp.bytes().unwrap();
        assert!(!body.is_empty());

        let resp2 = client
            .get(&format!("http://{}/", addr))
            .unwrap()
            .send()
            .unwrap();
        let text = resp2.text().unwrap();
        assert!(!text.is_empty());
    }

    #[cfg(feature = "tokio")]
    #[test]
    fn blocking_response_url_and_version() {
        use crate::runtime::tokio_rt::TcpConnector;

        let addr = aioduct_test_server::h1::spawn_h1_server();
        let engine = crate::HttpEngineSend::<crate::runtime::TokioRuntime, TcpConnector>::new();
        let client = BlockingClient::<_, crate::runtime::TokioRuntime>::new(engine);
        let resp = client
            .get(&format!("http://{}/", addr))
            .unwrap()
            .send()
            .unwrap();
        assert!(resp.url().to_string().contains(&addr.to_string()));
        assert_eq!(resp.version(), http::Version::HTTP_11);
        assert!(resp.remote_addr().is_some());
        assert!(resp.tls_info().is_none());
    }

    #[cfg(feature = "tokio")]
    #[test]
    fn blocking_response_native_metadata_mutators() {
        use crate::runtime::tokio_rt::TcpConnector;

        #[derive(Clone, Debug, PartialEq)]
        struct Marker(&'static str);

        let addr = aioduct_test_server::h1::spawn_h1_server();
        let engine = crate::HttpEngineSend::<crate::runtime::TokioRuntime, TcpConnector>::new();
        let client = BlockingClient::<_, crate::runtime::TokioRuntime>::new(engine);
        let mut resp = client
            .get(&format!("http://{addr}/"))
            .unwrap()
            .send()
            .unwrap();

        resp.headers_mut()
            .insert("x-blocking-mutated", http::HeaderValue::from_static("yes"));
        assert_eq!(resp.headers().get("x-blocking-mutated").unwrap(), "yes");

        assert!(resp.extensions().get::<Marker>().is_none());
        resp.extensions_mut().insert(Marker("native"));
        assert_eq!(
            resp.extensions().get::<Marker>().map(|marker| marker.0),
            Some("native")
        );
    }

    #[cfg(feature = "tokio")]
    #[test]
    fn blocking_head_sends_and_returns_status() {
        use crate::runtime::tokio_rt::TcpConnector;

        let addr = aioduct_test_server::h1::spawn_h1_server_with(aioduct_test_server::h1::echo);
        let engine = crate::HttpEngineSend::<crate::runtime::TokioRuntime, TcpConnector>::new();
        let client = BlockingClient::<_, crate::runtime::TokioRuntime>::new(engine);
        let resp = client
            .head(&format!("http://{}/test", addr))
            .unwrap()
            .send()
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[cfg(feature = "tokio")]
    #[test]
    fn blocking_post_sends_body_and_returns_echo() {
        use crate::runtime::tokio_rt::TcpConnector;

        let addr = aioduct_test_server::h1::spawn_h1_server_with(aioduct_test_server::h1::echo);
        let engine = crate::HttpEngineSend::<crate::runtime::TokioRuntime, TcpConnector>::new();
        let client = BlockingClient::<_, crate::runtime::TokioRuntime>::new(engine);
        let resp = client
            .post(&format!("http://{}/submit", addr))
            .unwrap()
            .body("payload")
            .send()
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let text = resp.text().unwrap();
        assert!(
            text.contains("method=POST"),
            "expected POST method in echo: {text}"
        );
        assert!(
            text.contains("body=payload"),
            "expected body in echo: {text}"
        );
    }

    #[cfg(feature = "tokio")]
    #[test]
    fn blocking_put_sends_and_echoes_method() {
        use crate::runtime::tokio_rt::TcpConnector;

        let addr = aioduct_test_server::h1::spawn_h1_server_with(aioduct_test_server::h1::echo);
        let engine = crate::HttpEngineSend::<crate::runtime::TokioRuntime, TcpConnector>::new();
        let client = BlockingClient::<_, crate::runtime::TokioRuntime>::new(engine);
        let resp = client
            .put(&format!("http://{}/resource", addr))
            .unwrap()
            .body("update")
            .send()
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let text = resp.text().unwrap();
        assert!(
            text.contains("method=PUT"),
            "expected PUT method in echo: {text}"
        );
        assert!(
            text.contains("body=update"),
            "expected body in echo: {text}"
        );
    }

    #[cfg(feature = "tokio")]
    #[test]
    fn blocking_patch_sends_and_echoes_method() {
        use crate::runtime::tokio_rt::TcpConnector;

        let addr = aioduct_test_server::h1::spawn_h1_server_with(aioduct_test_server::h1::echo);
        let engine = crate::HttpEngineSend::<crate::runtime::TokioRuntime, TcpConnector>::new();
        let client = BlockingClient::<_, crate::runtime::TokioRuntime>::new(engine);
        let resp = client
            .patch(&format!("http://{}/resource", addr))
            .unwrap()
            .body("partial")
            .send()
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let text = resp.text().unwrap();
        assert!(
            text.contains("method=PATCH"),
            "expected PATCH method in echo: {text}"
        );
        assert!(
            text.contains("body=partial"),
            "expected body in echo: {text}"
        );
    }

    #[cfg(feature = "tokio")]
    #[test]
    fn blocking_delete_sends_and_echoes_method() {
        use crate::runtime::tokio_rt::TcpConnector;

        let addr = aioduct_test_server::h1::spawn_h1_server_with(aioduct_test_server::h1::echo);
        let engine = crate::HttpEngineSend::<crate::runtime::TokioRuntime, TcpConnector>::new();
        let client = BlockingClient::<_, crate::runtime::TokioRuntime>::new(engine);
        let resp = client
            .delete(&format!("http://{}/resource/42", addr))
            .unwrap()
            .send()
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let text = resp.text().unwrap();
        assert!(
            text.contains("method=DELETE"),
            "expected DELETE method in echo: {text}"
        );
        assert!(
            text.contains("path=/resource/42"),
            "expected path in echo: {text}"
        );
    }

    #[cfg(feature = "tokio")]
    #[test]
    fn blocking_custom_method_sends_and_echoes() {
        use crate::runtime::tokio_rt::TcpConnector;

        let addr = aioduct_test_server::h1::spawn_h1_server_with(aioduct_test_server::h1::echo);
        let engine = crate::HttpEngineSend::<crate::runtime::TokioRuntime, TcpConnector>::new();
        let client = BlockingClient::<_, crate::runtime::TokioRuntime>::new(engine);
        let resp = client
            .request(Method::OPTIONS, &format!("http://{}/options", addr))
            .unwrap()
            .send()
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let text = resp.text().unwrap();
        assert!(
            text.contains("method=OPTIONS"),
            "expected OPTIONS method in echo: {text}"
        );
    }

    #[cfg(feature = "tokio")]
    #[test]
    fn blocking_response_error_for_status_4xx() {
        use crate::runtime::tokio_rt::TcpConnector;

        let addr = aioduct_test_server::h1::spawn_h1_server_with(|_req| async {
            Ok(hyper::Response::builder()
                .status(404)
                .body(http_body_util::Full::new(bytes::Bytes::from("not found")))
                .unwrap())
        });
        let engine = crate::HttpEngineSend::<crate::runtime::TokioRuntime, TcpConnector>::new();
        let client = BlockingClient::<_, crate::runtime::TokioRuntime>::new(engine);
        let resp = client
            .get(&format!("http://{}/missing", addr))
            .unwrap()
            .send()
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        let err = resp.error_for_status().unwrap_err();
        match err {
            crate::error::Error::Status(s) => assert_eq!(s, StatusCode::NOT_FOUND),
            other => panic!("expected Error::Status, got: {other:?}"),
        }
    }

    #[cfg(feature = "tokio")]
    #[test]
    fn blocking_response_error_for_status_5xx() {
        use crate::runtime::tokio_rt::TcpConnector;

        let addr = aioduct_test_server::h1::spawn_h1_server_with(|_req| async {
            Ok(hyper::Response::builder()
                .status(500)
                .body(http_body_util::Full::new(bytes::Bytes::from(
                    "internal error",
                )))
                .unwrap())
        });
        let engine = crate::HttpEngineSend::<crate::runtime::TokioRuntime, TcpConnector>::new();
        let client = BlockingClient::<_, crate::runtime::TokioRuntime>::new(engine);
        let resp = client
            .get(&format!("http://{}/fail", addr))
            .unwrap()
            .send()
            .unwrap();
        assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let err = resp.error_for_status().unwrap_err();
        match err {
            crate::error::Error::Status(s) => assert_eq!(s, StatusCode::INTERNAL_SERVER_ERROR),
            other => panic!("expected Error::Status, got: {other:?}"),
        }
    }

    #[cfg(feature = "tokio")]
    #[test]
    fn blocking_response_error_for_status_ref_4xx() {
        use crate::runtime::tokio_rt::TcpConnector;

        let addr = aioduct_test_server::h1::spawn_h1_server_with(|_req| async {
            Ok(hyper::Response::builder()
                .status(403)
                .body(http_body_util::Full::new(bytes::Bytes::from("forbidden")))
                .unwrap())
        });
        let engine = crate::HttpEngineSend::<crate::runtime::TokioRuntime, TcpConnector>::new();
        let client = BlockingClient::<_, crate::runtime::TokioRuntime>::new(engine);
        let resp = client
            .get(&format!("http://{}/secret", addr))
            .unwrap()
            .send()
            .unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
        let err = resp.error_for_status_ref().unwrap_err();
        match err {
            crate::error::Error::Status(s) => assert_eq!(s, StatusCode::FORBIDDEN),
            other => panic!("expected Error::Status, got: {other:?}"),
        }
    }

    #[cfg(feature = "tokio")]
    #[test]
    fn blocking_response_content_length_present() {
        use crate::runtime::tokio_rt::TcpConnector;

        let addr = aioduct_test_server::h1::spawn_h1_server_with(|_req| async {
            Ok(hyper::Response::builder()
                .header("content-length", "7")
                .body(http_body_util::Full::new(bytes::Bytes::from("1234567")))
                .unwrap())
        });
        let engine = crate::HttpEngineSend::<crate::runtime::TokioRuntime, TcpConnector>::new();
        let client = BlockingClient::<_, crate::runtime::TokioRuntime>::new(engine);
        let resp = client
            .get(&format!("http://{}/", addr))
            .unwrap()
            .send()
            .unwrap();
        assert_eq!(resp.content_length(), Some(7));
    }

    #[cfg(all(feature = "tokio", feature = "json"))]
    #[test]
    fn blocking_json_deserialization() {
        use crate::runtime::tokio_rt::TcpConnector;

        let addr = aioduct_test_server::h1::spawn_h1_server_with(|_req| async {
            let body = r#"{"name":"aioduct","version":1}"#;
            Ok(hyper::Response::builder()
                .header("content-type", "application/json")
                .body(http_body_util::Full::new(bytes::Bytes::from(body)))
                .unwrap())
        });
        let engine = crate::HttpEngineSend::<crate::runtime::TokioRuntime, TcpConnector>::new();
        let client = BlockingClient::<_, crate::runtime::TokioRuntime>::new(engine);
        let resp = client
            .get(&format!("http://{}/", addr))
            .unwrap()
            .send()
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        #[derive(serde::Deserialize, Debug)]
        struct Info {
            name: String,
            version: u32,
        }
        let info: Info = resp.json().unwrap();
        assert_eq!(info.name, "aioduct");
        assert_eq!(info.version, 1);
    }
}
