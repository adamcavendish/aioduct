use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use http::{HeaderMap, Method, StatusCode, Uri, Version};

use crate::error::Error;
use crate::runtime::tokio_rt::TokioRuntime;

/// A blocking HTTP client that wraps the async [`Client`](crate::Client).
///
/// Internally creates a tokio runtime to execute requests synchronously.
/// Requires the `blocking` feature (which enables `tokio`).
#[derive(Clone)]
pub struct Client {
    inner: crate::Client<TokioRuntime>,
    rt: Arc<tokio::runtime::Runtime>,
}

impl Client {
    /// Create a blocking client with default settings.
    pub fn new() -> Self {
        Self::builder().build()
    }

    /// Create a blocking client builder.
    pub fn builder() -> ClientBuilder {
        ClientBuilder {
            inner: crate::Client::<TokioRuntime>::builder(),
        }
    }

    fn request_builder<'a>(
        &'a self,
        rb: crate::request::RequestBuilder<'a, TokioRuntime>,
    ) -> RequestBuilder<'a> {
        RequestBuilder {
            inner: rb,
            rt: Arc::clone(&self.rt),
        }
    }

    /// Start a GET request.
    pub fn get(&self, uri: &str) -> Result<RequestBuilder<'_>, Error> {
        Ok(self.request_builder(self.inner.get(uri)?))
    }

    /// Start a HEAD request.
    pub fn head(&self, uri: &str) -> Result<RequestBuilder<'_>, Error> {
        Ok(self.request_builder(self.inner.head(uri)?))
    }

    /// Start a POST request.
    pub fn post(&self, uri: &str) -> Result<RequestBuilder<'_>, Error> {
        Ok(self.request_builder(self.inner.post(uri)?))
    }

    /// Start a PUT request.
    pub fn put(&self, uri: &str) -> Result<RequestBuilder<'_>, Error> {
        Ok(self.request_builder(self.inner.put(uri)?))
    }

    /// Start a PATCH request.
    pub fn patch(&self, uri: &str) -> Result<RequestBuilder<'_>, Error> {
        Ok(self.request_builder(self.inner.patch(uri)?))
    }

    /// Start a DELETE request.
    pub fn delete(&self, uri: &str) -> Result<RequestBuilder<'_>, Error> {
        Ok(self.request_builder(self.inner.delete(uri)?))
    }

    /// Start a request with a custom method.
    pub fn request(&self, method: Method, uri: &str) -> Result<RequestBuilder<'_>, Error> {
        Ok(self.request_builder(self.inner.request(method, uri)?))
    }
}

impl Default for Client {
    fn default() -> Self {
        Self::new()
    }
}

/// Builder for configuring a blocking [`Client`].
pub struct ClientBuilder {
    inner: crate::client::ClientBuilder<TokioRuntime>,
}

impl ClientBuilder {
    pub fn pool_idle_timeout(mut self, timeout: Duration) -> Self {
        self.inner = self.inner.pool_idle_timeout(timeout);
        self
    }

    pub fn pool_max_idle_per_host(mut self, max: usize) -> Self {
        self.inner = self.inner.pool_max_idle_per_host(max);
        self
    }

    pub fn max_redirects(mut self, max: usize) -> Self {
        self.inner = self.inner.max_redirects(max);
        self
    }

