use std::io;
use std::net::SocketAddr;
use std::time::Duration;

use super::smol_rt::{SmolIo, SmolRuntime, SmolSleep};

#[allow(deprecated)]
use super::legacy::Runtime;

#[allow(deprecated)]
impl Runtime for SmolRuntime {
    type TcpStream = SmolIo<smol::net::TcpStream>;
    type Sleep = SmolSleep;

    async fn connect(addr: SocketAddr) -> io::Result<Self::TcpStream> {
        let stream = smol::net::TcpStream::connect(addr).await?;
        stream.set_nodelay(true)?;
        Ok(SmolIo::new(stream))
    }

    async fn resolve_all(host: &str, port: u16) -> io::Result<Vec<SocketAddr>> {
        let addr = format!("{host}:{port}");
        let addrs: Vec<SocketAddr> = smol::net::resolve(addr).await?;
        if addrs.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::AddrNotAvailable,
                "no addresses found",
            ));
        }
        Ok(addrs)
    }

    fn sleep(duration: Duration) -> Self::Sleep {
        SmolSleep::new(async_io::Timer::after(duration))
    }

    fn spawn<F>(future: F)
    where
        F: std::future::Future<Output = ()> + Send + 'static,
    {
        smol::spawn(future).detach();
    }

    fn set_tcp_keepalive(
        stream: &Self::TcpStream,
        time: Duration,
        interval: Option<Duration>,
        retries: Option<u32>,
    ) -> io::Result<()> {
        use socket2::SockRef;
        let sock_ref = SockRef::from(stream.inner());
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
        use socket2::SockRef;
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

        let sock_ref = SockRef::from(stream.inner());
        let fd = sock_ref.as_raw_fd();
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
        let sock_ref = SockRef::from(stream.inner());
        sock_ref.bind_device(Some(interface.as_bytes()))
    }

    fn from_std_tcp(stream: std::net::TcpStream) -> io::Result<Self::TcpStream> {
        stream.set_nonblocking(true)?;
        stream.set_nodelay(true)?;
        let async_stream = smol::net::TcpStream::try_from(stream)?;
        Ok(SmolIo::new(async_stream))
    }

    async fn connect_bound(
        addr: SocketAddr,
        local: std::net::IpAddr,
    ) -> io::Result<Self::TcpStream> {
        use socket2::{Domain, Protocol, SockAddr, Socket, Type};

        let std_stream = smol::unblock(move || {
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
        .await?;
        std_stream.set_nonblocking(true)?;
        let smol_stream = smol::net::TcpStream::try_from(std_stream)?;
        Ok(SmolIo::new(smol_stream))
    }

    #[cfg(unix)]
    type UnixStream = SmolIo<smol::net::unix::UnixStream>;

    #[cfg(unix)]
    async fn connect_unix(path: &std::path::Path) -> io::Result<Self::UnixStream> {
        let stream = smol::net::unix::UnixStream::connect(path).await?;
        Ok(SmolIo::new(stream))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[allow(deprecated)]
    #[test]
    fn legacy_smol_spawn() {
        smol::block_on(async {
            let flag = Arc::new(AtomicBool::new(false));
            let flag2 = flag.clone();
            SmolRuntime::spawn(async move {
                flag2.store(true, Ordering::SeqCst);
            });
            smol::Timer::after(Duration::from_millis(50)).await;
            assert!(flag.load(Ordering::SeqCst));
        });
    }

    #[allow(deprecated)]
    #[test]
    fn legacy_smol_connect() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        smol::block_on(async {
            let stream = SmolRuntime::connect(addr).await;
            assert!(stream.is_ok());
        });
    }

    #[allow(deprecated)]
    #[test]
    fn legacy_smol_connect_bound() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let local: std::net::IpAddr = "127.0.0.1".parse().unwrap();
        smol::block_on(async {
            let stream = SmolRuntime::connect_bound(addr, local).await;
            assert!(stream.is_ok());
        });
    }

    #[allow(deprecated)]
    #[test]
    fn legacy_smol_from_std_tcp() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let stream = std::net::TcpStream::connect(addr).unwrap();
        let result = SmolRuntime::from_std_tcp(stream);
        assert!(result.is_ok());
    }

    #[allow(deprecated)]
    #[test]
    fn legacy_smol_resolve_all() {
        smol::block_on(async {
            let addrs = SmolRuntime::resolve_all("localhost", 80).await;
            assert!(addrs.is_ok());
            assert!(!addrs.unwrap().is_empty());
        });
    }

    #[allow(deprecated)]
    #[test]
    fn legacy_smol_set_tcp_keepalive() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        smol::block_on(async {
            let stream = SmolRuntime::connect(addr).await.unwrap();
            let result = SmolRuntime::set_tcp_keepalive(
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
    fn legacy_smol_connect_unix() {
        let dir = std::env::temp_dir().join("aioduct_legacy_smol_unix");
        let _ = std::fs::create_dir_all(&dir);
        let sock_path = dir.join("legacy_test.sock");
        let _ = std::fs::remove_file(&sock_path);

        let listener = std::os::unix::net::UnixListener::bind(&sock_path).unwrap();
        smol::block_on(async {
            let stream = SmolRuntime::connect_unix(&sock_path).await;
            assert!(stream.is_ok());
        });
        drop(listener);

        let _ = std::fs::remove_file(&sock_path);
        let _ = std::fs::remove_dir(&dir);
    }

    #[allow(deprecated)]
    #[test]
    fn legacy_smol_set_tcp_keepalive_no_interval_no_retries() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        smol::block_on(async {
            let stream = SmolRuntime::connect(addr).await.unwrap();
            let result =
                SmolRuntime::set_tcp_keepalive(&stream, Duration::from_secs(60), None, None);
            assert!(result.is_ok());
        });
    }

    #[allow(deprecated)]
    #[test]
    fn legacy_smol_resolve_default() {
        smol::block_on(async {
            let addr = SmolRuntime::resolve("localhost", 80).await;
            assert!(addr.is_ok());
        });
    }
}
