use super::*;

#[cfg(feature = "rustls")]
#[path = "proxy_local/incoming_multipart.rs"]
mod incoming_multipart;
#[cfg(feature = "rustls")]
#[path = "../proxy/mixed_chain.rs"]
mod mixed_chain;

// ── Proxy tests via local engine (connect_local.rs coverage) ─────────

#[cfg(all(feature = "rustls", feature = "rustls-aws-lc-rs"))]
#[derive(Clone, Default)]
struct EchPreflightLocalConnector {
    attempts: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

#[cfg(all(feature = "rustls", feature = "rustls-aws-lc-rs"))]
impl aioduct::runtime::ConnectorLocal for EchPreflightLocalConnector {
    type Stream = <TcpConnector as aioduct::runtime::ConnectorLocal>::Stream;

    async fn connect(&self, _addr: std::net::SocketAddr) -> std::io::Result<Self::Stream> {
        self.attempts
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Err(std::io::Error::other("ECH preflight reached the connector"))
    }
}

#[cfg(all(feature = "rustls", feature = "rustls-aws-lc-rs"))]
fn ech_grease_connector() -> aioduct::tls::RustlsConnector {
    use rustls::crypto::hpke::Hpke as _;

    let hpke = rustls::crypto::aws_lc_rs::hpke::DH_KEM_P256_HKDF_SHA256_AES_128;
    let (placeholder_key, _) = hpke.generate_key_pair().expect("HPKE key pair");
    let ech_mode = rustls::client::EchMode::Grease(rustls::client::EchGreaseConfig::new(
        hpke,
        placeholder_key,
    ));
    let config = rustls::ClientConfig::builder_with_provider(std::sync::Arc::new(
        rustls::crypto::aws_lc_rs::default_provider(),
    ))
    .with_ech(ech_mode)
    .expect("ECH config")
    .with_root_certificates(rustls::RootCertStore::empty())
    .with_no_client_auth();
    aioduct::tls::RustlsConnector::new(std::sync::Arc::new(config))
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum LocalProxyDetail {
    Tcp(aioduct::NegotiatedProtocol),
    Tls(Option<String>),
}

#[derive(Clone, Default)]
struct LocalProxyDetailObserver(std::sync::Arc<std::sync::Mutex<Vec<LocalProxyDetail>>>);

impl aioduct::observer::RequestObserver for LocalProxyDetailObserver {
    fn on_event(&self, event: &aioduct::observer::RequestEvent) {
        let detail = match &event.phase {
            aioduct::observer::RequestPhase::TcpConnected { protocol, .. } => {
                Some(LocalProxyDetail::Tcp(*protocol))
            }
            aioduct::observer::RequestPhase::TlsHandshakeComplete { alpn_protocol, .. } => {
                Some(LocalProxyDetail::Tls(alpn_protocol.clone()))
            }
            _ => None,
        };
        if let Some(detail) = detail {
            self.0.lock().unwrap().push(detail);
        }
    }

    fn on_connection_event(&self, _event: &aioduct::observer::ConnectionEvent) {}
}

impl LocalProxyDetailObserver {
    fn details(&self) -> Vec<LocalProxyDetail> {
        self.0.lock().unwrap().clone()
    }
}

fn start_h2_server_tokio() -> std::net::SocketAddr {
    start_counting_h2_server_tokio().0
}

fn start_silent_h2c_then_h1_origin_tokio() -> std::net::SocketAddr {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async move {
            use tokio::io::AsyncReadExt as _;

            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            tx.send(listener.local_addr().unwrap()).unwrap();
            let accepted = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
            loop {
                let (mut stream, _) = listener.accept().await.unwrap();
                let connection = accepted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                tokio::spawn(async move {
                    if connection == 0 {
                        let mut preface = [0_u8; 24];
                        stream.read_exact(&mut preface).await.unwrap();
                        assert_eq!(&preface, b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n");
                        std::future::pending::<()>().await;
                    }

                    let io = aioduct::runtime::tokio_rt::TokioIo::new(stream);
                    let _ = hyper::server::conn::http1::Builder::new()
                        .serve_connection(io, hyper::service::service_fn(super::hello))
                        .await;
                });
            }
        });
    });
    rx.recv().unwrap()
}

fn start_counting_h2_server_tokio() -> (std::net::SocketAddr, aioduct_test_server::ConnectionCounter)
{
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async move {
            let (addr, counter) = aioduct_test_server::h2::h2_server().await;
            tx.send((addr, counter)).unwrap();
            std::future::pending::<()>().await;
        });
    });
    rx.recv().unwrap()
}

#[cfg(feature = "rustls")]
fn start_stalled_https_proxy_tokio() -> std::net::SocketAddr {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async move {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            tx.send(listener.local_addr().unwrap()).unwrap();
            let (_stream, _) = listener.accept().await.unwrap();
            std::future::pending::<()>().await;
        });
    });
    rx.recv().unwrap()
}

#[cfg(feature = "rustls")]
fn start_tls_h1_origin_without_alpn_tokio() -> (
    std::net::SocketAddr,
    rustls::pki_types::CertificateDer<'static>,
) {
    start_tls_h1_origin_tokio(Vec::new())
}

#[cfg(feature = "rustls")]
fn start_tls_h1_origin_tokio(
    alpn_protocols: Vec<Vec<u8>>,
) -> (
    std::net::SocketAddr,
    rustls::pki_types::CertificateDer<'static>,
) {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async move {
            let alpn = alpn_protocols.iter().map(Vec::as_slice).collect::<Vec<_>>();
            let (addr, certificate, _) = aioduct_test_server::tls::tls_h1_server(&alpn).await;
            tx.send((addr, certificate)).unwrap();
            std::future::pending::<()>().await;
        });
    });
    rx.recv().unwrap()
}

#[cfg(feature = "rustls")]
fn start_unknown_alpn_origin_probe_tokio() -> (
    std::net::SocketAddr,
    rustls::pki_types::CertificateDer<'static>,
    std::sync::mpsc::Receiver<usize>,
) {
    let (address_tx, address_rx) = std::sync::mpsc::channel();
    let (read_tx, read_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async move {
            use tokio::io::AsyncReadExt as _;

            aioduct_test_server::tls::install_crypto_provider();
            let cert = aioduct_test_server::tls::generate_self_signed(&["localhost"]);
            let cert_der = cert.cert_der.clone();
            let mut config = rustls::ServerConfig::builder_with_provider(
                aioduct_test_server::tls::crypto_provider(),
            )
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(vec![cert.cert_der], cert.key_der)
            .unwrap();
            config.alpn_protocols = vec![b"custom-proto".to_vec()];
            let acceptor = tokio_rustls::TlsAcceptor::from(std::sync::Arc::new(config));
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            address_tx
                .send((listener.local_addr().unwrap(), cert_der))
                .unwrap();
            let (stream, _) = listener.accept().await.unwrap();
            let mut stream = acceptor.accept(stream).await.unwrap();
            let mut byte = [0_u8; 1];
            let read = tokio::time::timeout(Duration::from_secs(2), stream.read(&mut byte))
                .await
                .ok()
                .and_then(Result::ok)
                .unwrap_or(0);
            read_tx.send(read).unwrap();
        });
    });
    let (address, certificate) = address_rx.recv().unwrap();
    (address, certificate, read_rx)
}

#[cfg(feature = "rustls")]
fn start_tls_h2_origin_tokio() -> (
    std::net::SocketAddr,
    rustls::pki_types::CertificateDer<'static>,
) {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async move {
            let (addr, certificate, _) = aioduct_test_server::tls::tls_h2_server().await;
            tx.send((addr, certificate)).unwrap();
            std::future::pending::<()>().await;
        });
    });
    rx.recv().unwrap()
}

#[cfg(feature = "rustls")]
fn start_tls_origin_aborting_before_http_tokio() -> (
    std::net::SocketAddr,
    rustls::pki_types::CertificateDer<'static>,
) {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async move {
            aioduct_test_server::tls::install_crypto_provider();
            let cert = aioduct_test_server::tls::generate_self_signed(&["localhost"]);
            let cert_der = cert.cert_der.clone();
            let mut config = rustls::ServerConfig::builder_with_provider(
                aioduct_test_server::tls::crypto_provider(),
            )
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(vec![cert.cert_der], cert.key_der)
            .unwrap();
            config.alpn_protocols = vec![b"http/1.1".to_vec()];
            let acceptor = tokio_rustls::TlsAcceptor::from(std::sync::Arc::new(config));
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            tx.send((listener.local_addr().unwrap(), cert_der)).unwrap();
            let (stream, _) = listener.accept().await.unwrap();
            let tls = acceptor.accept(stream).await.unwrap();
            drop(tls);
        });
    });
    rx.recv().unwrap()
}

#[cfg(feature = "rustls")]
fn start_closing_server_tokio() -> std::net::SocketAddr {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async move {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            tx.send(listener.local_addr().unwrap()).unwrap();
            let (stream, _) = listener.accept().await.unwrap();
            tokio::time::sleep(Duration::from_millis(50)).await;
            drop(stream);
        });
    });
    rx.recv().unwrap()
}

fn start_closing_proxy_endpoint_tokio() -> (
    std::net::SocketAddr,
    std::sync::Arc<std::sync::atomic::AtomicUsize>,
) {
    let (tx, rx) = std::sync::mpsc::channel();
    let connections = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let server_connections = std::sync::Arc::clone(&connections);
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async move {
            use tokio::io::AsyncReadExt;

            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            tx.send(listener.local_addr().unwrap()).unwrap();
            loop {
                let (mut stream, _) = listener.accept().await.unwrap();
                server_connections.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                tokio::spawn(async move {
                    let mut byte = [0_u8; 1];
                    let _ = stream.read(&mut byte).await;
                });
            }
        });
    });
    (rx.recv().unwrap(), connections)
}

