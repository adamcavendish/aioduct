use std::future::Future;
use std::pin::Pin;

use crate::body::RequestBodySend;
use crate::error::Error;
use crate::pool::PooledConnection;
use crate::runtime::{ConnectorSend, RuntimePoll};

use super::HttpEngineSend;

impl<R: RuntimePoll, C: ConnectorSend> HttpEngineSend<R, C> {
    pub(crate) fn connect_plaintext_with_hint<S>(
        &self,
        stream: S,
        force_h2c: bool,
    ) -> Pin<Box<dyn Future<Output = Result<PooledConnection<RequestBodySend>, Error>> + Send + '_>>
    where
        S: hyper::rt::Read + hyper::rt::Write + Send + Unpin + 'static,
    {
        if force_h2c {
            Box::pin(self.connect_h2_prior_knowledge(stream))
        } else {
            Box::pin(self.connect_h1(stream))
        }
    }

    pub(crate) async fn connect_h1<S>(
        &self,
        stream: S,
    ) -> Result<PooledConnection<RequestBodySend>, Error>
    where
        S: hyper::rt::Read + hyper::rt::Write + Send + Unpin + 'static,
    {
        let (sender, conn) = hyper::client::conn::http1::handshake(stream).await?;
        R::spawn_send(async move {
            let _ = conn.with_upgrades().await;
        });
        Ok(PooledConnection::new_h1(sender))
    }

    pub(crate) async fn connect_h2_prior_knowledge<S>(
        &self,
        stream: S,
    ) -> Result<PooledConnection<RequestBodySend>, Error>
    where
        S: hyper::rt::Read + hyper::rt::Write + Send + Unpin + 'static,
    {
        let mut builder = hyper::client::conn::http2::Builder::new(
            crate::runtime::executor::poll_executor::<R>(),
        );
        if let Some(ref h2) = self.core.http2 {
            h2.apply(&mut builder);
        }
        let (sender, conn) = builder.handshake(stream).await?;
        R::spawn_send(async move {
            let _ = conn.await;
        });
        Ok(PooledConnection::new_h2(sender))
    }

    #[cfg(feature = "rustls")]
    pub(crate) async fn connect_tls(
        &self,
        tcp_stream: C::Stream,
        host: &str,
    ) -> Result<PooledConnection<RequestBodySend>, Error> {
        use crate::tls::TlsConnect;
        use std::time::Instant;

        #[cfg(feature = "tracing")]
        tracing::trace!(host = host, "tls.handshake.start");

        let tls_start = Instant::now();

        let tls_connector = self
            .core
            .tls
            .as_ref()
            .ok_or_else(|| Error::Tls("no TLS connector configured".into()))?;

        let tls_stream = <crate::tls::RustlsConnector as TlsConnect<C::Stream>>::connect(
            tls_connector,
            host,
            tcp_stream,
        )
        .await
        .map_err(|e| {
            #[cfg(feature = "tracing")]
            tracing::trace!(host = host, error = %e, "tls.handshake.error");
            Error::Tls(Box::new(e))
        })?;

        let tls_duration = tls_start.elapsed();

        let alpn = crate::tls::RustlsConnector::negotiated_protocol(tls_stream.tls_connection());

        #[cfg(feature = "tracing")]
        tracing::trace!(
            host = host,
            alpn = ?alpn,
            "tls.handshake.done",
        );
        let tls_info = tls_stream.tls_info();

        match alpn {
            Some(crate::tls::AlpnProtocol::H2) => {
                let mut builder = hyper::client::conn::http2::Builder::new(
                    crate::runtime::executor::poll_executor::<R>(),
                );
                if let Some(ref h2) = self.core.http2 {
                    h2.apply(&mut builder);
                }
                let (sender, conn) = builder.handshake(tls_stream).await?;
                R::spawn_send(async move {
                    let _ = conn.await;
                });
                let mut pooled = PooledConnection::new_h2(sender);
                pooled.tls_info = Some(tls_info);
                pooled.tls_handshake_duration = Some(tls_duration);
                Ok(pooled)
            }
            _ => {
                let (sender, conn) = hyper::client::conn::http1::handshake(tls_stream).await?;
                R::spawn_send(async move {
                    let _ = conn.with_upgrades().await;
                });
                let mut pooled = PooledConnection::new_h1(sender);
                pooled.tls_info = Some(tls_info);
                pooled.tls_handshake_duration = Some(tls_duration);
                Ok(pooled)
            }
        }
    }

    #[cfg(not(feature = "rustls"))]
    pub(crate) async fn connect_tls(
        &self,
        _tcp_stream: C::Stream,
        _host: &str,
    ) -> Result<PooledConnection<RequestBodySend>, Error> {
        Err(Error::Tls(
            "HTTPS requires the `rustls` TLS backend feature".into(),
        ))
    }
}
