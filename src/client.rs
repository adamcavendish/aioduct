use std::marker::PhantomData;
#[cfg(feature = "rustls")]
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use http::header::{HOST, LOCATION};
use http::{Method, StatusCode, Uri};
use http_body_util::BodyExt;

use crate::error::{Error, HyperBody, Result};
use crate::pool::{ConnectionPool, HttpConnection, PooledConnection};
use crate::request::RequestBuilder;
use crate::response::Response;
use crate::runtime::Runtime;

const DEFAULT_MAX_REDIRECTS: usize = 10;

pub struct Client<R: Runtime> {
    pool: ConnectionPool<R>,
    max_redirects: usize,
    #[cfg(feature = "rustls")]
    tls: Option<Arc<crate::tls::RustlsConnector>>,
    _runtime: PhantomData<R>,
}

pub struct ClientBuilder<R: Runtime> {
    pool_idle_timeout: Duration,
    pool_max_idle_per_host: usize,
    max_redirects: usize,
    #[cfg(feature = "rustls")]
    tls: Option<Arc<crate::tls::RustlsConnector>>,
    _runtime: PhantomData<R>,
}

impl<R: Runtime> Default for ClientBuilder<R> {
    fn default() -> Self {
        Self {
            pool_idle_timeout: Duration::from_secs(90),
            pool_max_idle_per_host: 10,
            max_redirects: DEFAULT_MAX_REDIRECTS,
            #[cfg(feature = "rustls")]
            tls: None,
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
        self.max_redirects = max;
        self
    }

    #[cfg(feature = "rustls")]
    pub fn tls(mut self, connector: crate::tls::RustlsConnector) -> Self {
        self.tls = Some(Arc::new(connector));
        self
    }

    pub fn build(self) -> Client<R> {
        Client {
            pool: ConnectionPool::new(self.pool_max_idle_per_host, self.pool_idle_timeout),
            max_redirects: self.max_redirects,
            #[cfg(feature = "rustls")]
            tls: self.tls,
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

    pub fn get(&self, uri: &str) -> Result<RequestBuilder<'_, R>> {
        let uri: Uri = uri.parse().map_err(|e| Error::InvalidUrl(format!("{e}")))?;
        Ok(RequestBuilder::new(self, Method::GET, uri))
    }

    pub fn post(&self, uri: &str) -> Result<RequestBuilder<'_, R>> {
        let uri: Uri = uri.parse().map_err(|e| Error::InvalidUrl(format!("{e}")))?;
        Ok(RequestBuilder::new(self, Method::POST, uri))
    }

    pub fn put(&self, uri: &str) -> Result<RequestBuilder<'_, R>> {
        let uri: Uri = uri.parse().map_err(|e| Error::InvalidUrl(format!("{e}")))?;
        Ok(RequestBuilder::new(self, Method::PUT, uri))
    }

    pub fn delete(&self, uri: &str) -> Result<RequestBuilder<'_, R>> {
        let uri: Uri = uri.parse().map_err(|e| Error::InvalidUrl(format!("{e}")))?;
        Ok(RequestBuilder::new(self, Method::DELETE, uri))
    }

    pub fn request(&self, method: Method, uri: &str) -> Result<RequestBuilder<'_, R>> {
        let uri: Uri = uri.parse().map_err(|e| Error::InvalidUrl(format!("{e}")))?;
        Ok(RequestBuilder::new(self, method, uri))
    }

    pub(crate) async fn execute(
        &self,
        method: Method,
        original_uri: Uri,
        headers: http::HeaderMap,
        body: Option<Bytes>,
    ) -> Result<Response> {
        let mut current_uri = original_uri;
        let mut current_method = method;
        let mut current_body = body;
        let mut current_headers = headers;

        for _ in 0..=self.max_redirects {
            let req_body: HyperBody = match &current_body {
                Some(b) => http_body_util::Full::new(b.clone())
                    .map_err(|never| match never {})
                    .boxed(),
                None => http_body_util::Full::new(Bytes::new())
                    .map_err(|never| match never {})
                    .boxed(),
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

            for (name, value) in &current_headers {
                builder = builder.header(name, value);
            }

            let request = builder.body(req_body)?;

            let resp = self.execute_single(request, &current_uri).await?;

            if !resp.status().is_redirection() || self.max_redirects == 0 {
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

            let _ = resp.bytes().await;

            match status {
                StatusCode::MOVED_PERMANENTLY | StatusCode::FOUND | StatusCode::SEE_OTHER => {
                    current_method = Method::GET;
                    current_body = None;
                }
                StatusCode::TEMPORARY_REDIRECT | StatusCode::PERMANENT_REDIRECT => {}
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
            format!("too many redirects (max {})", self.max_redirects).into(),
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

        let pool_key = crate::pool::PoolKey::new(scheme.clone(), authority.clone());

        if let Some(mut conn) = self.pool.checkout(&pool_key) {
            if conn.is_ready() {
                let resp = Self::send_on_connection(&mut conn, request).await?;
                self.pool.checkin(pool_key, conn);
                return Ok(resp);
            }
        }

        let is_https = scheme == &http::uri::Scheme::HTTPS;
        let default_port = if is_https { 443 } else { 80 };
        let addr = Self::resolve_authority(authority, default_port).await?;
        let tcp_stream = R::connect(addr).await?;

        let mut pooled = if is_https {
            self.connect_tls(tcp_stream, authority.host()).await?
        } else {
            self.connect_h1(tcp_stream).await?
        };

        let resp = Self::send_on_connection(&mut pooled, request).await?;
        self.pool.checkin(pool_key, pooled);

        Ok(resp)
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