/// Start a minimal SOCKS5 proxy server on a tokio thread. Returns the proxy's
/// listen address. The proxy connects to the target using the port from the
/// SOCKS5 CONNECT request, always connecting to 127.0.0.1.
fn start_socks5_proxy_tokio() -> std::net::SocketAddr {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            tx.send(addr).unwrap();

            loop {
                let (mut client, _) = listener.accept().await.unwrap();
                tokio::spawn(async move {
                    use tokio::io::{AsyncReadExt, AsyncWriteExt};

                    // Read SOCKS5 greeting.
                    let mut greeting = [0u8; 2];
                    if client.read_exact(&mut greeting).await.is_err() || greeting[0] != 0x05 {
                        return;
                    }
                    let mut methods = vec![0u8; greeting[1] as usize];
                    if client.read_exact(&mut methods).await.is_err() {
                        return;
                    }

                    // Reply: no auth required
                    client.write_all(&[0x05, 0x00]).await.unwrap();

                    // Read CONNECT request.
                    let mut request = [0u8; 4];
                    if client.read_exact(&mut request).await.is_err()
                        || request[..3] != [0x05, 0x01, 0x00]
                    {
                        return;
                    }

                    // Consume the target address. Locally resolved requests use
                    // their concrete IP; remote names retain the helper's
                    // localhost mapping.
                    let target_ip = match request[3] {
                        0x01 => {
                            let mut address = [0u8; 4];
                            if client.read_exact(&mut address).await.is_err() {
                                return;
                            }
                            Some(std::net::IpAddr::V4(std::net::Ipv4Addr::from(address)))
                        }
                        0x03 => {
                            let Ok(length) = client.read_u8().await else {
                                return;
                            };
                            let mut address = vec![0u8; length as usize];
                            if client.read_exact(&mut address).await.is_err() {
                                return;
                            }
                            None
                        }
                        0x04 => {
                            let mut address = [0u8; 16];
                            if client.read_exact(&mut address).await.is_err() {
                                return;
                            }
                            Some(std::net::IpAddr::V6(std::net::Ipv6Addr::from(address)))
                        }
                        _ => return,
                    };
                    let Ok(port) = client.read_u16().await else {
                        return;
                    };

                    let target = std::net::SocketAddr::new(
                        target_ip.unwrap_or(std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)),
                        port,
                    );
                    let mut upstream = match tokio::net::TcpStream::connect(target).await {
                        Ok(s) => s,
                        Err(_) => {
                            let _ = client
                                .write_all(&[0x05, 0x05, 0x00, 0x01, 0, 0, 0, 0, 0, 0])
                                .await;
                            return;
                        }
                    };

                    // Reply: success
                    client
                        .write_all(&[0x05, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00])
                        .await
                        .unwrap();

                    // Bidirectional relay
                    let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
                });
            }
        });
    });
    rx.recv().unwrap()
}

fn start_socks5_ip_literal_proxy_tokio(
    target: std::net::SocketAddr,
) -> (std::net::SocketAddr, std::sync::mpsc::Receiver<u8>) {
    let (addr_tx, addr_rx) = std::sync::mpsc::channel();
    let (atyp_tx, atyp_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(async move {
                use tokio::io::{AsyncReadExt, AsyncWriteExt};

                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                addr_tx.send(listener.local_addr().unwrap()).unwrap();
                let (mut client, _) = listener.accept().await.unwrap();
                let mut greeting = [0_u8; 3];
                client.read_exact(&mut greeting).await.unwrap();
                assert_eq!(greeting, [0x05, 0x01, 0x00]);
                client.write_all(&[0x05, 0x00]).await.unwrap();

                let mut request = [0_u8; 10];
                client.read_exact(&mut request).await.unwrap();
                assert_eq!(&request[..4], &[0x05, 0x01, 0x00, 0x01]);
                atyp_tx.send(request[3]).unwrap();

                let mut upstream = tokio::net::TcpStream::connect(target).await.unwrap();
                client
                    .write_all(&[0x05, 0x00, 0x00, 0x01, 0, 0, 0, 0, 0, 0])
                    .await
                    .unwrap();
                let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
            });
    });
    (addr_rx.recv().unwrap(), atyp_rx)
}

fn start_socks5_fallback_proxy_tokio() -> (
    std::net::SocketAddr,
    std::sync::Arc<std::sync::Mutex<Vec<std::net::SocketAddr>>>,
) {
    let captured = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let captured_server = std::sync::Arc::clone(&captured);
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};

            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            tx.send(listener.local_addr().unwrap()).unwrap();

            for attempt in 0..2 {
                let (mut client, _) = listener.accept().await.unwrap();
                let mut greeting = [0u8; 3];
                client.read_exact(&mut greeting).await.unwrap();
                client.write_all(&[0x05, 0x00]).await.unwrap();

                let mut request = [0u8; 10];
                client.read_exact(&mut request).await.unwrap();
                assert_eq!(&request[..4], &[0x05, 0x01, 0x00, 0x01]);
                let requested = std::net::SocketAddr::from((
                    [request[4], request[5], request[6], request[7]],
                    u16::from_be_bytes([request[8], request[9]]),
                ));
                captured_server.lock().unwrap().push(requested);

                if attempt == 0 {
                    client
                        .write_all(&[0x05, 0x04, 0x00, 0x01, 0, 0, 0, 0, 0, 0])
                        .await
                        .unwrap();
                    continue;
                }

                let mut upstream = tokio::net::TcpStream::connect(requested).await.unwrap();
                client
                    .write_all(&[0x05, 0x00, 0x00, 0x01, 0, 0, 0, 0, 0, 0])
                    .await
                    .unwrap();
                let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
            }
        });
    });
    (rx.recv().unwrap(), captured)
}

/// Start a minimal SOCKS5 proxy server that requires username/password auth.
fn start_socks5_auth_proxy_tokio() -> std::net::SocketAddr {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            tx.send(addr).unwrap();

            loop {
                let (mut client, _) = listener.accept().await.unwrap();
                tokio::spawn(async move {
                    use tokio::io::{AsyncReadExt, AsyncWriteExt};

                    let mut buf = [0u8; 256];
                    let n = client.read(&mut buf).await.unwrap();
                    if n < 3 || buf[0] != 0x05 {
                        return;
                    }

                    // Require username/password auth (method 0x02)
                    client.write_all(&[0x05, 0x02]).await.unwrap();

                    // Read auth sub-negotiation
                    let n = client.read(&mut buf).await.unwrap();
                    if n < 3 || buf[0] != 0x01 {
                        return;
                    }
                    let ulen = buf[1] as usize;
                    let username = String::from_utf8_lossy(&buf[2..2 + ulen]).to_string();
                    let plen = buf[2 + ulen] as usize;
                    let password =
                        String::from_utf8_lossy(&buf[3 + ulen..3 + ulen + plen]).to_string();

                    if username == "proxyuser" && password == "proxypass" {
                        client.write_all(&[0x01, 0x00]).await.unwrap(); // success
                    } else {
                        client.write_all(&[0x01, 0x01]).await.unwrap(); // failure
                        return;
                    }

                    // Read CONNECT request
                    let n = client.read(&mut buf).await.unwrap();
                    if n < 7 {
                        return;
                    }

                    let port = match buf[3] {
                        0x01 => u16::from_be_bytes([buf[8], buf[9]]),
                        0x03 => {
                            let domain_len = buf[4] as usize;
                            let port_offset = 5 + domain_len;
                            u16::from_be_bytes([buf[port_offset], buf[port_offset + 1]])
                        }
                        0x04 => u16::from_be_bytes([buf[20], buf[21]]),
                        _ => return,
                    };

                    let target = format!("127.0.0.1:{port}");
                    let mut upstream = match tokio::net::TcpStream::connect(target).await {
                        Ok(s) => s,
                        Err(_) => return,
                    };

                    client
                        .write_all(&[0x05, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00])
                        .await
                        .unwrap();

                    let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
                });
            }
        });
    });
    rx.recv().unwrap()
}

fn start_rejecting_socks5_proxy_tokio() -> (
    std::net::SocketAddr,
    std::sync::Arc<std::sync::atomic::AtomicUsize>,
) {
    let (tx, rx) = std::sync::mpsc::channel();
    let connections = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let server_connections = std::sync::Arc::clone(&connections);
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};

            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            tx.send(listener.local_addr().unwrap()).unwrap();
            loop {
                let (mut client, _) = listener.accept().await.unwrap();
                server_connections.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                tokio::spawn(async move {
                    let mut greeting = [0_u8; 2];
                    if client.read_exact(&mut greeting).await.is_err() || greeting[0] != 0x05 {
                        return;
                    }
                    let mut methods = vec![0_u8; greeting[1] as usize];
                    if client.read_exact(&mut methods).await.is_err() {
                        return;
                    }
                    let _ = client.write_all(&[0x05, 0xff]).await;
                });
            }
        });
    });
    (rx.recv().unwrap(), connections)
}

/// Start a minimal SOCKS4a proxy server on a tokio thread.
fn start_socks4_proxy_tokio() -> std::net::SocketAddr {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            tx.send(addr).unwrap();

            loop {
                let (mut client, _) = listener.accept().await.unwrap();
                tokio::spawn(async move {
                    use tokio::io::{AsyncReadExt, AsyncWriteExt};

                    // SOCKS4a request:
                    // VN(1) CD(1) DSTPORT(2) DSTIP(4) USERID(variable, null-terminated) HOSTNAME(variable, null-terminated)
                    let mut buf = [0u8; 512];
                    let n = client.read(&mut buf).await.unwrap();
                    if n < 9 || buf[0] != 0x04 || buf[1] != 0x01 {
                        return;
                    }

                    let port = ((buf[2] as u16) << 8) | (buf[3] as u16);

                    // Connect to the target on localhost
                    let target = format!("127.0.0.1:{port}");
                    let mut upstream = match tokio::net::TcpStream::connect(target).await {
                        Ok(s) => s,
                        Err(_) => {
                            // Reply: rejected
                            client
                                .write_all(&[0x00, 0x5B, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00])
                                .await
                                .ok();
                            return;
                        }
                    };

                    // Reply: request granted (VN=0, CD=0x5A, DSTPORT=0, DSTIP=0)
                    client
                        .write_all(&[0x00, 0x5A, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00])
                        .await
                        .unwrap();

                    let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
                });
            }
        });
    });
    rx.recv().unwrap()
}

fn start_forced_socks4_proxy_tokio() -> (
    std::net::SocketAddr,
    std::sync::mpsc::Receiver<std::net::SocketAddr>,
) {
    let (address_tx, address_rx) = std::sync::mpsc::channel();
    let (target_tx, target_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};

            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            address_tx.send(listener.local_addr().unwrap()).unwrap();
            let (mut downstream, _) = listener.accept().await.unwrap();
            let mut request = [0_u8; 8];
            downstream.read_exact(&mut request).await.unwrap();
            assert_eq!(&request[..2], &[0x04, 0x01]);
            let requested = std::net::SocketAddr::from((
                [request[4], request[5], request[6], request[7]],
                u16::from_be_bytes([request[2], request[3]]),
            ));
            loop {
                if downstream.read_u8().await.unwrap() == 0 {
                    break;
                }
            }
            target_tx.send(requested).unwrap();

            let mut upstream = tokio::net::TcpStream::connect(requested).await.unwrap();
            downstream
                .write_all(&[0x00, 0x5a, 0, 0, 0, 0, 0, 0])
                .await
                .unwrap();
            let _ = tokio::io::copy_bidirectional(&mut downstream, &mut upstream).await;
        });
    });
    (address_rx.recv().unwrap(), target_rx)
}

