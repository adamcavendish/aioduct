use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use crate::runtime::Runtime;

const HAPPY_EYEBALLS_DELAY: Duration = Duration::from_millis(250);

pub(crate) async fn connect_happy_eyeballs<R: Runtime>(
    addrs: &[SocketAddr],
    local_address: Option<std::net::IpAddr>,
) -> io::Result<(R::TcpStream, SocketAddr)> {
    if addrs.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::AddrNotAvailable,
            "no addresses to connect to",
        ));
    }

    if addrs.len() == 1 {
        let stream = connect_one::<R>(addrs[0], local_address).await?;
        return Ok((stream, addrs[0]));
    }

    let interleaved = interleave_addrs(addrs);
    race_connect::<R>(&interleaved, local_address).await
}

fn interleave_addrs(addrs: &[SocketAddr]) -> Vec<SocketAddr> {
    let (v6, v4): (Vec<&SocketAddr>, Vec<&SocketAddr>) = addrs.iter().partition(|a| a.is_ipv6());
    let mut result = Vec::with_capacity(addrs.len());
    let mut i6 = v6.into_iter();
    let mut i4 = v4.into_iter();
    loop {
        let a = i6.next();
        let b = i4.next();
        if a.is_none() && b.is_none() {
            break;
        }
        if let Some(addr) = a {
            result.push(*addr);
        }
        if let Some(addr) = b {
            result.push(*addr);
        }
    }
    result
}

async fn race_connect<R: Runtime>(
    addrs: &[SocketAddr],
    local_address: Option<std::net::IpAddr>,
) -> io::Result<(R::TcpStream, SocketAddr)> {
    let mut last_err = io::Error::new(io::ErrorKind::AddrNotAvailable, "no addresses");

    for (i, &addr) in addrs.iter().enumerate() {
        let is_last = i == addrs.len() - 1;

        if is_last {
            match connect_one::<R>(addr, local_address).await {
                Ok(stream) => return Ok((stream, addr)),
                Err(e) => last_err = e,
            }
        } else {
            match connect_with_deadline::<R>(addr, local_address).await {
                ConnectResult::Connected(stream) => return Ok((stream, addr)),
                ConnectResult::Failed(e) => last_err = e,
                ConnectResult::DeadlineReached => {}
            }
        }
    }

    Err(last_err)
}

enum ConnectResult<T> {
    Connected(T),
    Failed(io::Error),
    DeadlineReached,
}

async fn connect_with_deadline<R: Runtime>(
    addr: SocketAddr,
    local_address: Option<std::net::IpAddr>,
) -> ConnectResult<R::TcpStream> {
    SelectConnect::<R> {
        connect: Box::pin(connect_one::<R>(addr, local_address)),
        sleep: Box::pin(R::sleep(HAPPY_EYEBALLS_DELAY)),
        done: false,
    }
    .await
}

struct SelectConnect<R: Runtime> {
    connect: Pin<Box<dyn std::future::Future<Output = io::Result<R::TcpStream>> + Send>>,
    sleep: Pin<Box<dyn std::future::Future<Output = ()> + Send>>,
    done: bool,
}

impl<R: Runtime> std::future::Future for SelectConnect<R> {
    type Output = ConnectResult<R::TcpStream>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = unsafe { self.get_unchecked_mut() };

        if this.done {
            return Poll::Pending;
        }

        if let Poll::Ready(result) = this.connect.as_mut().poll(cx) {
            this.done = true;
            return Poll::Ready(match result {
                Ok(stream) => ConnectResult::Connected(stream),
                Err(e) => ConnectResult::Failed(e),
            });
        }

        if let Poll::Ready(()) = this.sleep.as_mut().poll(cx) {
            this.done = true;
            return Poll::Ready(ConnectResult::DeadlineReached);
        }

        Poll::Pending
    }
}

async fn connect_one<R: Runtime>(
    addr: SocketAddr,
    local_address: Option<std::net::IpAddr>,
) -> io::Result<R::TcpStream> {
    if let Some(local) = local_address {
        R::connect_bound(addr, local).await
    } else {
        R::connect(addr).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interleave_v6_first() {
        let addrs = vec![
            "127.0.0.1:80".parse().unwrap(),
            "[::1]:80".parse().unwrap(),
            "10.0.0.1:80".parse().unwrap(),
            "[::2]:80".parse().unwrap(),
        ];
        let result = interleave_addrs(&addrs);
        assert!(result[0].is_ipv6());
        assert!(result[1].is_ipv4());
        assert!(result[2].is_ipv6());
        assert!(result[3].is_ipv4());
    }

    #[test]
    fn interleave_only_v4() {
        let addrs = vec![
            "1.1.1.1:443".parse().unwrap(),
            "8.8.8.8:443".parse().unwrap(),
        ];
        let result = interleave_addrs(&addrs);
        assert_eq!(result.len(), 2);
        assert!(result.iter().all(|a| a.is_ipv4()));
    }

    #[test]
    fn interleave_empty() {
        let result = interleave_addrs(&[]);
        assert!(result.is_empty());
    }
}
