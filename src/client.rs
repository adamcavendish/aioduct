use std::marker::PhantomData;
use std::pin::Pin;
#[cfg(feature = "rustls")]
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use http::header::{HOST, HeaderMap, HeaderValue, LOCATION, USER_AGENT};
use http::{Method, StatusCode, Uri};
use http_body_util::BodyExt;

use crate::body::RequestBody;
use crate::cookie::CookieJar;
use crate::error::{Error, HyperBody, Result};
use crate::pool::{ConnectionPool, HttpConnection, PooledConnection};
use crate::proxy::ProxyConfig;
use crate::redirect::{RedirectAction, RedirectPolicy};
use crate::request::RequestBuilder;
use crate::response::Response;
use crate::retry::RetryConfig;
use crate::runtime::Runtime;

const DEFAULT_USER_AGENT: &str = concat!("aioduct/", env!("CARGO_PKG_VERSION"));

pub struct Client<R: Runtime> {
    pool: ConnectionPool<R>,
    redirect_policy: RedirectPolicy,
    timeout: Option<Duration>,
    default_headers: HeaderMap,
    retry: Option<RetryConfig>,
    cookie_jar: Option<CookieJar>,
    proxy: Option<ProxyConfig>,
    #[cfg(feature = "rustls")]
    tls: Option<Arc<crate::tls::RustlsConnector>>,
    #[cfg(feature = "http3")]
    h3_endpoint: Option<quinn::Endpoint>,
    _runtime: PhantomData<R>,
}

impl<R: Runtime> Clone for Client<R> {
    fn clone(&self) -> Self {
        Self {
            pool: self.pool.clone(),
            redirect_policy: self.redirect_policy.clone(),
            timeout: self.timeout,
            default_headers: self.default_headers.clone(),
            retry: self.retry.clone(),
            cookie_jar: self.cookie_jar.clone(),
            proxy: self.proxy.clone(),
            #[cfg(feature = "rustls")]
            tls: self.tls.clone(),
            #[cfg(feature = "http3")]
            h3_endpoint: self.h3_endpoint.clone(),
            _runtime: PhantomData,
        }
    }
}

pub struct ClientBuilder<R: Runtime> {
    pool_idle_timeout: Duration,
    pool_max_idle_per_host: usize,
    redirect_policy: RedirectPolicy,
    timeout: Option<Duration>,
    default_headers: HeaderMap,
    retry: Option<RetryConfig>,
    cookie_jar: Option<CookieJar>,
    proxy: Option<ProxyConfig>,
    #[cfg(feature = "rustls")]
    tls: Option<Arc<crate::tls::RustlsConnector>>,
    #[cfg(feature = "http3")]
    h3_endpoint: Option<quinn::Endpoint>,
    _runtime: PhantomData<R>,
}

impl<R: Runtime> Default for ClientBuilder<R> {
    fn default() -> Self {
        let mut default_headers = HeaderMap::new();
        default_headers.insert(USER_AGENT, HeaderValue::from_static(DEFAULT_USER_AGENT));

        Self {
            pool_idle_timeout: Duration::from_secs(90),
            pool_max_idle_per_host: 10,
            redirect_policy: RedirectPolicy::default(),
            timeout: None,
            default_headers,
            retry: None,
            cookie_jar: None,
            proxy: None,
            #[cfg(feature = "rustls")]
            tls: None,
            #[cfg(feature = "http3")]
            h3_endpoint: None,
            _runtime: PhantomData,
        }
    }
}

impl<R: Runtime> ClientBuilder<R> {
    pub fn pool_idle_timeout(mut self, timeout: Duration) -> Self {
        self.pool_idle_timeout = timeout;
        self
    }

    pub fn pool_max_idle_per_host(mut self, max: usize) -> Self {
        self.pool_max_idle_per_host = max;
        self
    }

    pub fn max_redirects(mut self, max: usize) -> Self {
        self.redirect_policy = RedirectPolicy::limited(max);
        self
    }

    pub fn redirect_policy(mut self, policy: RedirectPolicy) -> Self {
        self.redirect_policy = policy;
        self
    }

    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    pub fn default_headers(mut self, headers: HeaderMap) -> Self {
        self.default_headers.extend(headers);
        self
    }

    pub fn no_default_headers(mut self) -> Self {
        self.default_headers.clear();
        self
    }

