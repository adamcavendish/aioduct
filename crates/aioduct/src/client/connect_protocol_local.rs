use std::future::Future;
use std::pin::Pin;

use crate::body::RequestBodyLocal;
use crate::error::Error;
use crate::pool::PooledConnection;
use crate::runtime::{ConnectorLocal, RuntimeLocal};

use super::HttpEngineLocal;

impl<R: RuntimeLocal, C: ConnectorLocal + Clone> HttpEngineLocal<R, C> {
    pub(crate) fn connect_plaintext_local<S>(
        &self,
        stream: S,
    ) -> Pin<Box<dyn Future<Output = Result<PooledConnection<RequestBodyLocal>, Error>> + '_>>
    where
        S: hyper::rt::Read + hyper::rt::Write + Unpin + 'static,
    {
        self.connect_plaintext_local_with_hint(stream, false)
    }

    pub(crate) fn connect_plaintext_local_with_hint<S>(
        &self,
        stream: S,
        force_h2c: bool,
    ) -> Pin<Box<dyn Future<Output = Result<PooledConnection<RequestBodyLocal>, Error>> + '_>>
    where
        S: hyper::rt::Read + hyper::rt::Write + Unpin + 'static,
    {
        if self.core.http2_prior_knowledge || force_h2c {
            Box::pin(self.connect_h2_prior_knowledge_local(stream))
        } else {
            Box::pin(self.connect_h1_local(stream))
        }
    }

    pub(crate) async fn connect_h1_local<S>(
        &self,
        stream: S,
    ) -> Result<PooledConnection<RequestBodyLocal>, Error>
    where
        S: hyper::rt::Read + hyper::rt::Write + Unpin + 'static,
    {
        let (sender, conn) = hyper::client::conn::http1::handshake(stream).await?;

        let handle = crate::upgrade::UpgradeHandleLocal::new();
        let handle_clone = handle.clone();

        R::spawn_local(async move {
            match conn.without_shutdown().await {
                Ok(parts) => {
                    let upgraded = crate::upgrade::UpgradedLocal::new(parts.io, parts.read_buf);
                    handle_clone.fulfill(upgraded);
                }
                Err(_) => {
                    handle_clone.fail();
                }
            }
        });

        let mut pooled = PooledConnection::new_h1(sender);
        pooled.upgrade_handle_local = Some(handle);
        Ok(pooled)
    }

    pub(crate) async fn connect_h2_prior_knowledge_local<S>(
        &self,
        stream: S,
    ) -> Result<PooledConnection<RequestBodyLocal>, Error>
    where
        S: hyper::rt::Read + hyper::rt::Write + Unpin + 'static,
    {
        let mut builder = hyper::client::conn::http2::Builder::new(
            crate::runtime::executor::completion_executor::<R>(),
        );
        if let Some(ref h2) = self.core.http2 {
            h2.apply(&mut builder);
        }
        let (sender, conn) = builder.handshake(stream).await?;
        R::spawn_local(async move {
            let _ = conn.await;
        });
        Ok(PooledConnection::new_h2(sender))
    }

    #[cfg(all(feature = "rustls", feature = "compio"))]
    pub(crate) async fn connect_tls_local(
        &self,
        tcp_stream: C::Stream,
        host: &str,
    ) -> Result<PooledConnection<RequestBodyLocal>, Error> {
        use crate::tls::TlsConnectLocal;
        use std::time::Instant;

        let tls_start = Instant::now();

        let tls_connector = self
            .core
            .tls
            .as_ref()
            .ok_or_else(|| Error::Tls("no TLS connector configured".into()))?;

        let tls_stream =
            <crate::tls::RustlsConnector as TlsConnectLocal<C::Stream>>::connect_local(
                tls_connector,
                host,
                tcp_stream,
            )
            .await
            .map_err(|e| Error::Tls(Box::new(e)))?;

        let tls_duration = tls_start.elapsed();

        let alpn = crate::tls::RustlsConnector::negotiated_protocol(tls_stream.tls_connection());
        let tls_info = tls_stream.tls_info();

        match alpn {
            Some(crate::tls::AlpnProtocol::H2) => {
                let mut builder = hyper::client::conn::http2::Builder::new(
                    crate::runtime::executor::completion_executor::<R>(),
                );
                if let Some(ref h2) = self.core.http2 {
                    h2.apply(&mut builder);
                }
                let (sender, conn) = builder.handshake(tls_stream).await?;
                R::spawn_local(async move {
                    let _ = conn.await;
                });
                let mut pooled = PooledConnection::new_h2(sender);
                pooled.tls_info = Some(tls_info);
                pooled.tls_handshake_duration = Some(tls_duration);
                Ok(pooled)
            }
            _ => {
                let (sender, conn) = hyper::client::conn::http1::handshake(tls_stream).await?;

                let handle = crate::upgrade::UpgradeHandleLocal::new();
                let handle_clone = handle.clone();

                R::spawn_local(async move {
                    match conn.without_shutdown().await {
                        Ok(parts) => {
                            let upgraded =
                                crate::upgrade::UpgradedLocal::new(parts.io, parts.read_buf);
                            handle_clone.fulfill(upgraded);
                        }
                        Err(_) => {
                            handle_clone.fail();
                        }
                    }
                });

                let mut pooled = PooledConnection::new_h1(sender);
                pooled.tls_info = Some(tls_info);
                pooled.tls_handshake_duration = Some(tls_duration);
                pooled.upgrade_handle_local = Some(handle);
                Ok(pooled)
            }
        }
    }

    #[cfg(all(feature = "rustls", not(feature = "compio")))]
    pub(crate) async fn connect_tls_local(
        &self,
        _tcp_stream: C::Stream,
        _host: &str,
    ) -> Result<PooledConnection<RequestBodyLocal>, Error> {
        Err(Error::Tls(
            "TLS with !Send streams requires the compio feature".into(),
        ))
    }

    #[cfg(not(feature = "rustls"))]
    pub(crate) async fn connect_tls_local(
        &self,
        _tcp_stream: C::Stream,
        _host: &str,
    ) -> Result<PooledConnection<RequestBodyLocal>, Error> {
        Err(Error::Tls(
            "HTTPS requires the `rustls` TLS backend feature".into(),
        ))
    }
}
