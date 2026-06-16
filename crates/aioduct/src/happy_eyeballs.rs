//! Happy Eyeballs v2 (RFC 8305) connection racing.
//!
//! When connecting to a host that resolves to multiple addresses (e.g. both IPv6
//! and IPv4), this module races connections in parallel with staggered starts:
//!
//! 1. Addresses are interleaved by family so neither starves the other. The
//!    family of the first address leads, so a caller preference such as
//!    [`AddressFamily::PreferIpv4`](crate::AddressFamily::PreferIpv4) (which
//!    puts IPv4 first) is honored: `[v4, v6, v4, v6, ...]`. Default resolver
//!    order typically leads with IPv6.
//! 2. The first connection attempt is spawned immediately.
//! 3. Every 250 ms (the Connection Attempt Delay), the next address is tried
//!    while all previous attempts **stay alive**.
//! 4. The first attempt to connect wins; all others are dropped.
//! 5. If all in-flight attempts fail before the timer fires, the next address
//!    is tried immediately without waiting.
//!
//! Two parallel implementations are generated via `impl_race_connect!`:
//! - **Send** (`race_connect`): for poll-based runtimes (tokio, smol) using `spawn_send`.
//! - **Local** (`race_connect_local`): for completion-based runtimes (compio) using `spawn_local`.

use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use futures_channel::mpsc;
use futures_core::Stream;

use crate::runtime::{ConnectorLocal, ConnectorSend, RuntimeLocal, RuntimePoll};

pub(crate) const HAPPY_EYEBALLS_DELAY: Duration = Duration::from_millis(250);

pub(crate) async fn connect_happy_eyeballs<R: RuntimePoll, C: ConnectorSend>(
    connector: &C,
    addrs: &[SocketAddr],
    local_address: Option<std::net::IpAddr>,
) -> io::Result<(C::Stream, SocketAddr)> {
    if addrs.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::AddrNotAvailable,
            "no addresses to connect to",
        ));
    }

    if addrs.len() == 1 {
        let stream = tcp_connect_send::<C>(connector, addrs[0], local_address).await?;
        return Ok((stream, addrs[0]));
    }

    let interleaved = interleave_addrs(addrs);
    race_connect::<R, C>(connector, &interleaved, local_address).await
}

