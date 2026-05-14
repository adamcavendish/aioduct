use std::net::SocketAddr;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use crate::ConnectionCounter;
use crate::h1::http_200_keepalive;

pub async fn h1_rst_on_reuse() -> (SocketAddr, ConnectionCounter) {
    let counter = ConnectionCounter::new();
    let counter2 = counter.clone();

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        loop {
            let (mut stream, _) = match listener.accept().await {
                Ok(v) => v,
                Err(_) => continue,
            };
            let n = counter2.inc_connections();
            counter2.inc_requests();

            if n == 0 {
                tokio::spawn(async move {
                    let mut buf = [0u8; 4096];
                    let _ = stream.read(&mut buf).await;
                    let _ = stream.write_all(http_200_keepalive()).await;
                    let _ = stream.flush().await;

                    let mut peek = [0u8; 1];
                    match stream.read(&mut peek).await {
                        Ok(0) | Err(_) => return,
                        Ok(_) => {}
                    }

                    let raw = stream.into_std().unwrap();
                    let sock = socket2::SockRef::from(&raw);
                    let _ = sock.set_linger(Some(Duration::from_secs(0)));
                    drop(raw);
                });
            } else {
                tokio::spawn(async move {
                    let mut buf = [0u8; 4096];
                    let _ = stream.read(&mut buf).await;
                    let _ = stream.write_all(http_200_keepalive()).await;
                    let _ = stream.flush().await;
                });
            }
        }
    });

    (addr, counter)
}

pub async fn h1_fin_on_reuse() -> (SocketAddr, ConnectionCounter) {
    let counter = ConnectionCounter::new();
    let counter2 = counter.clone();

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        loop {
            let (mut stream, _) = match listener.accept().await {
                Ok(v) => v,
                Err(_) => continue,
            };
            let n = counter2.inc_connections();
            counter2.inc_requests();

            if n == 0 {
                tokio::spawn(async move {
                    let mut buf = [0u8; 4096];
                    let _ = stream.read(&mut buf).await;
                    let _ = stream.write_all(http_200_keepalive()).await;
                    let _ = stream.flush().await;

                    let mut peek = [0u8; 1];
                    match stream.read(&mut peek).await {
                        Ok(0) | Err(_) => return,
                        Ok(_) => {}
                    }

                    let _ = stream.shutdown().await;
                });
            } else {
                tokio::spawn(async move {
                    let mut buf = [0u8; 4096];
                    let _ = stream.read(&mut buf).await;
                    let _ = stream.write_all(http_200_keepalive()).await;
                    let _ = stream.flush().await;
                });
            }
        }
    });

    (addr, counter)
}

pub async fn h1_rst_every_n(n: usize) -> (SocketAddr, ConnectionCounter) {
    let counter = ConnectionCounter::new();
    let counter2 = counter.clone();

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        loop {
            let (mut stream, _) = match listener.accept().await {
                Ok(v) => v,
                Err(_) => continue,
            };
            counter2.inc_connections();
            let counter3 = counter2.clone();
            let max_serve = n;

            tokio::spawn(async move {
                let mut served = 0usize;
                loop {
                    let mut buf = [0u8; 4096];
                    let bytes = match stream.read(&mut buf).await {
                        Ok(0) => break,
                        Ok(n) => n,
                        Err(_) => break,
                    };
                    if !buf[..bytes].starts_with(b"GET")
                        && !buf[..bytes].starts_with(b"POST")
                        && !buf[..bytes].starts_with(b"PUT")
                        && !buf[..bytes].starts_with(b"HEAD")
                    {
                        break;
                    }

                    counter3.inc_requests();
                    served += 1;

                    if served >= max_serve {
                        let raw = stream.into_std().unwrap();
                        let sock = socket2::SockRef::from(&raw);
                        let _ = sock.set_linger(Some(Duration::from_secs(0)));
                        drop(raw);
                        return;
                    }

                    let _ = stream.write_all(http_200_keepalive()).await;
                    let _ = stream.flush().await;
                }
            });
        }
    });

    (addr, counter)
}

pub async fn h1_always_rst() -> (SocketAddr, ConnectionCounter) {
    let counter = ConnectionCounter::new();
    let counter2 = counter.clone();

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        loop {
            let (stream, _) = match listener.accept().await {
                Ok(v) => v,
                Err(_) => continue,
            };
            counter2.inc_connections();
            let raw = stream.into_std().unwrap();
            let sock = socket2::SockRef::from(&raw);
            let _ = sock.set_linger(Some(Duration::from_secs(0)));
            drop(raw);
        }
    });

    (addr, counter)
}