    pub fn redirect_policy(mut self, policy: crate::RedirectPolicy) -> Self {
        self.inner = self.inner.redirect_policy(policy);
        self
    }

    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.inner = self.inner.timeout(timeout);
        self
    }

    pub fn connect_timeout(mut self, timeout: Duration) -> Self {
        self.inner = self.inner.connect_timeout(timeout);
        self
    }

    pub fn user_agent(mut self, value: impl AsRef<str>) -> Self {
        self.inner = self.inner.user_agent(value);
        self
    }

    pub fn https_only(mut self, enable: bool) -> Self {
        self.inner = self.inner.https_only(enable);
        self
    }

    pub fn default_headers(mut self, headers: HeaderMap) -> Self {
        self.inner = self.inner.default_headers(headers);
        self
    }

    pub fn retry(mut self, config: crate::RetryConfig) -> Self {
        self.inner = self.inner.retry(config);
        self
    }

    pub fn cookie_jar(mut self, jar: crate::CookieJar) -> Self {
        self.inner = self.inner.cookie_jar(jar);
        self
    }

    pub fn resolver(mut self, resolver: impl crate::Resolve) -> Self {
        self.inner = self.inner.resolver(resolver);
        self
    }

    pub fn proxy(mut self, config: crate::ProxyConfig) -> Self {
        self.inner = self.inner.proxy(config);
        self
    }

    pub fn system_proxy(mut self) -> Self {
        self.inner = self.inner.system_proxy();
        self
    }

    pub fn no_connection_reuse(mut self) -> Self {
        self.inner = self.inner.no_connection_reuse();
        self
    }

    pub fn no_decompression(mut self) -> Self {
        self.inner = self.inner.no_decompression();
        self
    }

    #[cfg(unix)]
    pub fn unix_socket(mut self, path: impl Into<std::path::PathBuf>) -> Self {
        self.inner = self.inner.unix_socket(path);
        self
    }

    pub fn cache(mut self, cache: crate::cache::HttpCache) -> Self {
        self.inner = self.inner.cache(cache);
        self
    }

    #[cfg(feature = "rustls")]
    pub fn tls(mut self, connector: crate::tls::RustlsConnector) -> Self {
        self.inner = self.inner.tls(connector);
        self
    }

    #[cfg(feature = "rustls")]
    pub fn danger_accept_invalid_certs(mut self) -> Self {
        self.inner = self.inner.danger_accept_invalid_certs();
        self
    }

    #[cfg(feature = "rustls")]
    pub fn add_root_certificates(mut self, certs: &[crate::Certificate]) -> Self {
        self.inner = self.inner.add_root_certificates(certs);
        self
    }

    /// Build the blocking client.
    pub fn build(self) -> Client {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("failed to create tokio runtime for blocking client");
        let _guard = rt.enter();
        Client {
            inner: self.inner.build(),
            rt: Arc::new(rt),
        }
    }
}

/// A blocking request builder.
pub struct RequestBuilder<'a> {
    inner: crate::request::RequestBuilder<'a, TokioRuntime>,
    rt: Arc<tokio::runtime::Runtime>,
}

impl RequestBuilder<'_> {
    pub fn header(
        mut self,
        name: http::header::HeaderName,
        value: http::header::HeaderValue,
    ) -> Self {
        self.inner = self.inner.header(name, value);
        self
    }

    pub fn headers(mut self, headers: HeaderMap) -> Self {
        self.inner = self.inner.headers(headers);
        self
    }

    pub fn bearer_auth(mut self, token: &str) -> Self {
        self.inner = self.inner.bearer_auth(token);
        self
    }

    pub fn basic_auth(mut self, username: &str, password: Option<&str>) -> Self {
        self.inner = self.inner.basic_auth(username, password);
        self
    }

    pub fn body(mut self, body: impl Into<Bytes>) -> Self {
        self.inner = self.inner.body(body);
        self
    }

    #[cfg(feature = "json")]
    pub fn json<T: serde::Serialize>(mut self, value: &T) -> Result<Self, Error> {
        self.inner = self.inner.json(value)?;
        Ok(self)
    }

    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.inner = self.inner.timeout(timeout);
        self
    }

    pub fn retry(mut self, config: crate::RetryConfig) -> Self {
        self.inner = self.inner.retry(config);
        self
    }

    pub fn version(mut self, version: Version) -> Self {
        self.inner = self.inner.version(version);
        self
    }

    /// Send the request and block until the response is received.
    pub fn send(self) -> Result<Response, Error> {
        let resp = self.rt.block_on(self.inner.send())?;
        Ok(Response {
            inner: resp,
            rt: self.rt,
        })
    }
}

/// A blocking HTTP response.
pub struct Response {
    inner: crate::Response,
    rt: Arc<tokio::runtime::Runtime>,
}

impl std::fmt::Debug for Response {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.inner.fmt(f)
    }
}

impl Response {
    pub fn status(&self) -> StatusCode {
        self.inner.status()
    }

    pub fn headers(&self) -> &HeaderMap {
        self.inner.headers()
    }

    pub fn version(&self) -> Version {
        self.inner.version()
    }