/// Start an HTTP CONNECT tunnel proxy on a tokio thread.
/// For HTTPS requests, the client sends CONNECT; for plain HTTP, the proxy
/// just forwards the request.
fn start_http_proxy_tokio() -> std::net::SocketAddr {
    start_http_proxy_tokio_with_version_and_count("HTTP/1.1").0
}

fn start_http10_proxy_tokio() -> std::net::SocketAddr {
    start_http_proxy_tokio_with_version_and_count("HTTP/1.0").0
}

fn start_counting_http_proxy_tokio() -> (
    std::net::SocketAddr,
    std::sync::Arc<std::sync::atomic::AtomicUsize>,
) {
    start_http_proxy_tokio_with_version_and_count("HTTP/1.1")
}

fn start_http_proxy_tokio_with_version_and_count(
    response_version: &'static str,
) -> (
    std::net::SocketAddr,
    std::sync::Arc<std::sync::atomic::AtomicUsize>,
) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpStream;

    let (tx, rx) = std::sync::mpsc::channel();
    let connections = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let server_connections = connections.clone();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            tx.send(addr).unwrap();

            loop {
                let (mut client, _) = match listener.accept().await {
                    Ok(c) => c,
                    Err(_) => return,
                };
                server_connections.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 8192];
                    let n = match client.read(&mut buf).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => n,
                    };
                    let head = String::from_utf8_lossy(&buf[..n]);
                    if !head.starts_with("CONNECT ") {
                        return;
                    }
                    let target = head.split_whitespace().nth(1).unwrap_or("");
                    client
                        .write_all(
                            format!("{response_version} 200 Connection Established\r\n\r\n")
                                .as_bytes(),
                        )
                        .await
                        .unwrap();
                    let mut target_stream = match TcpStream::connect(target).await {
                        Ok(s) => s,
                        Err(_) => return,
                    };
                    let _ = tokio::io::copy_bidirectional(&mut client, &mut target_stream).await;
                });
            }
        });
    });
    (rx.recv().unwrap(), connections)
}

#[cfg(feature = "rustls")]
fn start_https_proxy_tokio() -> (
    std::net::SocketAddr,
    rustls::pki_types::CertificateDer<'static>,
) {
    let (addr, certificate, _) = start_https_proxy_tokio_with_options(true, None);
    (addr, certificate)
}

#[cfg(feature = "rustls")]
fn start_https_proxy_tokio_without_alpn() -> (
    std::net::SocketAddr,
    rustls::pki_types::CertificateDer<'static>,
) {
    let (addr, certificate, _) = start_https_proxy_tokio_with_options(false, None);
    (addr, certificate)
}

#[cfg(feature = "rustls")]
fn start_https_proxy_tokio_observing_client_certificate(
    client_certificate: rustls::pki_types::CertificateDer<'static>,
) -> (
    std::net::SocketAddr,
    rustls::pki_types::CertificateDer<'static>,
    std::sync::Arc<std::sync::atomic::AtomicBool>,
) {
    let seen = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    start_https_proxy_tokio_with_options(true, Some((client_certificate, seen)))
}

#[cfg(feature = "rustls")]
fn start_https_proxy_tokio_with_options(
    advertise_http1_alpn: bool,
    client_auth_observation: Option<(
        rustls::pki_types::CertificateDer<'static>,
        std::sync::Arc<std::sync::atomic::AtomicBool>,
    )>,
) -> (
    std::net::SocketAddr,
    rustls::pki_types::CertificateDer<'static>,
    std::sync::Arc<std::sync::atomic::AtomicBool>,
) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    aioduct_test_server::tls::install_crypto_provider();
    let cert = aioduct_test_server::tls::generate_self_signed(&["localhost"]);
    let cert_der = cert.cert_der.clone();
    let builder =
        rustls::ServerConfig::builder_with_provider(aioduct_test_server::tls::crypto_provider())
            .with_safe_default_protocol_versions()
            .expect("configured rustls provider does not support the default TLS versions");
    let client_certificate_seen = client_auth_observation
        .as_ref()
        .map(|(_, seen)| seen.clone())
        .unwrap_or_else(|| std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)));
    let builder = match client_auth_observation {
        Some((client_certificate, _)) => {
            let mut roots = rustls::RootCertStore::empty();
            roots.add(client_certificate).unwrap();
            let verifier = rustls::server::WebPkiClientVerifier::builder_with_provider(
                std::sync::Arc::new(roots),
                aioduct_test_server::tls::crypto_provider(),
            )
            .allow_unauthenticated()
            .build()
            .unwrap();
            builder.with_client_cert_verifier(verifier)
        }
        None => builder.with_no_client_auth(),
    };
    let mut server_config = builder
        .with_single_cert(vec![cert.cert_der], cert.key_der)
        .unwrap();
    if advertise_http1_alpn {
        server_config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    }
    let acceptor = tokio_rustls::TlsAcceptor::from(std::sync::Arc::new(server_config));
    let server_client_certificate_seen = client_certificate_seen.clone();

    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async move {
            let listener = tokio::net::TcpListener::bind("localhost:0").await.unwrap();
            tx.send(listener.local_addr().unwrap()).unwrap();

            loop {
                let (tcp, _) = listener.accept().await.unwrap();
                let acceptor = acceptor.clone();
                let client_certificate_seen = server_client_certificate_seen.clone();
                tokio::spawn(async move {
                    let Ok(mut client) = acceptor.accept(tcp).await else {
                        return;
                    };
                    if client
                        .get_ref()
                        .1
                        .peer_certificates()
                        .is_some_and(|certificates| !certificates.is_empty())
                    {
                        client_certificate_seen.store(true, std::sync::atomic::Ordering::SeqCst);
                    }
                    if advertise_http1_alpn
                        && client.get_ref().1.alpn_protocol() != Some(b"http/1.1")
                    {
                        return;
                    }

                    let mut request = Vec::new();
                    let mut chunk = [0u8; 512];
                    while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                        let Ok(n) = client.read(&mut chunk).await else {
                            return;
                        };
                        if n == 0 || request.len() + n > 8192 {
                            return;
                        }
                        request.extend_from_slice(&chunk[..n]);
                    }
                    let head = String::from_utf8_lossy(&request);
                    if !head.starts_with("CONNECT ") {
                        return;
                    }
                    let Some(target) = head.split_whitespace().nth(1) else {
                        return;
                    };
                    let Ok(mut upstream) = tokio::net::TcpStream::connect(target).await else {
                        return;
                    };
                    if client
                        .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                        .await
                        .is_err()
                    {
                        return;
                    }
                    let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
                });
            }
        });
    });

    (rx.recv().unwrap(), cert_der, client_certificate_seen)
}

#[test]
fn test_compio_socks5_proxy_local() {
    let target_addr = start_server_tokio();
    let socks5_addr = start_socks5_proxy_tokio();

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .proxy(aioduct::proxy::ProxyConfig::socks5(&format!("socks5://{socks5_addr}")).unwrap())
            .timeout(Duration::from_secs(2))
            .build_local()
            .unwrap();

        let result = client
            .get_local(&format!("http://127.0.0.1:{}/", target_addr.port()))
            .unwrap()
            .send()
            .await;

        assert!(
            result.is_ok(),
            "SOCKS5 proxy via compio local engine failed: {:?}",
            result.unwrap_err()
        );
    });
}

#[test]
fn test_compio_overlong_socks5h_targets_fail_before_one_or_two_hop_proxy_io_local() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let first_addr = listener.local_addr().unwrap();
    let long_host = format!("{}.test", "a".repeat(256));

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let one_hop = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .proxy(
                aioduct::proxy::ProxyConfig::socks5h(&format!("socks5h://{first_addr}")).unwrap(),
            )
            .build_local()
            .unwrap();
        let error = one_hop
            .get_local(&format!("http://{long_host}/one-hop"))
            .unwrap()
            .send()
            .await
            .unwrap_err();
        assert!(error.to_string().contains("255 bytes"), "{error}");

        let two_hop = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .proxy_chain(aioduct::proxy::ProxyChain::new(vec![
                aioduct::proxy::ProxyConfig::socks5h(&format!("socks5h://{first_addr}")).unwrap(),
                aioduct::proxy::ProxyConfig::http(&format!("http://{long_host}:8080")).unwrap(),
            ]))
            .build_local()
            .unwrap();
        let error = two_hop
            .get_local("http://origin.test/two-hop")
            .unwrap()
            .send()
            .await
            .unwrap_err();
        assert!(error.to_string().contains("255 bytes"), "{error}");
    });

    assert!(
        matches!(listener.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock),
        "unencodable SOCKS5h target reached the first proxy"
    );
}

#[test]
fn test_compio_socks5h_ip_literal_uses_ip_address_type_local() {
    let target_addr = start_server_tokio();
    let (socks5_addr, atyp_rx) = start_socks5_ip_literal_proxy_tokio(target_addr);

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .proxy(
                aioduct::proxy::ProxyConfig::socks5h(&format!("socks5h://{socks5_addr}")).unwrap(),
            )
            .timeout(Duration::from_secs(2))
            .build_local()
            .unwrap();

        let response = client
            .get_local(&format!("http://127.0.0.1:{}/", target_addr.port()))
            .unwrap()
            .send()
            .await
            .unwrap();

        assert_eq!(response.text().await.unwrap(), "hello aioduct");
    });

    assert_eq!(
        atyp_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("SOCKS5 CONNECT address type"),
        0x01
    );
}

#[test]
fn test_compio_socks5_target_address_fallback_local() {
    let target_addr = start_server_tokio();
    let request_port = if target_addr.port() == u16::MAX {
        u16::MAX - 1
    } else {
        target_addr.port() + 1
    };
    let unavailable = std::net::SocketAddr::from(([127, 0, 0, 2], 9));
    let (socks5_addr, captured) = start_socks5_fallback_proxy_tokio();

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .proxy(aioduct::proxy::ProxyConfig::socks5(&format!("socks5://{socks5_addr}")).unwrap())
            .resolve_to_addrs("compio-socks-target.test", &[unavailable, target_addr])
            .build_local()
            .unwrap();

        let response = client
            .get_local(&format!(
                "http://compio-socks-target.test:{}/fallback",
                request_port
            ))
            .unwrap()
            .send()
            .await
            .unwrap();

        assert_eq!(response.text().await.unwrap(), "hello aioduct");
        assert_eq!(
            captured.lock().unwrap().as_slice(),
            &[unavailable, target_addr]
        );
    });
}

#[test]
fn compio_socks5_proxy_endpoint_falls_back_after_negotiation_transport_failure() {
    let target_addr = start_server_tokio();
    let (closing_addr, closing_connections) = start_closing_proxy_endpoint_tokio();
    let proxy_addr = start_socks5_proxy_tokio();

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .proxy(aioduct::ProxyConfig::socks5("socks5://proxy.test:1080").unwrap())
            .resolve_to_addrs("proxy.test", &[closing_addr, proxy_addr])
            .timeout(Duration::from_secs(5))
            .build_local()
            .unwrap();

        let response = client
            .get_local(&format!("http://{target_addr}/post-tcp-socks-fallback"))
            .unwrap()
            .send()
            .await
            .unwrap();

        assert_eq!(response.text().await.unwrap(), "hello aioduct");
    });

    assert_eq!(
        closing_connections.load(std::sync::atomic::Ordering::SeqCst),
        1
    );
}

