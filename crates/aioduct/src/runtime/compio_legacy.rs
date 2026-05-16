use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use super::compio_rt::{CompioIo, CompioRuntime, CompioSleep, CompioTcpStream};
#[allow(deprecated)]
use super::legacy::Runtime;

// Safety: compio is thread-per-core — values never actually cross thread boundaries.
//
// This exists solely for backward compatibility with the deprecated `Runtime` trait,
// which requires `TcpStream: Send`. The new `Connector` trait (used by `HttpEngineLocal`
// in a future phase) does NOT require `Send` on streams. Remove this when the
// deprecated `Runtime` trait is deleted in 0.3.0.
unsafe impl Send for CompioTcpStream {}

#[allow(deprecated)]
impl Runtime for CompioRuntime {
    type TcpStream = CompioTcpStream;
    type Sleep = CompioSleep;

    fn connect(addr: SocketAddr) -> impl Future<Output = io::Result<Self::TcpStream>> + Send {
        // Safety: compio is thread-per-core — this future never crosses thread boundaries.
        AssertSend(async move {
            let stream = compio_net::TcpStream::connect(addr).await?;
            stream.set_nodelay(true)?;
            Ok(CompioTcpStream::new(stream))
        })
    }

    fn resolve_all(
        host: &str,
        port: u16,
    ) -> impl Future<Output = io::Result<Vec<SocketAddr>>> + Send {
        let addr_str = format!("{host}:{port}");
        AssertSend(async move {
            let addrs = compio_runtime::spawn_blocking(move || {
                use std::net::ToSocketAddrs;
                addr_str
                    .to_socket_addrs()
                    .map(|iter| iter.collect::<Vec<_>>())
            })
            .await
            .map_err(|e| io::Error::other(format!("{e:?}")))?;
            let addrs = addrs?;
            if addrs.is_empty() {
                return Err(io::Error::new(
                    io::ErrorKind::AddrNotAvailable,
                    "no addresses found",
                ));
            }
            Ok(addrs)
        })
    }

    fn sleep(duration: Duration) -> Self::Sleep {
        CompioSleep::new(async_io::Timer::after(duration))
    }

