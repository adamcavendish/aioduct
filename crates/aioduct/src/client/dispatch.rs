use std::time::Instant;

use http::Uri;

use crate::error::{AioductBody, Error};
use crate::pool::{HttpConnection, PooledConnection};
use crate::response::Response;
use crate::runtime::Runtime;
use crate::timing::TimingCollector;

use super::Client;

impl<R: Runtime> Client<R> {
    pub(crate) async fn execute_single(
        &self,
        request: http::Request<AioductBody>,
        original_uri: &Uri,
    ) -> Result<Response, Error> {
        let request_start = Instant::now();

        if let Some(ref limiter) = self.rate_limiter {
            while !limiter.try_acquire() {
                let wait = limiter.wait_duration();
                R::sleep(wait).await;
            }
        }

        let scheme = original_uri
            .scheme()
            .ok_or_else(|| Error::InvalidUrl("missing scheme".into()))?;
        let authority = original_uri
            .authority()
            .ok_or_else(|| Error::InvalidUrl("missing authority".into()))?;

        let is_https = scheme == &http::uri::Scheme::HTTPS;

        let pool_key = crate::pool::PoolKey::new(scheme.clone(), authority.clone());

        if !self.no_connection_reuse
            && let Some(mut conn) = self.pool.checkout(&pool_key)
        {
            #[cfg(feature = "tracing")]
            tracing::trace!(host = authority.host(), "connection.pool.hit");

            let transfer_start = Instant::now();
            let mut resp =
                Self::send_on_connection(&mut conn, request, original_uri.clone()).await?;
            let transfer = transfer_start.elapsed();
            resp.set_remote_addr(conn.remote_addr);
            resp.set_tls_info(conn.tls_info.clone());
            resp.set_timings(Some(
                TimingCollector::default().into_timings(Some(transfer), request_start.elapsed()),
            ));
            if resp.status() != http::StatusCode::SWITCHING_PROTOCOLS {
                self.pool.checkin(pool_key, conn);
            }
            return Ok(resp);
        }

        #[cfg(all(feature = "http3", feature = "rustls"))]
        if is_https && let Some(endpoint) = &self.h3_endpoint {
            let use_h3 = self.prefer_h3 || self.alt_svc_cache.lookup_h3(authority).is_some();
            if use_h3 {
                let default_port = 443u16;
                let (h3_host, h3_port) = self
                    .alt_svc_cache
                    .lookup_h3(authority)
                    .unwrap_or_else(|| (None, authority.port_u16().unwrap_or(default_port)));
                let connect_host = h3_host.as_deref().unwrap_or(authority.host());
                let addrs = self
                    .resolve_all_authority_raw(connect_host, h3_port)
                    .await?;
                let sni_host = authority.host().to_owned();
                let (mut pooled, addr) = crate::h3_transport::connect_h3_addrs::<R>(
                    endpoint,
                    &addrs,
                    &sni_host,
                    self.local_address,
                )
                .await?;
                pooled.remote_addr = Some(addr);
                let mut resp =
                    Self::send_on_connection(&mut pooled, request, original_uri.clone()).await?;
                resp.set_remote_addr(pooled.remote_addr);
                resp.set_tls_info(pooled.tls_info.clone());
                if resp.status() != http::StatusCode::SWITCHING_PROTOCOLS {
                    self.pool.checkin(pool_key, pooled);
                }
                return Ok(resp);
            }
        }

        let proxy = self
            .proxy
            .as_ref()
            .and_then(|settings| settings.proxy_for(original_uri));

        #[cfg(unix)]
        let unix_socket = self.unix_socket.as_ref();
        #[cfg(not(unix))]
        let unix_socket: Option<&std::path::PathBuf> = None;

        let mut timing = TimingCollector::default();

        let mut pooled = if let Some(unix_path) = unix_socket {
            let _ = &proxy; // suppress unused warning when unix_socket is set
            #[cfg(unix)]
            {
                let connect_fut = async {
                    let unix_stream = R::connect_unix(unix_path).await.map_err(Error::Io)?;
                    self.connect_plaintext(unix_stream).await
                };
                match self.connect_timeout {
                    Some(duration) => {
                        crate::timeout::Timeout::WithTimeout {
                            future: connect_fut,
                            sleep: R::sleep(duration),
                        }
                        .await?
                    }
                    None => connect_fut.await?,
                }
            }
            #[cfg(not(unix))]
            unreachable!()
        } else if let Some(ref proxy) = proxy {
            self.connect_via_proxy(proxy, authority, is_https).await?
        } else {
            let default_port = if is_https { 443 } else { 80 };
            let host = authority.host();
            let port = authority.port_u16().unwrap_or(default_port);

            let dns_start = Instant::now();
            let addrs = self.resolve_all_authority_raw(host, port).await?;
            timing.dns = Some(dns_start.elapsed());

            let tcp_keepalive = self.tcp_keepalive;
            let tcp_keepalive_interval = self.tcp_keepalive_interval;
            let tcp_keepalive_retries = self.tcp_keepalive_retries;
            let tcp_fast_open = self.tcp_fast_open;
            let local_address = self.local_address;
            #[cfg(target_os = "linux")]
            let interface = self.interface.as_deref();

            let tcp_start = Instant::now();
            let connect_fut = async {
                #[cfg(feature = "tracing")]
                tracing::trace!(addrs = ?addrs, "tcp.connect.start");

                let (tcp_stream, addr) = if addrs.len() > 1 && local_address.is_none() {
                    #[cfg(feature = "tower")]
                    let _ = original_uri;
                    crate::happy_eyeballs::connect_happy_eyeballs::<R>(&addrs, local_address)
                        .await
                        .map_err(Error::Io)?
                } else {
                    let addr = addrs[0];
                    let stream = if let Some(local_addr) = local_address {
                        R::connect_bound(addr, local_addr)
                            .await
                            .map_err(Error::Io)?
                    } else {
                        #[cfg(feature = "tower")]
                        if let Some(ref connector) = self.connector {
                            let info = crate::connector::ConnectInfo {
                                uri: original_uri.clone(),
                                addr,
                            };
                            connector.connect(info).await.map_err(Error::Io)?
                        } else {
                            R::connect(addr).await?
                        }
                        #[cfg(not(feature = "tower"))]
                        R::connect(addr).await?
                    };
                    (stream, addr)
                };

                #[cfg(target_os = "linux")]
                if let Some(iface) = interface {
                    R::bind_device(&tcp_stream, iface)?;
                }
                if let Some(time) = tcp_keepalive {
                    R::set_tcp_keepalive(
                        &tcp_stream,
                        time,
                        tcp_keepalive_interval,
                        tcp_keepalive_retries,
                    )?;
                }
                if tcp_fast_open {
                    let _ = R::set_tcp_fast_open(&tcp_stream);
                }
                #[cfg(feature = "tracing")]
                tracing::trace!(addr = %addr, "tcp.connect.done");

                let mut conn = if is_https {
                    self.connect_tls(tcp_stream, authority.host()).await?
                } else {
                    self.connect_plaintext(tcp_stream).await?
                };
                conn.remote_addr = Some(addr);
                Ok::<(PooledConnection<R>, Instant), Error>((conn, Instant::now()))
            };

            let (conn, connect_done) = match self.connect_timeout {
                Some(duration) => {
                    crate::timeout::Timeout::WithTimeout {
                        future: connect_fut,
                        sleep: R::sleep(duration),
                    }
                    .await?
                }
                None => connect_fut.await?,
            };
            let tcp_tls_elapsed = connect_done.duration_since(tcp_start);
            if is_https {
                if let Some(tls_dur) = conn.tls_handshake_duration {
                    timing.tls_handshake = Some(tls_dur);
                    timing.tcp_connect = Some(tcp_tls_elapsed.saturating_sub(tls_dur));
                } else {
                    timing.tcp_connect = Some(tcp_tls_elapsed);
                }
            } else {
                timing.tcp_connect = Some(tcp_tls_elapsed);
            }
            conn
        };

        let transfer_start = Instant::now();
        let mut resp = Self::send_on_connection(&mut pooled, request, original_uri.clone()).await?;
        let transfer = transfer_start.elapsed();
        resp.set_remote_addr(pooled.remote_addr);
        resp.set_tls_info(pooled.tls_info.clone());
        resp.set_timings(Some(
            timing.into_timings(Some(transfer), request_start.elapsed()),
        ));
        if !self.no_connection_reuse && resp.status() != http::StatusCode::SWITCHING_PROTOCOLS {
            self.pool.checkin(pool_key, pooled);
        }

        Ok(resp)
    }