#[cfg(feature = "rustls")]
#[test]
fn test_compio_socks5_origin_tls_io_fallback_local() {
    let closing_addr = start_closing_server_tokio();
    let (healthy_addr, healthy_cert) = start_tls_h1_origin_without_alpn_tokio();
    let socks_addr = start_socks5_proxy_tokio();

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let connector = aioduct::tls::RustlsConnector::new(
            aioduct_test_server::tls::make_client_config(&healthy_cert),
        );
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .tls(connector)
            .proxy(aioduct::ProxyConfig::socks5(&format!("socks5://{socks_addr}")).unwrap())
            .resolve_to_addrs("localhost", &[closing_addr, healthy_addr])
            .no_connection_reuse()
            .timeout(Duration::from_secs(5))
            .build_local()
            .unwrap();

        let response = client
            .get_local("https://localhost/post-tunnel-io-fallback")
            .unwrap()
            .send()
            .await
            .unwrap();

        assert_eq!(response.version(), http::Version::HTTP_11);
        assert_eq!(response.text().await.unwrap(), "hello tls");
    });
}

#[cfg(feature = "rustls")]
#[test]
fn test_compio_socks5_second_proxy_tls_io_fallback_local() {
    let origin_addr = start_server_tokio();
    let closing_addr = start_closing_server_tokio();
    let (second_proxy_addr, second_proxy_cert) = start_https_proxy_tokio();
    let first_proxy_addr = start_socks5_proxy_tokio();

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let connector = aioduct::tls::RustlsConnector::new(
            aioduct_test_server::tls::make_client_config(&second_proxy_cert),
        );
        let chain = aioduct::ProxyChain::new(vec![
            aioduct::ProxyConfig::socks5(&format!("socks5://{first_proxy_addr}")).unwrap(),
            aioduct::ProxyConfig::https(&format!("https://localhost:{}", second_proxy_addr.port()))
                .unwrap(),
        ]);
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .tls(connector)
            .proxy_chain(chain)
            .resolve_to_addrs("localhost", &[closing_addr, second_proxy_addr])
            .no_connection_reuse()
            .timeout(Duration::from_secs(5))
            .build_local()
            .unwrap();

        let response = client
            .get_local(&format!("http://{origin_addr}/second-proxy-tls-fallback"))
            .unwrap()
            .send()
            .await
            .unwrap();

        assert_eq!(response.version(), http::Version::HTTP_11);
        assert_eq!(response.text().await.unwrap(), "hello aioduct");
    });
}

#[test]
fn test_compio_proxy_tcp_observer_fires_before_socks_auth_failure_local() {
    let socks5_addr = start_socks5_auth_proxy_tokio();

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let observer = LocalProxyDetailObserver::default();
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .proxy(
                aioduct::proxy::ProxyConfig::socks5(&format!("socks5://{socks5_addr}"))
                    .unwrap()
                    .basic_auth("proxyuser", "wrong"),
            )
            .request_observer(observer.clone())
            .build_local()
            .unwrap();

        client
            .forward_local(super::valid_forward_request(
                hyper::Request::builder()
                    .uri("/auth-failure")
                    .header(http::header::HOST, "downstream.test")
                    .body(Full::new(Bytes::new()))
                    .unwrap(),
            ))
            .upstream("http://127.0.0.1:80".parse::<http::Uri>().unwrap())
            .adaptive_h2c()
            .send()
            .await
            .unwrap_err();

        assert_eq!(
            observer.details(),
            [LocalProxyDetail::Tcp(aioduct::NegotiatedProtocol::Http2)],
            "SOCKS TCP observation should precede auth failure and retain the origin protocol"
        );
    });
}

#[test]
fn compio_socks5_auth_negotiation_failure_survives_configured_retry() {
    let (socks5_addr, proxy_connections) = start_rejecting_socks5_proxy_tokio();
    let first = std::net::SocketAddr::from(([127, 0, 0, 2], 8080));
    let second = std::net::SocketAddr::from(([127, 0, 0, 1], 8080));

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .proxy(aioduct::ProxyConfig::socks5(&format!("socks5://{socks5_addr}")).unwrap())
            .resolve_to_addrs("auth-target.test", &[first, second])
            .retry(
                aioduct::RetryConfig::default()
                    .max_retries(2)
                    .initial_backoff(Duration::from_millis(10))
                    .max_backoff(Duration::from_millis(10)),
            )
            .build_local()
            .unwrap();

        let error = client
            .forward_local(super::valid_forward_request(
                hyper::Request::builder()
                    .uri("/auth-failure")
                    .header(http::header::HOST, "downstream.test")
                    .body(Full::new(Bytes::new()))
                    .unwrap(),
            ))
            .upstream("http://auth-target.test:8080".parse::<http::Uri>().unwrap())
            .adaptive_h2c()
            .send()
            .await
            .unwrap_err();

        assert!(error.to_string().contains("no acceptable authentication"));
    });

    assert_eq!(
        proxy_connections.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "fatal SOCKS authentication failures must bypass configured retry"
    );
}

#[test]
fn test_compio_proxy_selector_is_snapshotted_once_per_dispatch() {
    let target_addr = start_server_tokio();
    let proxy_addr = start_http_proxy_tokio();
    let selector_calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let selector_calls2 = selector_calls.clone();
    let proxy = aioduct::ProxyConfig::http(&format!("http://{proxy_addr}")).unwrap();
    let settings = aioduct::ProxySettings::default().custom(move |_uri| {
        selector_calls2.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Some(proxy.clone())
    });

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .proxy_settings(settings)
            .timeout(Duration::from_secs(2))
            .build_local()
            .unwrap();

        let response = client
            .get_local(&format!("http://{target_addr}/snapshot"))
            .unwrap()
            .send()
            .await
            .unwrap();
        assert_eq!(response.text().await.unwrap(), "hello aioduct");
    });

    assert_eq!(selector_calls.load(std::sync::atomic::Ordering::SeqCst), 1);
}

#[test]
fn test_compio_socks5_proxy_with_auth_local() {
    let target_addr = start_server_tokio();
    let socks5_addr = start_socks5_auth_proxy_tokio();

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .proxy(
                aioduct::proxy::ProxyConfig::socks5(&format!("socks5://{socks5_addr}"))
                    .unwrap()
                    .basic_auth("proxyuser", "proxypass"),
            )
            .timeout(Duration::from_secs(2))
            .build_local()
            .unwrap();

        let result = client
            .get_local(&format!("http://127.0.0.1:{}/", target_addr.port()))
            .unwrap()
            .send()
            .await;

        assert!(
            result.is_ok(),
            "SOCKS5 proxy with auth via compio local engine failed: {:?}",
            result.unwrap_err()
        );
    });
}

#[test]
fn test_compio_socks4_proxy_local() {
    let target_addr = start_server_tokio();
    let socks4_addr = start_socks4_proxy_tokio();

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .proxy(aioduct::proxy::ProxyConfig::socks4(&format!("socks4://{socks4_addr}")).unwrap())
            .timeout(Duration::from_secs(2))
            .build_local()
            .unwrap();

        let result = client
            .get_local(&format!("http://127.0.0.1:{}/", target_addr.port()))
            .unwrap()
            .send()
            .await;

        assert!(
            result.is_ok(),
            "SOCKS4 proxy via compio local engine failed: {:?}",
            result.unwrap_err()
        );
    });
}

#[test]
fn compio_ipv4_force_addr_is_the_effective_socks4_destination_for_an_ipv6_origin() {
    let target_addr = start_server_tokio();

    for scheme in ["socks4", "socks4a"] {
        let (proxy_addr, captured_target) = start_forced_socks4_proxy_tokio();
        compio_runtime::Runtime::new().unwrap().block_on(async {
            let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
                .proxy(
                    aioduct::proxy::ProxyConfig::socks4(&format!("{scheme}://{proxy_addr}"))
                        .unwrap(),
                )
                .timeout(Duration::from_secs(2))
                .build_local()
                .unwrap();
            let logical_url = "http://[2001:db8::1]:8080/forced";

            let no_override = client
                .get_local(logical_url)
                .unwrap()
                .send()
                .await
                .unwrap_err();
            assert!(no_override.to_string().contains("IPv6"), "{no_override}");

            let ipv6_override = client
                .get_local(logical_url)
                .unwrap()
                .force_addr("[::1]:8080".parse().unwrap())
                .send()
                .await
                .unwrap_err();
            assert!(
                ipv6_override.to_string().contains("force_addr"),
                "{ipv6_override}"
            );

            let response = client
                .get_local(logical_url)
                .unwrap()
                .force_addr(target_addr)
                .send()
                .await
                .unwrap();
            assert_eq!(response.text().await.unwrap(), "hello aioduct");
        });

        assert_eq!(
            captured_target
                .recv_timeout(std::time::Duration::from_secs(1))
                .unwrap(),
            target_addr
        );
    }
}

#[test]
fn test_compio_http_proxy_local() {
    let target_addr = start_server_tokio();
    let http_proxy_addr = start_http_proxy_tokio();

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .proxy(aioduct::proxy::ProxyConfig::http(&format!("http://{http_proxy_addr}")).unwrap())
            .build_local()
            .unwrap();

        let resp = client
            .get_local(&format!("http://{target_addr}/test-path"))
            .unwrap()
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), http::StatusCode::OK);
        let body = resp.text().await.unwrap();
        assert!(
            body.contains("hello aioduct"),
            "expected target response, got: {body}"
        );
    });
}

#[test]
fn test_compio_h2_origin_reports_http1_proxy_tcp_protocol_local() {
    let target_addr = start_h2_server_tokio();
    let proxy_addr = start_http_proxy_tokio();

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let observer = LocalProxyDetailObserver::default();
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .proxy(aioduct::ProxyConfig::http(&format!("http://{proxy_addr}")).unwrap())
            .request_observer(observer.clone())
            .build_local()
            .unwrap();

        let response = client
            .get_local(&format!("http://{target_addr}/h2-through-proxy"))
            .unwrap()
            .h2c_prior_knowledge()
            .send()
            .await
            .unwrap();

        assert_eq!(response.version(), http::Version::HTTP_2);
        assert_eq!(response.text().await.unwrap(), "hello aioduct");
        assert!(
            observer
                .details()
                .contains(&LocalProxyDetail::Tcp(aioduct::NegotiatedProtocol::Http1))
        );
    });
}