    fn spawn<F>(future: F)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        compio_runtime::spawn(future).detach();
    }

    fn set_tcp_keepalive(
        stream: &Self::TcpStream,
        time: Duration,
        interval: Option<Duration>,
        retries: Option<u32>,
    ) -> io::Result<()> {
        use socket2::SockRef;
        let sock_ref = SockRef::from(&stream.socket_handle);
        let mut keepalive = socket2::TcpKeepalive::new().with_time(time);
        if let Some(interval) = interval {
            keepalive = keepalive.with_interval(interval);
        }
        #[cfg(any(
            target_os = "linux",
            target_os = "macos",
            target_os = "ios",
            target_os = "freebsd",
            target_os = "netbsd",
        ))]
        if let Some(retries) = retries {
            keepalive = keepalive.with_retries(retries);
        }
        #[cfg(not(any(
            target_os = "linux",
            target_os = "macos",
            target_os = "ios",
            target_os = "freebsd",
            target_os = "netbsd",
        )))]
        let _ = retries;
        sock_ref.set_tcp_keepalive(&keepalive)
    }

    #[cfg(target_os = "linux")]
    fn set_tcp_fast_open(stream: &Self::TcpStream) -> io::Result<()> {
        use std::os::unix::io::AsRawFd;

        unsafe extern "C" {
            fn setsockopt(
                sockfd: std::ffi::c_int,
                level: std::ffi::c_int,
                optname: std::ffi::c_int,
                optval: *const std::ffi::c_void,
                optlen: u32,
            ) -> std::ffi::c_int;
        }

        let fd = stream.socket_handle.as_raw_fd();
        const IPPROTO_TCP: std::ffi::c_int = 6;
        const TCP_FASTOPEN_CONNECT: std::ffi::c_int = 30;
        let optval: std::ffi::c_int = 1;
        unsafe {
            let ret = setsockopt(
                fd,
                IPPROTO_TCP,
                TCP_FASTOPEN_CONNECT,
                &optval as *const std::ffi::c_int as *const std::ffi::c_void,
                std::mem::size_of::<std::ffi::c_int>() as u32,
            );
            if ret != 0 {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(())
    }

    #[cfg(target_os = "linux")]
    fn bind_device(stream: &Self::TcpStream, interface: &str) -> io::Result<()> {
        use socket2::SockRef;
        let sock_ref = SockRef::from(&stream.socket_handle);
        sock_ref.bind_device(Some(interface.as_bytes()))
    }

    fn from_std_tcp(stream: std::net::TcpStream) -> io::Result<Self::TcpStream> {
        stream.set_nonblocking(true)?;
        stream.set_nodelay(true)?;
        let compio_stream = compio_net::TcpStream::from_std(stream)?;
        Ok(CompioTcpStream::new(compio_stream))
    }

    fn connect_bound(
        addr: SocketAddr,
        local: std::net::IpAddr,
    ) -> impl Future<Output = io::Result<Self::TcpStream>> + Send {
        AssertSend(async move {
            use socket2::{Domain, Protocol, SockAddr, Socket, Type};

            let std_stream = compio_runtime::spawn_blocking(move || {
                let domain = if addr.is_ipv4() {
                    Domain::IPV4
                } else {
                    Domain::IPV6
                };
                let socket = Socket::new(domain, Type::STREAM, Some(Protocol::TCP))?;
                socket.bind(&SockAddr::from(std::net::SocketAddr::new(local, 0)))?;
                socket.connect(&SockAddr::from(addr))?;
                socket.set_tcp_nodelay(true)?;
                Ok::<std::net::TcpStream, io::Error>(socket.into())
            })
            .await
            .map_err(|e| io::Error::other(format!("{e:?}")))?;
            let std_stream = std_stream?;
            std_stream.set_nonblocking(true)?;
            let compio_stream = compio_net::TcpStream::from_std(std_stream)?;
            Ok(CompioTcpStream::new(compio_stream))
        })
    }

    #[cfg(unix)]
    type UnixStream = CompioIo<async_io::Async<std::os::unix::net::UnixStream>>;

    #[cfg(unix)]
    fn connect_unix(
        path: &std::path::Path,
    ) -> impl Future<Output = io::Result<Self::UnixStream>> + Send {
        let path = path.to_owned();
        AssertSend(async move {
            let stream = async_io::Async::<std::os::unix::net::UnixStream>::connect(&path).await?;
            Ok(CompioIo::new(stream))
        })
    }
}

/// Wrapper that unsafely implements Send for a !Send future.
///
/// # Safety
///
/// This is only safe in compio's thread-per-core model where futures are never
/// sent between threads. The CompioRuntime must only be used within a single
/// compio runtime thread.
struct AssertSend<F>(F);

// Safety: compio is thread-per-core — these futures never cross thread boundaries.
// Backward compatibility only: used in the deprecated `Runtime::connect` impl.
// The new `Connector::connect` is async-fn-in-trait and does not need this.
// Remove when the deprecated `Runtime` trait is deleted in 0.3.0.
unsafe impl<F> Send for AssertSend<F> {}

impl<F: Future> Future for AssertSend<F> {
    type Output = F::Output;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let inner = unsafe { self.map_unchecked_mut(|s| &mut s.0) };
        inner.poll(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[allow(deprecated)]
    #[test]
    fn legacy_compio_spawn() {
        compio_runtime::Runtime::new().unwrap().block_on(async {
            let flag = Arc::new(AtomicBool::new(false));
            let flag2 = flag.clone();
            CompioRuntime::spawn(async move {
                flag2.store(true, Ordering::SeqCst);
            });
            compio_runtime::time::sleep(Duration::from_millis(50)).await;
            assert!(flag.load(Ordering::SeqCst));
        });
    }

    #[allow(deprecated)]
    #[test]
    fn legacy_compio_connect() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        compio_runtime::Runtime::new().unwrap().block_on(async {
            let stream = CompioRuntime::connect(addr).await;
            assert!(stream.is_ok());
        });
    }

    #[allow(deprecated)]
    #[test]
    fn legacy_compio_connect_bound() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let local: std::net::IpAddr = "127.0.0.1".parse().unwrap();
        compio_runtime::Runtime::new().unwrap().block_on(async {
            let stream = CompioRuntime::connect_bound(addr, local).await;
            assert!(stream.is_ok());
        });
    }

    #[allow(deprecated)]
    #[test]
    fn legacy_compio_from_std_tcp() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let stream = std::net::TcpStream::connect(addr).unwrap();
        compio_runtime::Runtime::new().unwrap().block_on(async {
            let result = CompioRuntime::from_std_tcp(stream);
            assert!(result.is_ok());
        });
    }

    #[allow(deprecated)]
    #[test]
    fn legacy_compio_resolve_all() {
        compio_runtime::Runtime::new().unwrap().block_on(async {
            let addrs = CompioRuntime::resolve_all("localhost", 80).await;
            assert!(addrs.is_ok());
            assert!(!addrs.unwrap().is_empty());
        });
    }

    #[allow(deprecated)]
    #[test]
    fn legacy_compio_set_tcp_keepalive() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        compio_runtime::Runtime::new().unwrap().block_on(async {
            let stream = CompioRuntime::connect(addr).await.unwrap();
            let result = CompioRuntime::set_tcp_keepalive(
                &stream,
                Duration::from_secs(60),
                Some(Duration::from_secs(10)),
                Some(3),
            );
            assert!(result.is_ok());
        });
    }

    #[cfg(unix)]
    #[allow(deprecated)]
    #[test]
    fn legacy_compio_connect_unix() {
        let dir = std::env::temp_dir().join("aioduct_legacy_compio_unix");
        let _ = std::fs::create_dir_all(&dir);
        let sock_path = dir.join("legacy_test.sock");
        let _ = std::fs::remove_file(&sock_path);

        let _listener = std::os::unix::net::UnixListener::bind(&sock_path).unwrap();
        compio_runtime::Runtime::new().unwrap().block_on(async {
            let stream = CompioRuntime::connect_unix(&sock_path).await;
            assert!(stream.is_ok());
        });

        let _ = std::fs::remove_file(&sock_path);
        let _ = std::fs::remove_dir(&dir);
    }

    #[allow(deprecated)]
    #[test]
    fn legacy_compio_set_tcp_keepalive_no_interval_no_retries() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        compio_runtime::Runtime::new().unwrap().block_on(async {
            let stream = CompioRuntime::connect(addr).await.unwrap();
            let result =
                CompioRuntime::set_tcp_keepalive(&stream, Duration::from_secs(60), None, None);
            assert!(result.is_ok());
        });
    }

    #[allow(deprecated)]
    #[test]
    fn legacy_compio_resolve_default() {
        compio_runtime::Runtime::new().unwrap().block_on(async {
            let addr = CompioRuntime::resolve("localhost", 80).await;
            assert!(addr.is_ok());
        });
    }
}