pub(crate) fn interleave_addrs(addrs: &[SocketAddr]) -> Vec<SocketAddr> {
    // Interleave by family so neither starves the other. The family of the
    // first address leads, preserving any caller-expressed preference (e.g.
    // `AddressFamily::PreferIpv4` puts IPv4 first). RFC 8305 interleaves by
    // family but does not mandate IPv6-first; leading with the first address's
    // family keeps the resolver/preference ordering meaningful.
    let lead_is_v6 = addrs.first().map(|a| a.is_ipv6()).unwrap_or(true);
    let (v6, v4): (Vec<&SocketAddr>, Vec<&SocketAddr>) = addrs.iter().partition(|a| a.is_ipv6());
    let mut result = Vec::with_capacity(addrs.len());
    let (mut first, mut second) = if lead_is_v6 {
        (v6.into_iter(), v4.into_iter())
    } else {
        (v4.into_iter(), v6.into_iter())
    };
    loop {
        let a = first.next();
        let b = second.next();
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

async fn tcp_connect_send<C: ConnectorSend>(
    connector: &C,
    addr: SocketAddr,
    local_address: Option<std::net::IpAddr>,
) -> io::Result<C::Stream> {
    if let Some(local) = local_address {
        connector.connect_bound(addr, local).await
    } else {
        connector.connect(addr).await
    }
}

// ── Macro: generate both Send and Local variants ─────────────────────────────

macro_rules! impl_race_connect {
    (
        race_fn: $race_fn:ident,
        spawn_fn: $spawn_fn:ident,
        connect_fn: $connect_fn:ident,
        wait_result: $WaitResult:ident,
        wait_future: $WaitFuture:ident,
        connector_trait: $C:ident $(: $extra_bound:ident)*,
        runtime_trait: $R:ident,
        spawn_method: $spawn_method:ident,
        future_extra_bound: $fut_bound:tt,
    ) => {
        async fn $race_fn<R: $R, C: $C $(+ $extra_bound)*>(
            connector: &C,
            addrs: &[SocketAddr],
            local_address: Option<std::net::IpAddr>,
        ) -> io::Result<(C::Stream, SocketAddr)> {
            let mut last_err =
                io::Error::new(io::ErrorKind::AddrNotAvailable, "no addresses");

            let (tx, mut rx) =
                mpsc::unbounded::<Result<(C::Stream, SocketAddr), io::Error>>();

            let mut next_idx = 0;
            let mut in_flight = 0usize;

            $spawn_fn::<R, C>(connector, addrs[next_idx], local_address, &tx);
            next_idx += 1;
            in_flight += 1;

            let mut delay: Pin<Box<dyn std::future::Future<Output = ()> + $fut_bound>> =
                if next_idx < addrs.len() {
                    Box::pin(R::sleep(HAPPY_EYEBALLS_DELAY))
                } else {
                    Box::pin(std::future::pending())
                };

            loop {
                match ($WaitResult::<C>::wait(&mut rx, &mut delay)).await {
                    $WaitResult::Message(Ok((stream, addr))) => return Ok((stream, addr)),
                    $WaitResult::Message(Err(e)) => {
                        last_err = e;
                        in_flight -= 1;
                        if in_flight == 0 {
                            if next_idx >= addrs.len() {
                                return Err(last_err);
                            }
                            $spawn_fn::<R, C>(
                                connector,
                                addrs[next_idx],
                                local_address,
                                &tx,
                            );
                            next_idx += 1;
                            in_flight += 1;
                            delay = if next_idx < addrs.len() {
                                Box::pin(R::sleep(HAPPY_EYEBALLS_DELAY))
                            } else {
                                Box::pin(std::future::pending())
                            };
                        }
                    }
                    $WaitResult::Delay => {
                        if next_idx < addrs.len() {
                            $spawn_fn::<R, C>(
                                connector,
                                addrs[next_idx],
                                local_address,
                                &tx,
                            );
                            next_idx += 1;
                            in_flight += 1;
                            delay = if next_idx < addrs.len() {
                                Box::pin(R::sleep(HAPPY_EYEBALLS_DELAY))
                            } else {
                                Box::pin(std::future::pending())
                            };
                        }
                    }
                    $WaitResult::ChannelClosed => {
                        return Err(last_err);
                    }
                }
            }
        }

        fn $spawn_fn<R: $R, C: $C $(+ $extra_bound)*>(
            connector: &C,
            addr: SocketAddr,
            local_address: Option<std::net::IpAddr>,
            tx: &mpsc::UnboundedSender<Result<(C::Stream, SocketAddr), io::Error>>,
        ) {
            let connector = connector.clone();
            let tx = tx.clone();
            R::$spawn_method(async move {
                let result = $connect_fn::<C>(&connector, addr, local_address).await;
                let _ = tx.unbounded_send(result.map(|stream| (stream, addr)));
            });
        }

        enum $WaitResult<C: $C $(+ $extra_bound)*> {
            Message(Result<(C::Stream, SocketAddr), io::Error>),
            Delay,
            ChannelClosed,
        }

        impl<C: $C $(+ $extra_bound)*> $WaitResult<C> {
            fn wait<'a>(
                rx: &'a mut mpsc::UnboundedReceiver<
                    Result<(C::Stream, SocketAddr), io::Error>,
                >,
                delay: &'a mut Pin<
                    Box<dyn std::future::Future<Output = ()> + $fut_bound>,
                >,
            ) -> $WaitFuture<'a, C> {
                $WaitFuture {
                    rx,
                    delay,
                    _marker: std::marker::PhantomData,
                }
            }
        }

        struct $WaitFuture<'a, C: $C $(+ $extra_bound)*> {
            rx: &'a mut mpsc::UnboundedReceiver<
                Result<(C::Stream, SocketAddr), io::Error>,
            >,
            delay: &'a mut Pin<
                Box<dyn std::future::Future<Output = ()> + $fut_bound>,
            >,
            _marker: std::marker::PhantomData<C>,
        }

        impl<C: $C $(+ $extra_bound)*> std::future::Future for $WaitFuture<'_, C> {
            type Output = $WaitResult<C>;

            fn poll(
                self: Pin<&mut Self>,
                cx: &mut Context<'_>,
            ) -> Poll<Self::Output> {
                // SAFETY: we only hold mutable references to sub-futures; we never move them.
                let this = unsafe { self.get_unchecked_mut() };

                if let Poll::Ready(msg) = Pin::new(&mut *this.rx).poll_next(cx) {
                    return Poll::Ready(match msg {
                        Some(result) => $WaitResult::Message(result),
                        None => $WaitResult::ChannelClosed,
                    });
                }

                if let Poll::Ready(()) = this.delay.as_mut().poll(cx) {
                    return Poll::Ready($WaitResult::Delay);
                }

                Poll::Pending
            }
        }
    };
}