#[test]
fn compio_adaptive_h2c_probes_h2_through_proxy_and_caches_route() {
    let target_addr = start_h2_server_tokio();
    let (proxy_addr, proxy_connections) = start_counting_http_proxy_tokio();

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .proxy(aioduct::ProxyConfig::http(&format!("http://{proxy_addr}")).unwrap())
            .build_local()
            .unwrap();
        let upstream = format!("http://{target_addr}")
            .parse::<http::Uri>()
            .unwrap();

        for path in ["/first", "/cached"] {
            let request = hyper::Request::builder()
                .uri(path)
                .header(http::header::HOST, "downstream.test")
                .body(Full::new(Bytes::new()))
                .unwrap();
            let response = client
                .forward_local(super::valid_forward_request(request))
                .upstream(upstream.clone())
                .adaptive_h2c()
                .send()
                .await
                .unwrap();
            assert_eq!(response.text().await.unwrap(), "hello aioduct");
        }
    });

    assert_eq!(
        proxy_connections.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "the cached Local H2 tunnel should be reused"
    );
}

#[test]
fn compio_adaptive_h2c_reconnects_same_proxy_route_for_h1_fallback() {
    let target_addr = start_server_tokio();
    let (proxy_addr, proxy_connections) = start_counting_http_proxy_tokio();

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .proxy(aioduct::ProxyConfig::http(&format!("http://{proxy_addr}")).unwrap())
            .build_local()
            .unwrap();
        let upstream = format!("http://{target_addr}")
            .parse::<http::Uri>()
            .unwrap();

        for path in ["/fallback", "/cached"] {
            let request = hyper::Request::builder()
                .uri(path)
                .header(http::header::HOST, "downstream.test")
                .body(Full::new(Bytes::new()))
                .unwrap();
            let response = client
                .forward_local(super::valid_forward_request(request))
                .upstream(upstream.clone())
                .adaptive_h2c()
                .send()
                .await
                .unwrap();
            assert_eq!(response.text().await.unwrap(), "hello aioduct");
        }
    });

    assert_eq!(
        proxy_connections.load(std::sync::atomic::Ordering::SeqCst),
        2,
        "one Local H2 probe tunnel and one cached H1 fallback tunnel are expected"
    );
}

#[test]
fn compio_adaptive_h2c_times_out_silent_settings_through_proxy() {
    let target_addr = start_silent_h2c_then_h1_origin_tokio();
    let (proxy_addr, proxy_connections) = start_counting_http_proxy_tokio();
    let (result_tx, result_rx) = std::sync::mpsc::channel();

    std::thread::spawn(move || {
        let result = compio_runtime::Runtime::new().unwrap().block_on(async {
            let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
                .proxy(aioduct::ProxyConfig::http(&format!("http://{proxy_addr}")).unwrap())
                .build_local()
                .unwrap();
            let request = hyper::Request::builder()
                .uri("/silent-settings")
                .header(http::header::HOST, "downstream.test")
                .body(Full::new(Bytes::new()))
                .unwrap();
            let response = client
                .forward_local(super::valid_forward_request(request))
                .upstream(format!("http://{target_addr}"))
                .adaptive_h2c()
                .send()
                .await
                .unwrap();
            response.text().await.unwrap()
        });
        result_tx.send(result).unwrap();
    });

    assert_eq!(
        result_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("silent Local HTTP/2 SETTINGS probe did not fall back"),
        "hello aioduct"
    );
    assert_eq!(
        proxy_connections.load(std::sync::atomic::Ordering::SeqCst),
        2,
        "the timed-out Local H2 probe must reconnect through the same proxy for H1"
    );
}

#[cfg(feature = "rustls")]
#[test]
fn compio_unknown_origin_alpn_is_rejected_inside_connect_tunnel_before_http_bytes() {
    let (target_addr, target_cert, read_rx) = start_unknown_alpn_origin_probe_tokio();
    let proxy_addr = start_http_proxy_tokio();
    let mut client_config = aioduct_test_server::tls::make_client_config(&target_cert);
    std::sync::Arc::get_mut(&mut client_config)
        .unwrap()
        .alpn_protocols = vec![b"custom-proto".to_vec()];

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let connector = aioduct::tls::RustlsConnector::new(client_config);
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .tls(connector)
            .proxy(aioduct::ProxyConfig::http(&format!("http://{proxy_addr}")).unwrap())
            .timeout(Duration::from_secs(5))
            .build_local()
            .unwrap();
        let error = client
            .get_local(&format!(
                "https://localhost:{}/unknown-alpn",
                target_addr.port()
            ))
            .unwrap()
            .send()
            .await
            .unwrap_err();
        assert!(
            error.to_string().contains("custom-proto"),
            "unexpected tunneled unknown-ALPN error: {error}"
        );
    });

    assert_eq!(
        read_rx.recv_timeout(Duration::from_secs(3)).unwrap(),
        0,
        "local dispatch sent HTTP bytes through CONNECT after unknown ALPN"
    );
}

#[cfg(feature = "rustls")]
#[test]
fn compio_adaptive_h2c_uses_normal_alpn_for_proxied_https_h1_origin() {
    let (target_addr, target_cert) = start_tls_h1_origin_tokio(vec![b"http/1.1".to_vec()]);
    let (proxy_addr, proxy_connections) = start_counting_http_proxy_tokio();

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let connector = aioduct::tls::RustlsConnector::new(
            aioduct_test_server::tls::make_client_config(&target_cert),
        );
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .tls(connector)
            .proxy(aioduct::ProxyConfig::http(&format!("http://{proxy_addr}")).unwrap())
            .build_local()
            .unwrap();
        let response = client
            .forward_local(super::valid_forward_request(
                hyper::Request::builder()
                    .uri("/proxied-https-h1")
                    .header(http::header::HOST, "downstream.test")
                    .body(Full::new(Bytes::new()))
                    .unwrap(),
            ))
            .upstream(
                format!("https://localhost:{}", target_addr.port())
                    .parse::<http::Uri>()
                    .unwrap(),
            )
            .adaptive_h2c()
            .send()
            .await
            .unwrap();

        assert_eq!(response.version(), http::Version::HTTP_11);
        assert_eq!(response.text().await.unwrap(), "hello tls");
    });

    assert_eq!(
        proxy_connections.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "proxied HTTPS must use one ALPN-negotiated tunnel, not an h2c probe and fallback"
    );
}

#[cfg(feature = "rustls")]
#[test]
fn compio_adaptive_h2c_uses_normal_alpn_for_proxied_https_h2_origin() {
    let (target_addr, target_cert) = start_tls_h2_origin_tokio();
    let (proxy_addr, proxy_connections) = start_counting_http_proxy_tokio();

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let connector = aioduct::tls::RustlsConnector::new(
            aioduct_test_server::tls::make_client_config(&target_cert),
        );
        let observer = LocalProxyDetailObserver::default();
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .tls(connector)
            .proxy(aioduct::ProxyConfig::http(&format!("http://{proxy_addr}")).unwrap())
            .request_observer(observer.clone())
            .build_local()
            .unwrap();
        let response = client
            .forward_local(super::valid_forward_request(
                hyper::Request::builder()
                    .uri("/proxied-https-h2")
                    .header(http::header::HOST, "downstream.test")
                    .body(Full::new(Bytes::new()))
                    .unwrap(),
            ))
            .upstream(
                format!("https://localhost:{}", target_addr.port())
                    .parse::<http::Uri>()
                    .unwrap(),
            )
            .adaptive_h2c()
            .send()
            .await
            .unwrap();

        assert_eq!(response.text().await.unwrap(), "hello tls");
        assert_eq!(
            observer.details(),
            [
                LocalProxyDetail::Tcp(aioduct::NegotiatedProtocol::Http1),
                LocalProxyDetail::Tls(Some("h2".to_owned())),
            ]
        );
    });

    assert_eq!(
        proxy_connections.load(std::sync::atomic::Ordering::SeqCst),
        1
    );
}

#[test]
fn compio_adaptive_h2c_cache_distinguishes_locally_resolved_socks5_targets() {
    let h1_addr = start_server_tokio();
    let (h2_addr, h2_counter) = start_counting_h2_server_tokio();
    let socks_addr = start_socks5_proxy_tokio();
    let resolution_count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let resolver_count = std::sync::Arc::clone(&resolution_count);

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .proxy(aioduct::ProxyConfig::socks5(&format!("socks5://{socks_addr}")).unwrap())
            .resolver(move |host: &str, _port: u16| {
                assert_eq!(host, "adaptive-socks-target.test");
                let addr = match resolver_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst) {
                    0 => h1_addr,
                    _ => h2_addr,
                };
                Box::pin(async move { Ok(addr) })
                    as std::pin::Pin<
                        Box<
                            dyn std::future::Future<Output = std::io::Result<std::net::SocketAddr>>
                                + Send,
                        >,
                    >
            })
            .no_connection_reuse()
            .timeout(Duration::from_secs(5))
            .build_local()
            .unwrap();
        let upstream = "http://adaptive-socks-target.test"
            .parse::<http::Uri>()
            .unwrap();

        let first = client
            .forward_local(super::valid_forward_request(
                hyper::Request::builder()
                    .uri("/h1")
                    .header(http::header::HOST, "downstream.test")
                    .body(Full::new(Bytes::new()))
                    .unwrap(),
            ))
            .upstream(upstream.clone())
            .adaptive_h2c()
            .send()
            .await
            .unwrap();
        assert_eq!(first.version(), http::Version::HTTP_11);
        assert_eq!(first.text().await.unwrap(), "hello aioduct");

        let second = client
            .forward_local(super::valid_forward_request(
                hyper::Request::builder()
                    .uri("/h2")
                    .header(http::header::HOST, "downstream.test")
                    .body(Full::new(Bytes::new()))
                    .unwrap(),
            ))
            .upstream(upstream)
            .adaptive_h2c()
            .send()
            .await
            .unwrap();
        assert_eq!(second.version(), http::Version::HTTP_11);
        assert_eq!(second.text().await.unwrap(), "hello aioduct");
    });

    assert_eq!(
        resolution_count.load(std::sync::atomic::Ordering::SeqCst),
        2
    );
    assert_eq!(h2_counter.requests(), 1);
}

#[test]
fn compio_force_addr_overrides_http_proxy_tunnel_destination() {
    let target_addr = start_server_tokio();
    let proxy_addr = start_http_proxy_tokio();

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .proxy(aioduct::ProxyConfig::http(&format!("http://{proxy_addr}")).unwrap())
            .build_local()
            .unwrap();
        let response = client
            .get_local("http://unresolvable-force-addr.invalid:1/forced")
            .unwrap()
            .force_addr(target_addr)
            .send()
            .await
            .unwrap();

        assert_eq!(response.text().await.unwrap(), "hello aioduct");
    });
}