    pub fn retry(mut self, config: RetryConfig) -> Self {
        self.retry = Some(config);
        self
    }

    pub fn cookie_jar(mut self, jar: CookieJar) -> Self {
        self.cookie_jar = Some(jar);
        self
    }

    pub fn proxy(mut self, config: ProxyConfig) -> Self {
        self.proxy = Some(config);
        self
    }

    #[cfg(feature = "rustls")]
    pub fn tls(mut self, connector: crate::tls::RustlsConnector) -> Self {
        self.tls = Some(Arc::new(connector));
        self
    }

    #[cfg(feature = "http3")]
    pub fn http3(mut self, enable: bool) -> Self {
        if enable {
            let tls_config = self
                .tls
                .as_ref()
                .expect("HTTP/3 requires a TLS connector — call .tls() before .http3(true)")
                .config()
                .clone();
            let endpoint = crate::h3_transport::build_quinn_endpoint(tls_config)
                .expect("failed to build QUIC endpoint");
            self.h3_endpoint = Some(endpoint);
        } else {
            self.h3_endpoint = None;
        }
        self
    }

    pub fn build(self) -> Client<R> {
        Client {
            pool: ConnectionPool::new(self.pool_max_idle_per_host, self.pool_idle_timeout),
            redirect_policy: self.redirect_policy,
            timeout: self.timeout,
            default_headers: self.default_headers,
            retry: self.retry,
            cookie_jar: self.cookie_jar,
            proxy: self.proxy,
            #[cfg(feature = "rustls")]
            tls: self.tls,
            #[cfg(feature = "http3")]
            h3_endpoint: self.h3_endpoint,
            _runtime: PhantomData,
        }
    }
}

impl<R: Runtime> Default for Client<R> {
    fn default() -> Self {
        Self::new()
    }
}

impl<R: Runtime> Client<R> {
    pub fn builder() -> ClientBuilder<R> {
        ClientBuilder::default()
    }

    pub fn new() -> Self {
        Self::builder().build()
    }

    #[cfg(feature = "rustls")]
    pub fn with_rustls() -> Self {
        Self::builder()
            .tls(crate::tls::RustlsConnector::with_webpki_roots())
            .build()
    }

    #[cfg(feature = "http3")]
    pub fn with_http3() -> Self {
        Self::builder()
            .tls(crate::tls::RustlsConnector::with_webpki_roots())
            .http3(true)
            .build()
    }