// ── Send variant (tokio / smol) ──────────────────────────────────────────────

impl_race_connect! {
    race_fn: race_connect,
    spawn_fn: spawn_attempt,
    connect_fn: tcp_connect_send,
    wait_result: WaitResult,
    wait_future: WaitFuture,
    connector_trait: ConnectorSend,
    runtime_trait: RuntimePoll,
    spawn_method: spawn_send,
    future_extra_bound: Send,
}

// ── Local variant (compio) ───────────────────────────────────────────────────

async fn tcp_connect_local<C: ConnectorLocal>(
    connector: &C,
    addr: SocketAddr,
    local_address: Option<std::net::IpAddr>,
) -> io::Result<C::Stream> {
    if let Some(local) = local_address {
        connector.connect_bound(addr, local).await
    } else {
        connector.connect(addr).await
    }
}

pub(crate) async fn connect_happy_eyeballs_local<R: RuntimeLocal, C: ConnectorLocal + Clone>(
    connector: &C,
    addrs: &[SocketAddr],
    local_address: Option<std::net::IpAddr>,
) -> io::Result<(C::Stream, SocketAddr)> {
    if addrs.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::AddrNotAvailable,
            "no addresses to connect to",
        ));
    }

    if addrs.len() == 1 {
        let stream = tcp_connect_local::<C>(connector, addrs[0], local_address).await?;
        return Ok((stream, addrs[0]));
    }

    let interleaved = interleave_addrs(addrs);
    race_connect_local::<R, C>(connector, &interleaved, local_address).await
}