#[test]
fn test_compio_proxy_endpoint_falls_back_to_second_resolved_address() {
    let target_addr = start_server_tokio();
    let proxy_addr = start_http_proxy_tokio();
    let unavailable_listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let unavailable_addr = unavailable_listener.local_addr().unwrap();
    drop(unavailable_listener);

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .proxy(aioduct::ProxyConfig::http("http://proxy.test:8080").unwrap())
            .resolve_to_addrs("proxy.test", &[unavailable_addr, proxy_addr])
            .timeout(Duration::from_secs(2))
            .build_local()
            .unwrap();

        let response = client
            .get_local(&format!("http://{target_addr}/proxy-address-fallback"))
            .unwrap()
            .send()
            .await
            .unwrap();

        assert_eq!(response.status(), http::StatusCode::OK);
        assert_eq!(response.text().await.unwrap(), "hello aioduct");
    });
}

#[test]
fn compio_http_proxy_endpoint_falls_back_after_connect_transport_failure() {
    let target_addr = start_server_tokio();
    let (closing_addr, closing_connections) = start_closing_proxy_endpoint_tokio();
    let (proxy_addr, proxy_connections) = start_counting_http_proxy_tokio();

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .proxy(aioduct::ProxyConfig::http("http://proxy.test:8080").unwrap())
            .resolve_to_addrs("proxy.test", &[closing_addr, proxy_addr])
            .timeout(Duration::from_secs(5))
            .build_local()
            .unwrap();

        let response = client
            .get_local(&format!("http://{target_addr}/post-tcp-connect-fallback"))
            .unwrap()
            .send()
            .await
            .unwrap();

        assert_eq!(response.text().await.unwrap(), "hello aioduct");
    });

    assert_eq!(
        closing_connections.load(std::sync::atomic::Ordering::SeqCst),
        1
    );
    assert_eq!(
        proxy_connections.load(std::sync::atomic::Ordering::SeqCst),
        1
    );
}

#[test]
fn compio_failed_socks5_proxy_endpoint_is_not_retried_for_next_target() {
    let target_addr = start_server_tokio();
    let rejected_target = std::net::SocketAddr::from(([0, 0, 0, 0], 0));
    let (closing_addr, closing_connections) = start_closing_proxy_endpoint_tokio();
    let proxy_addr = start_socks5_proxy_tokio();

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .proxy(aioduct::ProxyConfig::socks5("socks5://proxy.test:1080").unwrap())
            .resolve_to_addrs("proxy.test", &[closing_addr, proxy_addr])
            .resolve_to_addrs("target.test", &[rejected_target, target_addr])
            .timeout(Duration::from_secs(5))
            .build_local()
            .unwrap();

        let response = client
            .get_local(&format!(
                "http://target.test:{}/persistent-proxy-fallback",
                target_addr.port()
            ))
            .unwrap()
            .send()
            .await
            .unwrap();

        assert_eq!(response.text().await.unwrap(), "hello aioduct");
    });

    assert_eq!(
        closing_connections.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "failed first-proxy endpoints must stay excluded across target fallback"
    );
}

#[cfg(feature = "rustls")]
#[test]
fn compio_https_proxy_endpoint_falls_back_after_tls_transport_failure() {
    let target_addr = start_server_tokio();
    let (closing_addr, closing_connections) = start_closing_proxy_endpoint_tokio();
    let (proxy_addr, proxy_cert) = start_https_proxy_tokio();

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let connector = aioduct::tls::RustlsConnector::new(
            aioduct_test_server::tls::make_client_config(&proxy_cert),
        );
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .tls(connector)
            .proxy(aioduct::ProxyConfig::https("https://localhost:8443").unwrap())
            .resolve_to_addrs("localhost", &[closing_addr, proxy_addr])
            .timeout(Duration::from_secs(5))
            .build_local()
            .unwrap();

        let response = client
            .get_local(&format!("http://{target_addr}/post-tcp-tls-fallback"))
            .unwrap()
            .send()
            .await
            .unwrap();

        assert_eq!(response.text().await.unwrap(), "hello aioduct");
    });

    assert_eq!(
        closing_connections.load(std::sync::atomic::Ordering::SeqCst),
        1
    );
}

#[test]
fn test_compio_custom_proxy_is_resolved_once_per_dispatch_attempt() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let target_addr = start_server_tokio();
    let proxy_addr = start_http_proxy_tokio();
    let resolutions = Arc::new(AtomicUsize::new(0));
    let observed_resolutions = resolutions.clone();

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let settings = aioduct::ProxySettings::default().custom(move |_uri| {
            observed_resolutions.fetch_add(1, Ordering::SeqCst);
            Some(aioduct::ProxyConfig::http(&format!("http://{proxy_addr}")).unwrap())
        });
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .proxy_settings(settings)
            .build_local()
            .unwrap();

        let response = client
            .get_local(&format!("http://{target_addr}/custom-proxy"))
            .unwrap()
            .send()
            .await
            .unwrap();

        assert_eq!(response.status(), http::StatusCode::OK);
        assert_eq!(response.text().await.unwrap(), "hello aioduct");
    });

    assert_eq!(resolutions.load(Ordering::SeqCst), 1);
}

#[test]
fn test_compio_http10_connect_response_local() {
    let target_addr = start_server_tokio();
    let proxy_addr = start_http10_proxy_tokio();

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .proxy(aioduct::proxy::ProxyConfig::http(&format!("http://{proxy_addr}")).unwrap())
            .timeout(Duration::from_secs(2))
            .build_local()
            .unwrap();
        let response = client
            .get_local(&format!("http://{target_addr}/http10-connect"))
            .unwrap()
            .send()
            .await
            .unwrap();

        assert_eq!(response.status(), http::StatusCode::OK);
        assert_eq!(response.text().await.unwrap(), "hello aioduct");
    });
}

#[test]
fn test_compio_two_http_proxy_hops_local() {
    let target_addr = start_server_tokio();
    let second_addr = start_http_proxy_tokio();
    let first_addr = start_http_proxy_tokio();
    let chain = aioduct::proxy::ProxyChain::new(vec![
        aioduct::proxy::ProxyConfig::http(&format!("http://{first_addr}")).unwrap(),
        aioduct::proxy::ProxyConfig::http(&format!("http://{second_addr}")).unwrap(),
    ]);

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .proxy_chain(chain)
            .timeout(Duration::from_secs(5))
            .build_local()
            .unwrap();
        let response = client
            .get_local(&format!("http://{target_addr}/two-http-proxies"))
            .unwrap()
            .send()
            .await
            .unwrap();

        assert_eq!(response.status(), http::StatusCode::OK);
        assert_eq!(response.text().await.unwrap(), "hello aioduct");
    });
}

#[cfg(feature = "rustls")]
#[test]
fn test_compio_https_proxy_negotiates_http1_local() {
    let target_addr = start_server_tokio();
    let (proxy_addr, proxy_cert) = start_https_proxy_tokio();

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let connector = aioduct::tls::RustlsConnector::new(
            aioduct_test_server::tls::make_client_config(&proxy_cert),
        );
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .tls(connector)
            .proxy(
                aioduct::proxy::ProxyConfig::https(&format!(
                    "https://localhost:{}",
                    proxy_addr.port()
                ))
                .unwrap(),
            )
            .timeout(Duration::from_secs(5))
            .build_local()
            .unwrap();

        let response = client
            .get_local(&format!("http://{target_addr}/https-proxy"))
            .unwrap()
            .send()
            .await
            .unwrap();

        assert_eq!(response.status(), http::StatusCode::OK);
        assert_eq!(response.text().await.unwrap(), "hello aioduct");
    });
}

#[cfg(feature = "rustls")]
#[test]
fn compio_https_proxy_uses_configured_client_identity_local() {
    aioduct_test_server::tls::install_crypto_provider();
    let target_addr = start_server_tokio();
    let client_certificate =
        rcgen::generate_simple_self_signed(vec!["proxy-client.test".into()]).unwrap();
    let client_certificate_der =
        rustls::pki_types::CertificateDer::from(client_certificate.cert.der().to_vec());
    let mut identity_pem = client_certificate.cert.pem();
    identity_pem.push_str(&client_certificate.signing_key.serialize_pem());
    let identity = aioduct::tls::Identity::from_pem(identity_pem.as_bytes()).unwrap();
    let (proxy_addr, proxy_certificate, client_certificate_seen) =
        start_https_proxy_tokio_observing_client_certificate(client_certificate_der);

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .add_root_certificates(&[aioduct::tls::Certificate::from_der(
                proxy_certificate.to_vec(),
            )])
            .identity(identity)
            .proxy(
                aioduct::ProxyConfig::https(&format!("https://localhost:{}", proxy_addr.port()))
                    .unwrap(),
            )
            .timeout(Duration::from_secs(5))
            .build_local()
            .unwrap();

        let response = client
            .get_local(&format!("http://{target_addr}/proxy-client-identity"))
            .unwrap()
            .send()
            .await
            .unwrap();
        assert_eq!(response.text().await.unwrap(), "hello aioduct");
    });

    assert!(
        client_certificate_seen.load(std::sync::atomic::Ordering::SeqCst),
        "configured client identity was not presented to the HTTPS proxy"
    );
}

#[cfg(feature = "rustls")]
#[test]
fn test_compio_https_proxy_without_alpn_defaults_to_http1_local() {
    let target_addr = start_server_tokio();
    let (proxy_addr, proxy_cert) = start_https_proxy_tokio_without_alpn();

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let connector = aioduct::tls::RustlsConnector::new(
            aioduct_test_server::tls::make_client_config(&proxy_cert),
        );
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .tls(connector)
            .proxy(
                aioduct::proxy::ProxyConfig::https(&format!(
                    "https://localhost:{}",
                    proxy_addr.port()
                ))
                .unwrap(),
            )
            .timeout(Duration::from_secs(5))
            .build_local()
            .unwrap();

        let response = client
            .get_local(&format!("http://{target_addr}/https-proxy-no-alpn"))
            .unwrap()
            .send()
            .await
            .unwrap();

        assert_eq!(response.status(), http::StatusCode::OK);
        assert_eq!(response.text().await.unwrap(), "hello aioduct");
    });
}

#[cfg(feature = "rustls")]
#[test]
fn test_compio_proxied_origin_tls_observer_preserves_missing_alpn_local() {
    let (target_addr, target_cert) = start_tls_h1_origin_without_alpn_tokio();
    let proxy_addr = start_http_proxy_tokio();

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let connector = aioduct::tls::RustlsConnector::new(
            aioduct_test_server::tls::make_client_config(&target_cert),
        );
        let observer = LocalProxyDetailObserver::default();
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .tls(connector)
            .proxy(aioduct::ProxyConfig::http(&format!("http://{proxy_addr}")).unwrap())
            .request_observer(observer.clone())
            .timeout(Duration::from_secs(5))
            .build_local()
            .unwrap();

        let response = client
            .get_local(&format!(
                "https://localhost:{}/missing-alpn",
                target_addr.port()
            ))
            .unwrap()
            .send()
            .await
            .unwrap();

        assert_eq!(response.text().await.unwrap(), "hello tls");
        assert_eq!(
            observer.details(),
            [
                LocalProxyDetail::Tcp(aioduct::NegotiatedProtocol::Http1),
                LocalProxyDetail::Tls(None),
            ]
        );
    });
}