    pub fn get(&self, uri: &str) -> Result<RequestBuilder<'_, R>> {
        let uri: Uri = uri.parse().map_err(|e| Error::InvalidUrl(format!("{e}")))?;
        Ok(RequestBuilder::new(self, Method::GET, uri))
    }

    pub fn head(&self, uri: &str) -> Result<RequestBuilder<'_, R>> {
        let uri: Uri = uri.parse().map_err(|e| Error::InvalidUrl(format!("{e}")))?;
        Ok(RequestBuilder::new(self, Method::HEAD, uri))
    }

    pub fn post(&self, uri: &str) -> Result<RequestBuilder<'_, R>> {
        let uri: Uri = uri.parse().map_err(|e| Error::InvalidUrl(format!("{e}")))?;
        Ok(RequestBuilder::new(self, Method::POST, uri))
    }

    pub fn put(&self, uri: &str) -> Result<RequestBuilder<'_, R>> {
        let uri: Uri = uri.parse().map_err(|e| Error::InvalidUrl(format!("{e}")))?;
        Ok(RequestBuilder::new(self, Method::PUT, uri))
    }

    pub fn patch(&self, uri: &str) -> Result<RequestBuilder<'_, R>> {
        let uri: Uri = uri.parse().map_err(|e| Error::InvalidUrl(format!("{e}")))?;
        Ok(RequestBuilder::new(self, Method::PATCH, uri))
    }

    pub fn delete(&self, uri: &str) -> Result<RequestBuilder<'_, R>> {
        let uri: Uri = uri.parse().map_err(|e| Error::InvalidUrl(format!("{e}")))?;
        Ok(RequestBuilder::new(self, Method::DELETE, uri))
    }

    pub fn request(&self, method: Method, uri: &str) -> Result<RequestBuilder<'_, R>> {
        let uri: Uri = uri.parse().map_err(|e| Error::InvalidUrl(format!("{e}")))?;
        Ok(RequestBuilder::new(self, method, uri))
    }

    pub fn chunk_download(&self, url: &str) -> crate::chunk_download::ChunkDownload<R> {
        crate::chunk_download::ChunkDownload::new(self.clone(), url.to_owned())
    }

    pub(crate) fn default_timeout(&self) -> Option<Duration> {
        self.timeout
    }

    pub(crate) fn default_retry(&self) -> Option<&RetryConfig> {
        self.retry.as_ref()
    }

    pub(crate) async fn execute(
        &self,
        method: Method,
        original_uri: Uri,
        headers: http::HeaderMap,
        body: Option<RequestBody>,
        version: Option<http::Version>,
    ) -> Result<Response> {
        let mut current_uri = original_uri;
        let mut current_method = method;
        let mut current_body = body;
        let mut current_headers = headers;

        for (name, value) in &self.default_headers {
            if !current_headers.contains_key(name) {
                current_headers.insert(name, value.clone());
            }
        }

        for _ in 0..=self.redirect_policy.max_redirects() {
            if let Some(jar) = &self.cookie_jar {
                if let Some(authority) = current_uri.authority() {
                    let is_secure = current_uri.scheme() == Some(&http::uri::Scheme::HTTPS);
                    jar.apply_to_request(authority.host(), is_secure, &mut current_headers);
                }
            }

            let (req_body, body_for_redirect) = match current_body.take() {
                Some(RequestBody::Buffered(b)) => {
                    let body_clone = RequestBody::Buffered(b.clone());
                    (RequestBody::Buffered(b).into_hyper_body(), Some(body_clone))
                }
                Some(rb @ RequestBody::Streaming(_)) => (rb.into_hyper_body(), None),
                None => {
                    let empty: HyperBody = http_body_util::Full::new(Bytes::new())
                        .map_err(|never| match never {})
                        .boxed();
                    (empty, None)
                }
            };

            if !current_headers.contains_key(HOST) {
                if let Some(authority) = current_uri.authority() {
                    if let Ok(host_value) = authority.as_str().parse() {
                        current_headers.insert(HOST, host_value);
                    }
                }
            }

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

            let request = builder.body(req_body)?;

            let resp = self.execute_single(request, &current_uri).await?;

            if let Some(jar) = &self.cookie_jar {
                if let Some(authority) = current_uri.authority() {
                    jar.store_from_response(authority.host(), resp.headers());
                }
            }

            if !resp.status().is_redirection()
                || matches!(self.redirect_policy, RedirectPolicy::None)
            {
                return Ok(resp);
            }

            let status = resp.status();
            let location = resp
                .headers()
                .get(LOCATION)
                .ok_or_else(|| Error::Other("redirect without Location header".into()))?
                .to_str()
                .map_err(|e| Error::Other(Box::new(e)))?
                .to_owned();

            let next_uri = resolve_redirect(&current_uri, &location)?;

            if self
                .redirect_policy
                .check(&current_uri, &next_uri, status, &current_method)
                == RedirectAction::Stop
            {
                let _ = resp.bytes().await;
                return Ok(Response::new(
                    http::Response::builder()
                        .status(status)
                        .header(LOCATION, location)
                        .body(
                            http_body_util::Full::new(Bytes::new())
                                .map_err(|never| match never {})
                                .boxed(),
                        )?,
                ));
            }

            let _ = resp.bytes().await;

            match status {
                StatusCode::MOVED_PERMANENTLY | StatusCode::FOUND | StatusCode::SEE_OTHER => {
                    current_method = Method::GET;
                    current_body = None;
                }
                StatusCode::TEMPORARY_REDIRECT | StatusCode::PERMANENT_REDIRECT => {
                    current_body = body_for_redirect;
                }
                _ => return Err(Error::Other("unexpected redirect status".into())),
            }

            // Update Host header for the new target
            if let Some(authority) = next_uri.authority() {
                if let Ok(host_value) = authority.as_str().parse() {
                    current_headers.insert(HOST, host_value);
                }
            }

            current_uri = next_uri;
        }

        Err(Error::Other(
            format!(
                "too many redirects (max {})",
                self.redirect_policy.max_redirects()
            )
            .into(),
        ))
    }

    async fn execute_single(
        &self,
        request: http::Request<HyperBody>,
        original_uri: &Uri,
    ) -> Result<Response> {
        let scheme = original_uri
            .scheme()
            .ok_or_else(|| Error::InvalidUrl("missing scheme".into()))?;
        let authority = original_uri
            .authority()
            .ok_or_else(|| Error::InvalidUrl("missing authority".into()))?;

        let is_https = scheme == &http::uri::Scheme::HTTPS;

        #[cfg(feature = "http3")]
        if is_https {
            if let Some(endpoint) = &self.h3_endpoint {
                let default_port = 443u16;
                let addr = Self::resolve_authority(authority, default_port).await?;
                let host = authority.host().to_owned();
                let quinn_conn = endpoint
                    .connect(addr, &host)
                    .map_err(|e| Error::Other(Box::new(e)))?
                    .await
                    .map_err(|e| Error::Other(Box::new(e)))?;
                return crate::h3_transport::send_h3_request::<R>(quinn_conn, request).await;
            }
        }

        let pool_key = crate::pool::PoolKey::new(scheme.clone(), authority.clone());

        if let Some(mut conn) = self.pool.checkout(&pool_key) {
            if conn.is_ready() {
                let resp = Self::send_on_connection(&mut conn, request).await?;
                self.pool.checkin(pool_key, conn);
                return Ok(resp);
            }
        }

        let mut pooled = if let Some(proxy) = &self.proxy {
            self.connect_via_proxy(proxy, authority, is_https).await?
        } else {
            let default_port = if is_https { 443 } else { 80 };
            let addr = Self::resolve_authority(authority, default_port).await?;
            let tcp_stream = R::connect(addr).await?;

            if is_https {
                self.connect_tls(tcp_stream, authority.host()).await?
            } else {
                self.connect_h1(tcp_stream).await?
            }
        };

        let resp = Self::send_on_connection(&mut pooled, request).await?;
        self.pool.checkin(pool_key, pooled);

        Ok(resp)
    }

    async fn connect_via_proxy(
        &self,
        proxy: &ProxyConfig,
        target_authority: &http::uri::Authority,
        is_https: bool,
    ) -> Result<PooledConnection<R>> {
        let proxy_authority = proxy.authority()?;
        let default_port = 80;
        let proxy_addr = Self::resolve_authority(proxy_authority, default_port).await?;
        let tcp_stream = R::connect(proxy_addr).await?;

        if is_https {
            self.connect_tunnel(tcp_stream, proxy, target_authority)
                .await
        } else {
            self.connect_h1(tcp_stream).await
        }
    }

    async fn connect_tunnel(
        &self,
        mut tcp_stream: R::TcpStream,
        proxy: &ProxyConfig,
        target_authority: &http::uri::Authority,
    ) -> Result<PooledConnection<R>> {
        use hyper::rt::{Read, Write};

        let target = target_authority.as_str();

        let mut connect_msg = format!("CONNECT {target} HTTP/1.1\r\nHost: {target}\r\n");
        if let Some(auth_value) = proxy.connect_header(target) {
            connect_msg.push_str(&format!("Proxy-Authorization: {auth_value}\r\n"));
        }
        connect_msg.push_str("\r\n");

        let buf = connect_msg.into_bytes();
        let mut written = 0;
        while written < buf.len() {
            let n = std::future::poll_fn(|cx| {
                Pin::new(&mut tcp_stream).poll_write(cx, &buf[written..])
            })
            .await
            .map_err(Error::Io)?;
            written += n;
        }

        let mut resp_buf = Vec::with_capacity(256);
        loop {
            let mut one = [0u8; 1];
            let mut read_buf = hyper::rt::ReadBuf::new(&mut one);
            std::future::poll_fn(|cx| Pin::new(&mut tcp_stream).poll_read(cx, read_buf.unfilled()))
                .await
                .map_err(Error::Io)?;

            if read_buf.filled().is_empty() {
                return Err(Error::Other("proxy closed connection".into()));
            }
            resp_buf.push(one[0]);
            read_buf = hyper::rt::ReadBuf::new(&mut one);

            if resp_buf.len() >= 4 && resp_buf[resp_buf.len() - 4..] == *b"\r\n\r\n" {
                break;
            }

            if resp_buf.len() > 8192 {
                return Err(Error::Other("CONNECT response too large".into()));
            }
        }

        let resp_str = String::from_utf8_lossy(&resp_buf);
        let status_line = resp_str
            .lines()
            .next()
            .ok_or_else(|| Error::Other("empty CONNECT response".into()))?;

        if !status_line.contains("200") {
            return Err(Error::Other(
                format!("CONNECT tunnel failed: {status_line}").into(),
            ));
        }

        self.connect_tls(tcp_stream, target_authority.host()).await
    }

    async fn connect_h1(&self, tcp_stream: R::TcpStream) -> Result<PooledConnection<R>> {
        let (sender, conn) = hyper::client::conn::http1::handshake(tcp_stream).await?;
        R::spawn(async move {
            let _ = conn.await;
        });
        Ok(PooledConnection::new_h1(sender))
    }

    #[cfg(feature = "rustls")]
    async fn connect_tls(
        &self,
        tcp_stream: R::TcpStream,
        host: &str,
    ) -> Result<PooledConnection<R>> {
        use crate::tls::TlsConnect;

        let tls_connector = self
            .tls
            .as_ref()
            .ok_or_else(|| Error::Tls("no TLS connector configured".into()))?;

        let tls_stream = <crate::tls::RustlsConnector as TlsConnect<R>>::connect(
            tls_connector,
            host,
            tcp_stream,
        )
        .await
        .map_err(|e| Error::Tls(Box::new(e)))?;

        let alpn = crate::tls::RustlsConnector::negotiated_protocol(tls_stream.tls_connection());

        match alpn {
            Some(crate::tls::AlpnProtocol::H2) => {
                let (sender, conn) = hyper::client::conn::http2::handshake(
                    crate::runtime::hyper_executor::<R>(),
                    tls_stream,
                )
                .await?;
                R::spawn(async move {
                    let _ = conn.await;
                });
                Ok(PooledConnection::new_h2(sender))
            }
            _ => {
                let (sender, conn) = hyper::client::conn::http1::handshake(tls_stream).await?;
                R::spawn(async move {
                    let _ = conn.await;
                });
                Ok(PooledConnection::new_h1(sender))
            }
        }
    }

    #[cfg(not(feature = "rustls"))]
    async fn connect_tls(
        &self,
        _tcp_stream: R::TcpStream,
        _host: &str,
    ) -> Result<PooledConnection<R>> {
        Err(Error::Tls("HTTPS requires the `rustls` feature".into()))
    }

    async fn send_on_connection(
        conn: &mut PooledConnection<R>,
        request: http::Request<HyperBody>,
    ) -> Result<Response> {
        match &mut conn.conn {
            HttpConnection::H1(sender) => {
                let resp = sender.send_request(request).await?;
                let resp = resp.map(|body| body.map_err(Error::Hyper).boxed());
                Ok(Response::new(resp))
            }
            HttpConnection::H2(sender) => {
                let resp = sender.send_request(request).await?;
                let resp = resp.map(|body| body.map_err(Error::Hyper).boxed());
                Ok(Response::new(resp))
            }
        }
    }

    async fn resolve_authority(
        authority: &http::uri::Authority,
        default_port: u16,
    ) -> Result<std::net::SocketAddr> {
        let host = authority.host();
        let port = authority.port_u16().unwrap_or(default_port);

        if let Ok(addr) = format!("{host}:{port}").parse() {
            return Ok(addr);
        }

        R::resolve(host, port)
            .await
            .map_err(|e| Error::InvalidUrl(format!("cannot resolve {host}:{port}: {e}")))
    }
}

fn resolve_redirect(base: &Uri, location: &str) -> Result<Uri> {
    if let Ok(absolute) = location.parse::<Uri>() {
        if absolute.scheme().is_some() {
            return Ok(absolute);
        }
    }

    let scheme = base
        .scheme_str()
        .ok_or_else(|| Error::InvalidUrl("missing scheme in base".into()))?;
    let authority = base
        .authority()
        .ok_or_else(|| Error::InvalidUrl("missing authority in base".into()))?;

    let new_uri = format!("{scheme}://{authority}{location}");
    new_uri
        .parse()
        .map_err(|e| Error::InvalidUrl(format!("invalid redirect URL: {e}")))
}