impl_race_connect! {
    race_fn: race_connect_local,
    spawn_fn: spawn_attempt_local,
    connect_fn: tcp_connect_local,
    wait_result: WaitResultLocal,
    wait_future: WaitFutureLocal,
    connector_trait: ConnectorLocal: Clone,
    runtime_trait: RuntimeLocal,
    spawn_method: spawn_local,
    future_extra_bound: 'static,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interleave_leads_with_first_family_v6() {
        // First address is IPv6 → IPv6 leads, families interleaved.
        let addrs = vec![
            "[::1]:80".parse().unwrap(),
            "127.0.0.1:80".parse().unwrap(),
            "[::2]:80".parse().unwrap(),
            "10.0.0.1:80".parse().unwrap(),
        ];
        let result = interleave_addrs(&addrs);
        assert!(result[0].is_ipv6());
        assert!(result[1].is_ipv4());
        assert!(result[2].is_ipv6());
        assert!(result[3].is_ipv4());
    }

    #[test]
    fn interleave_leads_with_first_family_v4() {
        // First address is IPv4 (e.g. after AddressFamily::PreferIpv4) → IPv4
        // leads, so the preference survives interleaving.
        let addrs = vec![
            "10.0.0.1:80".parse().unwrap(),
            "10.0.0.2:80".parse().unwrap(),
            "[::1]:80".parse().unwrap(),
            "[::2]:80".parse().unwrap(),
        ];
        let result = interleave_addrs(&addrs);
        assert!(result[0].is_ipv4());
        assert!(result[1].is_ipv6());
        assert!(result[2].is_ipv4());
        assert!(result[3].is_ipv6());
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

    #[test]
    fn interleave_only_v6() {
        let addrs = vec!["[::1]:443".parse().unwrap(), "[::2]:443".parse().unwrap()];
        let result = interleave_addrs(&addrs);
        assert_eq!(result.len(), 2);
        assert!(result.iter().all(|a| a.is_ipv6()));
    }

    #[test]
    fn interleave_single_v4() {
        let addrs = vec!["1.2.3.4:80".parse().unwrap()];
        let result = interleave_addrs(&addrs);
        assert_eq!(result.len(), 1);
        assert!(result[0].is_ipv4());
    }

    #[test]
    fn interleave_single_v6() {
        let addrs = vec!["[::1]:80".parse().unwrap()];
        let result = interleave_addrs(&addrs);
        assert_eq!(result.len(), 1);
        assert!(result[0].is_ipv6());
    }

    #[test]
    fn interleave_uneven_more_v6() {
        let addrs = vec![
            "[::1]:80".parse().unwrap(),
            "[::2]:80".parse().unwrap(),
            "[::3]:80".parse().unwrap(),
            "1.1.1.1:80".parse().unwrap(),
        ];
        let result = interleave_addrs(&addrs);
        assert_eq!(result.len(), 4);
        assert!(result[0].is_ipv6()); // ::1
        assert!(result[1].is_ipv4()); // 1.1.1.1
        assert!(result[2].is_ipv6()); // ::2
        assert!(result[3].is_ipv6()); // ::3
    }

    #[test]
    fn interleave_uneven_more_v4() {
        let addrs = vec![
            "1.1.1.1:80".parse().unwrap(),
            "2.2.2.2:80".parse().unwrap(),
            "3.3.3.3:80".parse().unwrap(),
            "[::1]:80".parse().unwrap(),
        ];
        let result = interleave_addrs(&addrs);
        assert_eq!(result.len(), 4);
        // First address is IPv4, so IPv4 leads; the lone IPv6 slots in second.
        assert!(result[0].is_ipv4()); // 1.1.1.1
        assert!(result[1].is_ipv6()); // ::1
        assert!(result[2].is_ipv4()); // 2.2.2.2
        assert!(result[3].is_ipv4()); // 3.3.3.3
    }

    #[test]
    fn interleave_preserves_order_within_family() {
        let addrs = vec![
            "1.0.0.1:80".parse().unwrap(),
            "[2001:db8::1]:80".parse().unwrap(),
            "8.8.8.8:80".parse().unwrap(),
            "[2001:db8::2]:80".parse().unwrap(),
        ];
        let result = interleave_addrs(&addrs);
        let v6: Vec<_> = result.iter().filter(|a| a.is_ipv6()).collect();
        let v4: Vec<_> = result.iter().filter(|a| a.is_ipv4()).collect();
        assert_eq!(v6[0].to_string(), "[2001:db8::1]:80");
        assert_eq!(v6[1].to_string(), "[2001:db8::2]:80");
        assert_eq!(v4[0].to_string(), "1.0.0.1:80");
        assert_eq!(v4[1].to_string(), "8.8.8.8:80");
    }

    #[cfg(feature = "tokio")]
    #[tokio::test]
    async fn connect_empty_addrs_errors() {
        use crate::runtime::tokio_rt::{TcpConnector, TokioRuntime};
        let connector = TcpConnector;
        let result =
            connect_happy_eyeballs::<TokioRuntime, TcpConnector>(&connector, &[], None).await;
        let err = result.err().expect("should be an error");
        assert_eq!(err.kind(), io::ErrorKind::AddrNotAvailable);
    }

    #[cfg(feature = "tokio")]
    #[tokio::test]
    async fn connect_single_addr_succeeds() {
        use crate::runtime::tokio_rt::{TcpConnector, TokioRuntime};
        let connector = TcpConnector;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (stream, connected_addr) =
            connect_happy_eyeballs::<TokioRuntime, TcpConnector>(&connector, &[addr], None)
                .await
                .unwrap();
        assert_eq!(connected_addr, addr);
        drop(stream);
    }

    #[cfg(feature = "tokio")]
    #[tokio::test]
    async fn connect_multi_addrs_first_succeeds() {
        use crate::runtime::tokio_rt::{TcpConnector, TokioRuntime};
        let connector = TcpConnector;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let good_addr = listener.local_addr().unwrap();
        let bad_addr: SocketAddr = "127.0.0.1:1".parse().unwrap();
        let (stream, connected_addr) = connect_happy_eyeballs::<TokioRuntime, TcpConnector>(
            &connector,
            &[good_addr, bad_addr],
            None,
        )
        .await
        .unwrap();
        assert_eq!(connected_addr, good_addr);
        drop(stream);
    }

    #[cfg(feature = "tokio")]
    #[tokio::test]
    async fn connect_multi_addrs_second_succeeds() {
        use crate::runtime::tokio_rt::{TcpConnector, TokioRuntime};
        let connector = TcpConnector;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let good_addr = listener.local_addr().unwrap();
        let bad_addr: SocketAddr = "127.0.0.1:1".parse().unwrap();
        let (stream, connected_addr) = connect_happy_eyeballs::<TokioRuntime, TcpConnector>(
            &connector,
            &[bad_addr, good_addr],
            None,
        )
        .await
        .unwrap();
        assert_eq!(connected_addr, good_addr);
        drop(stream);
    }

    #[cfg(feature = "tokio")]
    #[tokio::test]
    async fn connect_all_fail() {
        use crate::runtime::tokio_rt::{TcpConnector, TokioRuntime};
        let connector = TcpConnector;
        let bad1: SocketAddr = "127.0.0.1:1".parse().unwrap();
        let bad2: SocketAddr = "127.0.0.1:2".parse().unwrap();
        let result =
            connect_happy_eyeballs::<TokioRuntime, TcpConnector>(&connector, &[bad1, bad2], None)
                .await;
        assert!(result.is_err());
    }

    #[cfg(feature = "compio")]
    #[test]
    fn local_connect_empty_addrs_errors() {
        use crate::runtime::compio_rt::{CompioRuntime, TcpConnector};
        compio_runtime::Runtime::new().unwrap().block_on(async {
            let connector = TcpConnector;
            let result =
                connect_happy_eyeballs_local::<CompioRuntime, TcpConnector>(&connector, &[], None)
                    .await;
            let err = result.err().expect("should be an error");
            assert_eq!(err.kind(), io::ErrorKind::AddrNotAvailable);
        });
    }

    #[cfg(feature = "compio")]
    #[test]
    fn local_connect_single_addr_succeeds() {
        use crate::runtime::compio_rt::{CompioRuntime, TcpConnector};
        compio_runtime::Runtime::new().unwrap().block_on(async {
            let connector = TcpConnector;
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let addr = listener.local_addr().unwrap();
            let (_stream, connected_addr) = connect_happy_eyeballs_local::<
                CompioRuntime,
                TcpConnector,
            >(&connector, &[addr], None)
            .await
            .unwrap();
            assert_eq!(connected_addr, addr);
        });
    }

    #[cfg(feature = "compio")]
    #[test]
    fn local_connect_multi_addrs_first_succeeds() {
        use crate::runtime::compio_rt::{CompioRuntime, TcpConnector};
        compio_runtime::Runtime::new().unwrap().block_on(async {
            let connector = TcpConnector;
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let good_addr = listener.local_addr().unwrap();
            let bad_addr: SocketAddr = "127.0.0.1:1".parse().unwrap();
            let (_stream, connected_addr) = connect_happy_eyeballs_local::<
                CompioRuntime,
                TcpConnector,
            >(&connector, &[good_addr, bad_addr], None)
            .await
            .unwrap();
            assert_eq!(connected_addr, good_addr);
        });
    }

    #[cfg(feature = "compio")]
    #[test]
    fn local_connect_multi_addrs_second_succeeds() {
        use crate::runtime::compio_rt::{CompioRuntime, TcpConnector};
        compio_runtime::Runtime::new().unwrap().block_on(async {
            let connector = TcpConnector;
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let good_addr = listener.local_addr().unwrap();
            let bad_addr: SocketAddr = "127.0.0.1:1".parse().unwrap();
            let (_stream, connected_addr) = connect_happy_eyeballs_local::<
                CompioRuntime,
                TcpConnector,
            >(&connector, &[bad_addr, good_addr], None)
            .await
            .unwrap();
            assert_eq!(connected_addr, good_addr);
        });
    }

    #[cfg(feature = "compio")]
    #[test]
    fn local_connect_all_fail() {
        use crate::runtime::compio_rt::{CompioRuntime, TcpConnector};
        compio_runtime::Runtime::new().unwrap().block_on(async {
            let connector = TcpConnector;
            let bad1: SocketAddr = "127.0.0.1:1".parse().unwrap();
            let bad2: SocketAddr = "127.0.0.1:2".parse().unwrap();
            let result = connect_happy_eyeballs_local::<CompioRuntime, TcpConnector>(
                &connector,
                &[bad1, bad2],
                None,
            )
            .await;
            assert!(result.is_err());
        });
    }

    #[cfg(feature = "tokio")]
    #[tokio::test]
    async fn connect_single_addr_with_local_address() {
        use crate::runtime::tokio_rt::{TcpConnector, TokioRuntime};
        let connector = TcpConnector;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let local_ip: std::net::IpAddr = "127.0.0.1".parse().unwrap();
        let result = connect_happy_eyeballs::<TokioRuntime, TcpConnector>(
            &connector,
            &[addr],
            Some(local_ip),
        )
        .await;
        assert!(result.is_ok());
        let (_, connected_addr) = result.unwrap();
        assert_eq!(connected_addr, addr);
    }

    #[cfg(feature = "tokio")]
    #[tokio::test]
    async fn connect_multi_with_local_address() {
        use crate::runtime::tokio_rt::{TcpConnector, TokioRuntime};
        let connector = TcpConnector;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let good_addr = listener.local_addr().unwrap();
        let bad_addr: SocketAddr = "127.0.0.1:1".parse().unwrap();
        let local_ip: std::net::IpAddr = "127.0.0.1".parse().unwrap();
        let result = connect_happy_eyeballs::<TokioRuntime, TcpConnector>(
            &connector,
            &[bad_addr, good_addr],
            Some(local_ip),
        )
        .await;
        assert!(result.is_ok());
    }

    #[cfg(feature = "compio")]
    #[test]
    fn local_connect_single_with_local_address() {
        use crate::runtime::compio_rt::{CompioRuntime, TcpConnector};
        compio_runtime::Runtime::new().unwrap().block_on(async {
            let connector = TcpConnector;
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let addr = listener.local_addr().unwrap();
            let local_ip: std::net::IpAddr = "127.0.0.1".parse().unwrap();
            let result = connect_happy_eyeballs_local::<CompioRuntime, TcpConnector>(
                &connector,
                &[addr],
                Some(local_ip),
            )
            .await;
            assert!(result.is_ok());
        });
    }

    #[cfg(feature = "compio")]
    #[test]
    fn local_connect_multi_with_local_address() {
        use crate::runtime::compio_rt::{CompioRuntime, TcpConnector};
        compio_runtime::Runtime::new().unwrap().block_on(async {
            let connector = TcpConnector;
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let good_addr = listener.local_addr().unwrap();
            let bad_addr: SocketAddr = "127.0.0.1:1".parse().unwrap();
            let local_ip: std::net::IpAddr = "127.0.0.1".parse().unwrap();
            let result = connect_happy_eyeballs_local::<CompioRuntime, TcpConnector>(
                &connector,
                &[bad_addr, good_addr],
                Some(local_ip),
            )
            .await;
            assert!(result.is_ok());
        });
    }

    // ── smol tests ────────────────────────────────────────────────────

    #[cfg(feature = "smol")]
    #[test]
    fn smol_connect_empty_addrs_errors() {
        use crate::runtime::smol_rt::{SmolRuntime, TcpConnector};
        smol::block_on(async {
            let connector = TcpConnector;
            let result =
                connect_happy_eyeballs::<SmolRuntime, TcpConnector>(&connector, &[], None).await;
            let err = result.err().expect("should be an error");
            assert_eq!(err.kind(), io::ErrorKind::AddrNotAvailable);
        });
    }

    #[cfg(feature = "smol")]
    #[test]
    fn smol_connect_single_addr_succeeds() {
        use crate::runtime::smol_rt::{SmolRuntime, TcpConnector};
        smol::block_on(async {
            let connector = TcpConnector;
            let listener = smol::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let (_stream, connected_addr) =
                connect_happy_eyeballs::<SmolRuntime, TcpConnector>(&connector, &[addr], None)
                    .await
                    .unwrap();
            assert_eq!(connected_addr, addr);
        });
    }

    #[cfg(feature = "smol")]
    #[test]
    fn smol_connect_multi_addrs_second_succeeds() {
        use crate::runtime::smol_rt::{SmolRuntime, TcpConnector};
        smol::block_on(async {
            let connector = TcpConnector;
            let listener = smol::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let good_addr = listener.local_addr().unwrap();
            let bad_addr: SocketAddr = "127.0.0.1:1".parse().unwrap();
            let (_stream, connected_addr) = connect_happy_eyeballs::<SmolRuntime, TcpConnector>(
                &connector,
                &[bad_addr, good_addr],
                None,
            )
            .await
            .unwrap();
            assert_eq!(connected_addr, good_addr);
        });
    }

    #[cfg(feature = "smol")]
    #[test]
    fn smol_connect_all_fail() {
        use crate::runtime::smol_rt::{SmolRuntime, TcpConnector};
        smol::block_on(async {
            let connector = TcpConnector;
            let bad1: SocketAddr = "127.0.0.1:1".parse().unwrap();
            let bad2: SocketAddr = "127.0.0.1:2".parse().unwrap();
            let result = connect_happy_eyeballs::<SmolRuntime, TcpConnector>(
                &connector,
                &[bad1, bad2],
                None,
            )
            .await;
            assert!(result.is_err());
        });
    }

    #[cfg(feature = "smol")]
    #[test]
    fn smol_connect_single_with_local_address() {
        use crate::runtime::smol_rt::{SmolRuntime, TcpConnector};
        smol::block_on(async {
            let connector = TcpConnector;
            let listener = smol::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let local_ip: std::net::IpAddr = "127.0.0.1".parse().unwrap();
            let result = connect_happy_eyeballs::<SmolRuntime, TcpConnector>(
                &connector,
                &[addr],
                Some(local_ip),
            )
            .await;
            assert!(result.is_ok());
        });
    }

    /// Tests the DeadlineReached path: first address is non-routable (hangs),
    /// so the happy-eyeballs timer fires and tries the second (good) address.
    #[cfg(feature = "tokio")]
    #[tokio::test]
    async fn connect_deadline_reached_then_second_succeeds() {
        use crate::runtime::tokio_rt::{TcpConnector, TokioRuntime};
        let connector = TcpConnector;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let good_addr = listener.local_addr().unwrap();
        // TEST-NET-1: non-routable, connection will hang (triggers DeadlineReached)
        let hanging_addr: SocketAddr = "192.0.2.1:80".parse().unwrap();
        let (stream, connected_addr) = connect_happy_eyeballs::<TokioRuntime, TcpConnector>(
            &connector,
            &[hanging_addr, good_addr],
            None,
        )
        .await
        .unwrap();
        assert_eq!(connected_addr, good_addr);
        drop(stream);
    }
}