#[cfg(feature = "rustls")]
#[test]
fn test_compio_proxied_origin_tls_observer_fires_before_http_handshake_failure_local() {
    let (target_addr, target_cert) = start_tls_origin_aborting_before_http_tokio();
    let proxy_addr = start_http_proxy_tokio();

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let connector = aioduct::tls::RustlsConnector::new(
            aioduct_test_server::tls::make_client_config(&target_cert),
        );
        let observer = LocalProxyDetailObserver::default();
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .tls(connector)
            .proxy(aioduct::ProxyConfig::http(&format!("http://{proxy_addr}")).unwrap())
            .request_observer(observer.clone())
            .timeout(Duration::from_secs(5))
            .build_local()
            .unwrap();

        client
            .get_local(&format!(
                "https://localhost:{}/http-handshake-abort",
                target_addr.port()
            ))
            .unwrap()
            .send()
            .await
            .unwrap_err();

        let details = observer.details();
        assert!(
            details.starts_with(&[
                LocalProxyDetail::Tcp(aioduct::NegotiatedProtocol::Http1),
                LocalProxyDetail::Tls(Some("http/1.1".to_owned())),
            ]),
            "origin TLS completion must be observable before HTTP setup fails: {details:?}"
        );
    });
}

#[cfg(feature = "rustls")]
#[test]
fn test_compio_https_proxy_tcp_observer_fires_before_proxy_tls_timeout_local() {
    let proxy_addr = start_stalled_https_proxy_tokio();

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let observer = LocalProxyDetailObserver::default();
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .tls(aioduct::tls::RustlsConnector::danger_accept_invalid_certs())
            .proxy(aioduct::proxy::ProxyConfig::https(&format!("https://{proxy_addr}")).unwrap())
            .request_observer(observer.clone())
            .connect_timeout(Duration::from_millis(100))
            .build_local()
            .unwrap();

        client
            .get_local("http://origin.test/proxy-tls-timeout")
            .unwrap()
            .h2c_prior_knowledge()
            .send()
            .await
            .unwrap_err();

        assert_eq!(
            observer.details(),
            [LocalProxyDetail::Tcp(aioduct::NegotiatedProtocol::Http1)],
            "proxy TCP observation should precede a stalled proxy TLS handshake"
        );
    });
}

#[cfg(feature = "rustls")]
#[test]
fn test_compio_https_proxy_observer_fires_before_origin_tls_failure_local() {
    let target_addr = start_closing_server_tokio();
    let (proxy_addr, proxy_cert) = start_https_proxy_tokio();

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let connector = aioduct::tls::RustlsConnector::new(
            aioduct_test_server::tls::make_client_config(&proxy_cert),
        );
        let observer = LocalProxyDetailObserver::default();
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .tls(connector)
            .proxy(
                aioduct::proxy::ProxyConfig::https(&format!(
                    "https://localhost:{}",
                    proxy_addr.port()
                ))
                .unwrap(),
            )
            .request_observer(observer.clone())
            .timeout(Duration::from_secs(2))
            .build_local()
            .unwrap();

        client
            .get_local(&format!("https://{target_addr}/fails-during-origin-tls"))
            .unwrap()
            .send()
            .await
            .unwrap_err();

        let details = observer.details();
        assert!(
            details.contains(&LocalProxyDetail::Tcp(aioduct::NegotiatedProtocol::Http1)),
            "missing HTTP/1.1 proxy TCP phase after origin TLS failure: {details:?}"
        );
        assert!(
            details.contains(&LocalProxyDetail::Tls(Some("http/1.1".to_owned()))),
            "missing actual proxy TLS ALPN after origin TLS failure: {details:?}"
        );
    });
}

#[cfg(feature = "rustls")]
#[test]
fn test_compio_two_https_proxy_hops_local() {
    let target_addr = start_server_tokio();
    let (second_addr, second_cert) = start_https_proxy_tokio();
    let (first_addr, first_cert) = start_https_proxy_tokio();
    let mut roots = rustls::RootCertStore::empty();
    roots.add(first_cert).unwrap();
    roots.add(second_cert).unwrap();
    let mut config =
        rustls::ClientConfig::builder_with_provider(aioduct_test_server::tls::crypto_provider())
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_root_certificates(roots)
            .with_no_client_auth();
    config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    let connector = aioduct::tls::RustlsConnector::new(std::sync::Arc::new(config));
    let chain = aioduct::proxy::ProxyChain::new(vec![
        aioduct::proxy::ProxyConfig::https(&format!("https://localhost:{}", first_addr.port()))
            .unwrap(),
        aioduct::proxy::ProxyConfig::https(&format!("https://localhost:{}", second_addr.port()))
            .unwrap(),
    ]);

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .tls(connector)
            .proxy_chain(chain)
            .timeout(Duration::from_secs(5))
            .build_local()
            .unwrap();
        let response = client
            .get_local(&format!("http://{target_addr}/two-https-proxies"))
            .unwrap()
            .send()
            .await
            .unwrap();

        assert_eq!(response.status(), http::StatusCode::OK);
        assert_eq!(response.text().await.unwrap(), "hello aioduct");
    });
}

#[cfg(feature = "rustls")]
fn assert_mixed_chain_local(
    first: mixed_chain::ProxyKind,
    second: mixed_chain::ProxyKind,
    origin_protocol: mixed_chain::OriginProtocol,
) {
    use mixed_chain::{FIRST_PROXY_HOST, LiveMixedProxyChain, ORIGIN_HOST, SECOND_PROXY_HOST};

    let fixture = LiveMixedProxyChain::start_with_origin(first, second, origin_protocol);
    let connector = aioduct::tls::RustlsConnector::new(mixed_chain::client_config_trusting(
        fixture.certificates(),
    ));
    let first_proxy = fixture.first_proxy();
    let second_proxy = fixture.second_proxy();
    let first_addr = fixture.first_addr();
    let second_addr = fixture.second_addr();
    let origin_addr = fixture.origin_addr();
    let origin_url = fixture.origin_url();

    compio_runtime::Runtime::new()
        .unwrap()
        .block_on(Box::pin(async move {
            let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
                .tls(connector)
                .proxy_chain(aioduct::ProxyChain::new(vec![first_proxy, second_proxy]))
                .resolve(FIRST_PROXY_HOST, first_addr)
                .resolve(SECOND_PROXY_HOST, second_addr)
                .resolve(ORIGIN_HOST, origin_addr)
                .timeout(Duration::from_secs(5))
                .build_local()
                .unwrap();
            let response = client
                .get_local(&origin_url)
                .unwrap()
                .send()
                .await
                .unwrap_or_else(|error| {
                    panic!("{first:?} -> {second:?} via {origin_protocol:?} failed: {error}")
                });
            assert_eq!(response.text().await.unwrap(), "mixed-chain-ok");
        }));

    fixture.assert_wire_order();
}

#[cfg(feature = "rustls")]
#[test]
fn test_compio_all_two_hop_proxy_pairs_execute_in_wire_order_local() {
    let cases = mixed_chain::ordered_proxy_pairs();
    mixed_chain::assert_complete_ordered_pairs(&cases);
    for (first, second) in cases {
        assert_mixed_chain_local(first, second, mixed_chain::OriginProtocol::Http1);
    }
}

#[cfg(feature = "rustls")]
#[test]
fn test_compio_chained_https_origins_negotiate_h1_and_h2_for_all_proxy_pairs_local() {
    let cases = mixed_chain::ordered_proxy_pairs();
    mixed_chain::assert_complete_ordered_pairs(&cases);
    assert!(
        cases.contains(&(mixed_chain::ProxyKind::Https, mixed_chain::ProxyKind::Https)),
        "HTTPS origin matrix must include HTTPS -> HTTPS -> HTTPS triple TLS"
    );
    for protocol in [
        mixed_chain::OriginProtocol::HttpsHttp1,
        mixed_chain::OriginProtocol::HttpsHttp2,
    ] {
        for &(first, second) in &cases {
            assert_mixed_chain_local(first, second, protocol);
        }
    }
}

#[cfg(all(feature = "rustls", feature = "rustls-aws-lc-rs"))]
#[test]
fn test_compio_ech_https_proxy_hops_fail_before_dns_or_connector_io_local() {
    for https_hop in 0..2 {
        let resolver_attempts = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let resolver_counter = std::sync::Arc::clone(&resolver_attempts);
        let connector = EchPreflightLocalConnector::default();
        let proxies = if https_hop == 0 {
            vec![
                aioduct::ProxyConfig::https("https://first-proxy.test:8443").unwrap(),
                aioduct::ProxyConfig::http("http://second-proxy.test:8080").unwrap(),
            ]
        } else {
            vec![
                aioduct::ProxyConfig::http("http://first-proxy.test:8080").unwrap(),
                aioduct::ProxyConfig::https("https://second-proxy.test:8443").unwrap(),
            ]
        };
        let error = compio_runtime::Runtime::new().unwrap().block_on(async {
            let client = HttpEngineLocal::<CompioRuntime, EchPreflightLocalConnector>::builder_with_connector(
                connector.clone(),
            )
            .tls(ech_grease_connector())
            .proxy_chain(aioduct::ProxyChain::new(proxies))
            .resolver(move |_host: &str, _port: u16| {
                resolver_counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Box::pin(async { Ok("127.0.0.1:9".parse().unwrap()) })
                    as std::pin::Pin<
                        Box<
                            dyn std::future::Future<
                                    Output = std::io::Result<std::net::SocketAddr>,
                                > + Send,
                        >,
                    >
            })
            .build_local()
            .unwrap();

            client
                .get_local("http://origin.test/ech-preflight")
                .unwrap()
                .send()
                .await
                .unwrap_err()
        });

        assert!(
            error
                .to_string()
                .contains("cannot inherit an ECH-enabled origin configuration"),
            "unexpected ECH preflight error for hop {https_hop}: {error}"
        );
        assert_eq!(
            resolver_attempts.load(std::sync::atomic::Ordering::SeqCst),
            0
        );
        assert_eq!(
            connector.attempts.load(std::sync::atomic::Ordering::SeqCst),
            0
        );
    }
}