    pub fn url(&self) -> &Uri {
        self.inner.url()
    }

    pub fn remote_addr(&self) -> Option<SocketAddr> {
        self.inner.remote_addr()
    }

    pub fn content_length(&self) -> Option<u64> {
        self.inner.content_length()
    }

    pub fn tls_info(&self) -> Option<&crate::tls::TlsInfo> {
        self.inner.tls_info()
    }

    pub fn error_for_status(self) -> Result<Self, Error> {
        let rt = self.rt;
        let inner = self.inner.error_for_status()?;
        Ok(Self { inner, rt })
    }

    pub fn error_for_status_ref(&self) -> Result<&Self, Error> {
        self.inner.error_for_status_ref()?;
        Ok(self)
    }

    pub fn bytes(self) -> Result<Bytes, Error> {
        self.rt.block_on(self.inner.bytes())
    }

    pub fn text(self) -> Result<String, Error> {
        self.rt.block_on(self.inner.text())
    }

    #[cfg(feature = "json")]
    pub fn json<T: serde::de::DeserializeOwned>(self) -> Result<T, Error> {
        self.rt.block_on(self.inner.json())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_creates_client() {
        let client = Client::new();
        let _clone = client.clone();
    }

    #[test]
    fn default_creates_client() {
        let _client = Client::default();
    }

    #[test]
    fn builder_pool_idle_timeout() {
        let _client = Client::builder()
            .pool_idle_timeout(Duration::from_secs(30))
            .build();
    }

    #[test]
    fn builder_pool_max_idle_per_host() {
        let _client = Client::builder().pool_max_idle_per_host(5).build();
    }

    #[test]
    fn builder_max_redirects() {
        let _client = Client::builder().max_redirects(3).build();
    }

    #[test]
    fn builder_redirect_policy() {
        let _client = Client::builder()
            .redirect_policy(crate::RedirectPolicy::none())
            .build();
    }

    #[test]
    fn builder_timeout() {
        let _client = Client::builder().timeout(Duration::from_secs(5)).build();
    }

    #[test]
    fn builder_connect_timeout() {
        let _client = Client::builder()
            .connect_timeout(Duration::from_secs(2))
            .build();
    }

    #[test]
    fn builder_user_agent() {
        let _client = Client::builder().user_agent("test-agent/1.0").build();
    }

    #[test]
    fn builder_https_only() {
        let _client = Client::builder().https_only(true).build();
    }

    #[test]
    fn builder_default_headers() {
        let mut headers = HeaderMap::new();
        headers.insert(
            http::header::ACCEPT,
            http::header::HeaderValue::from_static("application/json"),
        );
        let _client = Client::builder().default_headers(headers).build();
    }

    #[test]
    fn builder_retry() {
        let _client = Client::builder()
            .retry(crate::RetryConfig::default())
            .build();
    }

    #[test]
    fn builder_no_connection_reuse() {
        let _client = Client::builder().no_connection_reuse().build();
    }

    #[test]
    fn builder_no_decompression() {
        let _client = Client::builder().no_decompression().build();
    }

    #[test]
    fn builder_cache() {
        let _client = Client::builder()
            .cache(crate::cache::HttpCache::new())
            .build();
    }

    #[test]
    fn builder_cookie_jar() {
        let _client = Client::builder()
            .cookie_jar(crate::CookieJar::new())
            .build();
    }

    #[test]
    fn builder_system_proxy() {
        let _client = Client::builder().system_proxy().build();
    }

    #[test]
    fn client_method_helpers() {
        let client = Client::new();
        assert!(client.get("http://example.com").is_ok());
        assert!(client.head("http://example.com").is_ok());
        assert!(client.post("http://example.com").is_ok());
        assert!(client.put("http://example.com").is_ok());
        assert!(client.patch("http://example.com").is_ok());
        assert!(client.delete("http://example.com").is_ok());
        assert!(
            client
                .request(Method::OPTIONS, "http://example.com")
                .is_ok()
        );
    }

    #[test]
    fn client_invalid_url() {
        let client = Client::new();
        assert!(client.get("not a url").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn builder_unix_socket() {
        let _client = Client::builder().unix_socket("/tmp/test.sock").build();
    }
}
