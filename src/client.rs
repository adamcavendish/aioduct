use std::marker::PhantomData;
#[cfg(feature = "rustls")]
use std::sync::Arc;
use std::time::Duration;

use http::{Method, Uri};
use http_body_util::BodyExt;

use crate::error::{Error, HyperBody, Result};
use crate::pool::{ConnectionPool, HttpConnection, PooledConnection};
use crate::request::RequestBuilder;
use crate::response::Response;
use crate::runtime::Runtime;

pub struct Client<R: Runtime> {
    pool: ConnectionPool<R>,
    #[cfg(feature = "rustls")]
    tls: Option<Arc<crate::tls::RustlsConnector>>,
    _runtime: PhantomData<R>,
}

pub struct ClientBuilder<R: Runtime> {
    pool_idle_timeout: Duration,
    pool_max_idle_per_host: usize,
    #[cfg(feature = "rustls")]
    tls: Option<Arc<crate::tls::RustlsConnector>>,
    _runtime: PhantomData<R>,
}

impl<R: Runtime> Default for ClientBuilder<R> {
    fn default() -> Self {
        Self {
            pool_idle_timeout: Duration::from_secs(90),
            pool_max_idle_per_host: 10,
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

    #[cfg(feature = "rustls")]
    pub fn tls(mut self, connector: crate::tls::RustlsConnector) -> Self {
        self.tls = Some(Arc::new(connector));
        self
    }

    pub fn build(self) -> Client<R> {
        Client {
            pool: ConnectionPool::new(self.pool_max_idle_per_host, self.pool_idle_timeout),
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

    pub(crate) async fn execute(&self, request: http::Request<HyperBody>) -> Result<Response> {
        let uri = request.uri().clone();
        let scheme = uri
            .scheme()
            .ok_or_else(|| Error::InvalidUrl("missing scheme".into()))?;
        let authority = uri
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

        let pooled = if is_https {
            self.connect_tls(tcp_stream, authority.host()).await?
        } else {
            self.connect_h1(tcp_stream).await?
        };

        let mut pooled = pooled;
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
        let addr_str = format!("{host}:{port}");
        addr_str
            .parse()
            .map_err(|e| Error::InvalidUrl(format!("cannot resolve {addr_str}: {e}")))
    }
}