#[test]
fn test_compio_http_proxy_with_auth_local() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    let target_addr = start_server_tokio();
    let auth_seen = Arc::new(AtomicBool::new(false));
    let auth_seen_clone = auth_seen.clone();

    let proxy_addr = {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpStream;

        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                let addr = listener.local_addr().unwrap();
                tx.send(addr).unwrap();

                loop {
                    let (mut client, _) = match listener.accept().await {
                        Ok(c) => c,
                        Err(_) => return,
                    };
                    let auth_seen = auth_seen_clone.clone();
                    tokio::spawn(async move {
                        let mut buf = vec![0u8; 8192];
                        let n = match client.read(&mut buf).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => n,
                        };
                        let head = String::from_utf8_lossy(&buf[..n]);
                        if !head.starts_with("CONNECT ") {
                            return;
                        }
                        if head.contains("proxy-authorization:")
                            || head.contains("Proxy-Authorization:")
                        {
                            auth_seen.store(true, Ordering::SeqCst);
                        }
                        let target = head.split_whitespace().nth(1).unwrap_or("");
                        client
                            .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                            .await
                            .unwrap();
                        let mut target_stream = match TcpStream::connect(target).await {
                            Ok(s) => s,
                            Err(_) => return,
                        };
                        let _ =
                            tokio::io::copy_bidirectional(&mut client, &mut target_stream).await;
                    });
                }
            });
        });
        rx.recv().unwrap()
    };

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .proxy(
                aioduct::proxy::ProxyConfig::http(&format!("http://{proxy_addr}"))
                    .unwrap()
                    .basic_auth("Aladdin", "open sesame"),
            )
            .build_local()
            .unwrap();

        let resp = client
            .get_local(&format!("http://{target_addr}/auth-test"))
            .unwrap()
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), http::StatusCode::OK);
        assert_eq!(resp.text().await.unwrap(), "hello aioduct");
    });

    // Give the proxy thread a moment to process the auth check
    std::thread::sleep(Duration::from_millis(50));
    assert!(
        auth_seen.load(Ordering::SeqCst),
        "CONNECT request should include Proxy-Authorization header"
    );
}

#[test]
fn test_compio_socks5_proxy_with_keepalive_and_fast_open() {
    let target_addr = start_server_tokio();
    let socks5_addr = start_socks5_proxy_tokio();

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .proxy(aioduct::proxy::ProxyConfig::socks5(&format!("socks5://{socks5_addr}")).unwrap())
            .tcp_keepalive(Duration::from_secs(30))
            .tcp_keepalive_interval(Duration::from_secs(10))
            .tcp_keepalive_retries(3)
            .tcp_fast_open(true)
            .timeout(Duration::from_secs(2))
            .build_local()
            .unwrap();

        let result = client
            .get_local(&format!("http://127.0.0.1:{}/", target_addr.port()))
            .unwrap()
            .send()
            .await;

        assert!(
            result.is_ok(),
            "SOCKS5 proxy with keepalive/fast_open via compio failed: {:?}",
            result.unwrap_err()
        );
    });
}

// ── Observer notification tests for TCP connections (execute_local.rs coverage) ────

#[test]
fn test_compio_observer_tcp_connected_on_plain_http() {
    use std::sync::{Arc, Mutex};

    let addr = start_server_tokio();
    compio_runtime::Runtime::new().unwrap().block_on(async {
        let phases = Arc::new(Mutex::new(Vec::<String>::new()));
        let phases_clone = phases.clone();

        struct PhaseObs(Arc<Mutex<Vec<String>>>);
        impl aioduct::observer::RequestObserver for PhaseObs {
            fn on_event(&self, event: &aioduct::observer::RequestEvent) {
                self.0.lock().unwrap().push(format!("{:?}", event.phase));
            }
            fn on_connection_event(&self, _event: &aioduct::observer::ConnectionEvent) {}
        }

        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .request_observer(PhaseObs(phases_clone))
            .no_connection_reuse()
            .build_local()
            .unwrap();

        // First request -- new connection, should fire DnsResolved and TcpConnected
        let resp = client
            .get_local(&format!("http://{addr}/"))
            .unwrap()
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), http::StatusCode::OK);
        let _ = resp.text().await.unwrap();

        let recorded = phases.lock().unwrap();
        let joined = recorded.join("\n");

        // Verify observer received DnsResolved notification
        assert!(
            joined.contains("DnsResolved"),
            "observer should have recorded DnsResolved, got:\n{joined}"
        );

        // Verify observer received TcpConnected notification (non-TLS path, execute_local.rs ~line 581-591)
        assert!(
            joined.contains("TcpConnected"),
            "observer should have recorded TcpConnected for plain HTTP, got:\n{joined}"
        );

        // Verify PoolCheckoutComplete with Miss (new connection path)
        assert!(
            joined.contains("Miss"),
            "observer should have recorded pool Miss, got:\n{joined}"
        );

        // Verify Started, RequestSent, ResponseStarted, ResponseComplete
        assert!(
            joined.contains("Started"),
            "observer should have recorded Started, got:\n{joined}"
        );
        assert!(
            joined.contains("RequestSent"),
            "observer should have recorded RequestSent, got:\n{joined}"
        );
        assert!(
            joined.contains("ResponseStarted"),
            "observer should have recorded ResponseStarted, got:\n{joined}"
        );
        assert!(
            joined.contains("ResponseComplete"),
            "observer should have recorded ResponseComplete, got:\n{joined}"
        );
    });
}

#[test]
fn test_compio_observer_pool_hit_vs_miss() {
    use std::sync::{Arc, Mutex};

    let addr = start_server_tokio();
    compio_runtime::Runtime::new().unwrap().block_on(async {
        let phases = Arc::new(Mutex::new(Vec::<String>::new()));
        let phases_clone = phases.clone();

        struct PhaseObs(Arc<Mutex<Vec<String>>>);
        impl aioduct::observer::RequestObserver for PhaseObs {
            fn on_event(&self, event: &aioduct::observer::RequestEvent) {
                self.0.lock().unwrap().push(format!("{:?}", event.phase));
            }
            fn on_connection_event(&self, _event: &aioduct::observer::ConnectionEvent) {}
        }

        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .request_observer(PhaseObs(phases_clone))
            .build_local()
            .unwrap();

        let url = format!("http://{addr}/");

        // First request: pool miss, fires TcpConnected + DnsResolved
        let resp1 = client.get_local(&url).unwrap().send().await.unwrap();
        assert_eq!(resp1.status(), http::StatusCode::OK);
        let _ = resp1.text().await.unwrap();

        // Record phases from first request
        let first_phases = {
            let recorded = phases.lock().unwrap();
            recorded.clone()
        };

        // Clear for second request
        phases.lock().unwrap().clear();

        // Second request: pool hit, should NOT fire DnsResolved/TcpConnected
        let resp2 = client.get_local(&url).unwrap().send().await.unwrap();
        assert_eq!(resp2.status(), http::StatusCode::OK);
        let _ = resp2.text().await.unwrap();

        let second_phases = phases.lock().unwrap().clone();
        let second_joined = second_phases.join("\n");

        // First request should have DnsResolved
        let first_joined = first_phases.join("\n");
        assert!(
            first_joined.contains("DnsResolved"),
            "first request should have DnsResolved, got:\n{first_joined}"
        );

        // Second request should have pool Hit (reuse), not DnsResolved
        assert!(
            second_joined.contains("Hit"),
            "second request should have pool Hit, got:\n{second_joined}"
        );
        assert!(
            !second_joined.contains("DnsResolved"),
            "second request should NOT have DnsResolved (pool hit), got:\n{second_joined}"
        );
    });
}

// ── TCP fast open test via direct connection ─────────────────────────

#[test]
fn test_compio_tcp_fast_open_direct() {
    let addr = start_server_tokio();
    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .tcp_fast_open(true)
            .build_local()
            .unwrap();
        let resp = client
            .get_local(&format!("http://{addr}/"))
            .unwrap()
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), http::StatusCode::OK);
        assert_eq!(resp.text().await.unwrap(), "hello aioduct");
    });
}

// ── TCP keepalive through proxy connections ──────────────────────────

#[test]
fn test_compio_socks5_proxy_with_tcp_keepalive() {
    let target_addr = start_server_tokio();
    let socks5_addr = start_socks5_proxy_tokio();

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .proxy(aioduct::proxy::ProxyConfig::socks5(&format!("socks5://{socks5_addr}")).unwrap())
            .tcp_keepalive(Duration::from_secs(60))
            .timeout(Duration::from_secs(2))
            .build_local()
            .unwrap();

        let result = client
            .get_local(&format!("http://127.0.0.1:{}/", target_addr.port()))
            .unwrap()
            .send()
            .await;

        assert!(
            result.is_ok(),
            "SOCKS5 proxy with keepalive via compio failed: {:?}",
            result.unwrap_err()
        );
    });
}

// ── Proxy connection reuse test ─────────────────────────────────────

#[test]
fn test_compio_http_proxy_connection_reuse_local() {
    let target_addr = start_server_tokio();
    let http_proxy_addr = start_http_proxy_tokio();

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .proxy(aioduct::proxy::ProxyConfig::http(&format!("http://{http_proxy_addr}")).unwrap())
            .pool_idle_timeout(std::time::Duration::from_secs(60))
            .pool_max_idle_per_host(5)
            .build_local()
            .unwrap();

        // First request
        let resp1 = client
            .get_local(&format!("http://{target_addr}/first"))
            .unwrap()
            .send()
            .await
            .unwrap();
        assert_eq!(resp1.status(), http::StatusCode::OK);
        let body1 = resp1.text().await.unwrap();
        assert!(
            body1.contains("hello aioduct"),
            "first request should succeed"
        );

        // Second request -- should reuse the connection
        let resp2 = client
            .get_local(&format!("http://{target_addr}/second"))
            .unwrap()
            .send()
            .await
            .unwrap();
        assert_eq!(resp2.status(), http::StatusCode::OK);
        let body2 = resp2.text().await.unwrap();
        assert!(
            body2.contains("hello aioduct"),
            "second request should also succeed"
        );
    });
}

// ── Socks4 with TCP fast open and keepalive ─────────────────────────

#[test]
fn test_compio_socks4_proxy_with_tcp_options() {
    let target_addr = start_server_tokio();
    let socks4_addr = start_socks4_proxy_tokio();

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .proxy(aioduct::proxy::ProxyConfig::socks4(&format!("socks4://{socks4_addr}")).unwrap())
            .tcp_keepalive(Duration::from_secs(30))
            .tcp_fast_open(true)
            .timeout(Duration::from_secs(2))
            .build_local()
            .unwrap();

        let result = client
            .get_local(&format!("http://127.0.0.1:{}/", target_addr.port()))
            .unwrap()
            .send()
            .await;

        assert!(
            result.is_ok(),
            "SOCKS4 proxy with tcp options via compio failed: {:?}",
            result.unwrap_err()
        );
    });
}

// #84: H2 connection multiplexing should work in local (compio) path