    pub(super) async fn send_on_connection(
        conn: &mut PooledConnection<R>,
        request: http::Request<AioductBody>,
        url: Uri,
    ) -> Result<Response, Error> {
        #[cfg(feature = "tracing")]
        let proto = match &conn.conn {
            HttpConnection::H1(_) => "h1",
            HttpConnection::H2(_) => "h2",
            #[cfg(all(feature = "http3", feature = "rustls"))]
            HttpConnection::H3(_) => "h3",
        };
        #[cfg(feature = "tracing")]
        tracing::trace!(
            protocol = proto,
            host = url.host().unwrap_or(""),
            "http.send.start"
        );

        let result = match &mut conn.conn {
            HttpConnection::H1(sender) => {
                let resp = sender.send_request(request).await?;
                let resp = resp.map(crate::response::ResponseBody::from_incoming);
                Ok(Response::new(resp, url))
            }
            HttpConnection::H2(sender) => {
                let resp = sender.send_request(request).await?;
                let resp = resp.map(crate::response::ResponseBody::from_incoming);
                Ok(Response::new(resp, url))
            }
            #[cfg(all(feature = "http3", feature = "rustls"))]
            HttpConnection::H3(sender) => {
                crate::h3_transport::send_on_h3(sender, request, url).await
            }
        };

        #[cfg(feature = "tracing")]
        if let Ok(ref resp) = result {
            tracing::trace!(status = resp.status().as_u16(), "http.send.done");
        }

        result
    }
}
