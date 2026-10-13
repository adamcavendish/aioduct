#![cfg(all(
    not(target_arch = "wasm32"),
    any(feature = "tokio", feature = "smol", feature = "compio")
))]
use aioduct::{
    ChannelDetachReason, ChannelHandoffMode, ChannelIoEvent, ConnectionAssociation, Interest,
    Observer, RequestFinish, RequestFinishReason, RequestInfo, RequestObservation,
    ResponseChannelHandoffEvent,
};
use std::{
    future::poll_fn,
    pin::Pin,
    sync::{Arc, Mutex},
    time::Duration,
};

#[derive(Debug, Clone, PartialEq, Eq)]
enum Fact {
    Started,
    Handoff(ChannelHandoffMode, Option<String>, ConnectionAssociation),
    Failed,
    Cancelled,
    HttpHandoff,
    HttpAbandoned,
    ReadEof(Option<u64>),
    Shutdown(Option<u64>),
    Detached(ChannelDetachReason, bool, bool, Option<u64>, Option<u64>),
}
#[derive(Clone)]
struct Recorder(Arc<Mutex<Vec<Fact>>>, Interest);
impl Observer for Recorder {
    type Context = ();
    type Observation = Self;
    type ConnectionObservation = aioduct::NoopConnectionObservation;
    fn begin_connection(&self) -> (Interest, Self::ConnectionObservation) {
        (Interest::EMPTY, aioduct::NoopConnectionObservation)
    }

    fn begin(&self, _: RequestInfo<'_>, _: Option<()>) -> (Interest, Self) {
        (self.1, self.clone())
    }
}

impl RequestObservation for Recorder {
    fn on_request_finish(&self, event: &RequestFinish<'_>) {
        let fact = match event.reason {
            RequestFinishReason::HandedOff => Fact::HttpHandoff,
            RequestFinishReason::ConsumerAbandoned => Fact::HttpAbandoned,
            _ => panic!("unexpected HTTP result: {:?}", event.reason),
        };
        self.0.lock().unwrap().push(fact);
    }

    fn on_response_channel_handoff(&self, event: &ResponseChannelHandoffEvent<'_>) {
        let fact = match event {
            ResponseChannelHandoffEvent::ChannelHandoffStarted { .. } => Fact::Started,
            ResponseChannelHandoffEvent::ResponseChannelHandoff {
                info, connection, ..
            } => Fact::Handoff(info.mode, info.protocol.map(str::to_owned), *connection),
            ResponseChannelHandoffEvent::ChannelHandoffFailed { .. } => Fact::Failed,
            ResponseChannelHandoffEvent::ChannelHandoffCancelled { .. } => Fact::Cancelled,
            _ => panic!("unexpected handoff fact"),
        };
        self.0.lock().unwrap().push(fact);
    }

    fn on_channel_io(&self, event: &ChannelIoEvent<'_>) {
        let fact = match event {
            ChannelIoEvent::ReadEof { read_bytes_so_far } => Fact::ReadEof(*read_bytes_so_far),
            ChannelIoEvent::WriteShutdown { write_bytes_so_far } => {
                Fact::Shutdown(*write_bytes_so_far)
            }
            ChannelIoEvent::Detached {
                reason,
                read_eof,
                write_shutdown,
                read_bytes_so_far,
                write_bytes_so_far,
            } => Fact::Detached(
                *reason,
                *read_eof,
                *write_shutdown,
                *read_bytes_so_far,
                *write_bytes_so_far,
            ),
            _ => panic!("unexpected channel fact: {event:?}"),
        };
        self.0.lock().unwrap().push(fact);
    }
}

fn recorder(interest: Interest) -> Recorder {
    Recorder(Arc::new(Mutex::new(Vec::new())), interest)
}

fn server(connect: bool) -> (String, std::thread::JoinHandle<Vec<u8>>) {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let thread = std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut header = Vec::new();
        let mut byte = [0];
        while !header.ends_with(b"\r\n\r\n") {
            socket.read_exact(&mut byte).unwrap();
            header.push(byte[0]);
        }
        assert_eq!(header.starts_with(b"CONNECT "), connect);
        let head: &[u8] = if connect {
            b"HTTP/1.1 200 Connection Established\r\n\r\n"
        } else {
            b"HTTP/1.1 101 Switching Protocols\r\nConnection: upgrade\r\nUpgrade: test\r\n\r\n"
        };
        socket.write_all(head).unwrap();
        socket.write_all(b"abc").unwrap();
        let mut received = Vec::new();
        let _ = socket.read_to_end(&mut received);
        received
    });
    (format!("http://{address}/"), thread)
}

async fn exchange<U: hyper::rt::Read + hyper::rt::Write + Unpin>(mut channel: U) {
    // Zero capacity cannot prove EOF, including with already buffered upgrade DATA.
    let mut empty = [];
    poll_fn(|cx| {
        Pin::new(&mut channel).poll_read(cx, hyper::rt::ReadBuf::new(&mut empty).unfilled())
    })
    .await
    .unwrap();
    let mut sent = 0;
    while sent < 3 {
        let n = poll_fn(|cx| Pin::new(&mut channel).poll_write(cx, &b"xyz"[sent..]))
            .await
            .unwrap();
        assert!(n > 0);
        sent += n;
    }
    poll_fn(|cx| Pin::new(&mut channel).poll_flush(cx))
        .await
        .unwrap();
    poll_fn(|cx| Pin::new(&mut channel).poll_shutdown(cx))
        .await
        .unwrap();
    let mut received = Vec::new();
    loop {
        let mut memory = [0; 16];
        let n = poll_fn(|cx| {
            let mut buf = hyper::rt::ReadBuf::new(&mut memory);
            match Pin::new(&mut channel).poll_read(cx, buf.unfilled()) {
                std::task::Poll::Ready(Ok(())) => std::task::Poll::Ready(Ok(buf.filled().len())),
                std::task::Poll::Ready(Err(error)) => std::task::Poll::Ready(Err(error)),
                std::task::Poll::Pending => std::task::Poll::Pending,
            }
        })
        .await
        .unwrap();
        if n == 0 {
            break;
        }
        received.extend_from_slice(&memory[..n]);
    }
    assert_eq!(received, b"abc");
}

fn verify(recorder: &Recorder, connect: bool) {
    let facts = recorder.0.lock().unwrap();
    let handoff = recorder.1.contains(Interest::RESPONSE_CHANNEL_HANDOFF);
    let lifecycle = recorder
        .1
        .intersects(Interest::CHANNEL_LIFECYCLE | Interest::CHANNEL_BYTES);
    let bytes = recorder.1.contains(Interest::CHANNEL_BYTES).then_some(3);
    if handoff {
        assert_eq!(
            facts
                .iter()
                .filter(|fact| matches!(fact, Fact::Started))
                .count(),
            1
        );
        let expected = if connect {
            ChannelHandoffMode::Http1Connect
        } else {
            ChannelHandoffMode::Http1Upgrade
        };
        assert_eq!(facts.iter().filter(|fact| matches!(fact, Fact::Handoff(mode, protocol, _)
            if *mode == expected && protocol.as_deref() == if connect { None } else { Some("test") })).count(), 1);
    } else {
        assert!(
            !facts
                .iter()
                .any(|fact| matches!(fact, Fact::Started | Fact::Handoff(..)))
        );
    }
    assert_eq!(facts.contains(&Fact::ReadEof(bytes)), lifecycle);
    assert_eq!(facts.contains(&Fact::Shutdown(bytes)), lifecycle);
    assert!(
        !facts
            .iter()
            .any(|fact| matches!(fact, Fact::Detached(..) | Fact::Failed | Fact::Cancelled))
    );
    assert_eq!(
        facts
            .iter()
            .filter(|fact| **fact == Fact::HttpHandoff)
            .count(),
        usize::from(recorder.1.contains(Interest::REQUEST_FINISH))
    );
    assert_eq!(
        facts.len(),
        usize::from(handoff) * 2
            + usize::from(lifecycle) * 2
            + usize::from(recorder.1.contains(Interest::REQUEST_FINISH))
    );
}

#[cfg(feature = "tokio")]
#[tokio::test]
async fn native_h1_upgrade_and_plain_connect_have_independent_lifecycles_and_interests() {
    for connect in [false, true] {
        for interest in [
            Interest::RESPONSE_CHANNEL_HANDOFF,
            Interest::CHANNEL_LIFECYCLE,
            Interest::CHANNEL_BYTES,
            Interest::RESPONSE_CHANNEL_HANDOFF | Interest::CHANNEL_BYTES | Interest::REQUEST_FINISH,
        ] {
            let recorder = recorder(interest);
            let client = aioduct::TokioClient::builder()
                .observer(recorder.clone())
                .build()
                .unwrap();
            let (url, server) = server(connect);
            let mut request = client
                .request(
                    if connect {
                        http::Method::CONNECT
                    } else {
                        http::Method::GET
                    },
                    &url,
                )
                .unwrap();
            if !connect {
                request = request
                    .header_str("connection", "upgrade")
                    .unwrap()
                    .header_str("upgrade", "test")
                    .unwrap();
            }
            let response = request.send().await.unwrap();
            assert!(recorder.0.lock().unwrap().is_empty());
            exchange(response.upgrade().await.unwrap()).await;
            assert_eq!(server.join().unwrap(), b"xyz");
            verify(&recorder, connect);
        }
    }
}

#[cfg(feature = "smol")]
#[test]
fn smol_handoff_and_channel_bytes() {
    smol::block_on(async {
        let recorder = recorder(
            Interest::RESPONSE_CHANNEL_HANDOFF | Interest::CHANNEL_BYTES | Interest::REQUEST_FINISH,
        );
        let client = aioduct::SmolClient::builder()
            .observer(recorder.clone())
            .build()
            .unwrap();
        let (url, server) = server(false);
        let response = client
            .get(&url)
            .unwrap()
            .header_str("connection", "upgrade")
            .unwrap()
            .header_str("upgrade", "test")
            .unwrap()
            .send()
            .await
            .unwrap();
        exchange(response.upgrade().await.unwrap()).await;
        assert_eq!(server.join().unwrap(), b"xyz");
        verify(&recorder, false);
    });
}

#[cfg(feature = "compio")]
#[test]
fn compio_handoff_counts_prefetched_bytes_once_when_delivered() {
    compio_runtime::Runtime::new().unwrap().block_on(async {
        let recorder = recorder(
            Interest::RESPONSE_CHANNEL_HANDOFF | Interest::CHANNEL_BYTES | Interest::REQUEST_FINISH,
        );
        let client = aioduct::CompioClient::builder()
            .observer(recorder.clone())
            .build_local()
            .unwrap();
        let (url, server) = server(false);
        let response = client
            .get_local(&url)
            .unwrap()
            .header_str("connection", "upgrade")
            .unwrap()
            .header_str("upgrade", "test")
            .unwrap()
            .send()
            .await
            .unwrap();
        exchange(response.upgrade().await.unwrap()).await;
        assert_eq!(server.join().unwrap(), b"xyz");
        verify(&recorder, false);
    });
}

#[cfg(feature = "tokio")]
#[tokio::test]
async fn raw_extraction_detaches_channel_without_changing_http_result() {
    let recorder = recorder(
        Interest::RESPONSE_CHANNEL_HANDOFF | Interest::CHANNEL_BYTES | Interest::REQUEST_FINISH,
    );
    let client = aioduct::TokioClient::builder()
        .observer(recorder.clone())
        .build()
        .unwrap();
    let (url, server) = server(false);
    let response = client
        .get(&url)
        .unwrap()
        .header_str("connection", "upgrade")
        .unwrap()
        .header_str("upgrade", "test")
        .unwrap()
        .send()
        .await
        .unwrap();
    let channel = response.upgrade().await.unwrap();
    let raw = channel.into_inner();
    assert_eq!(
        recorder.0.lock().unwrap().last(),
        Some(&Fact::Detached(
            ChannelDetachReason::IntoInner,
            false,
            false,
            Some(0),
            Some(0)
        ))
    );
    exchange(raw).await;
    assert_eq!(server.join().unwrap(), b"xyz");
    assert_eq!(recorder.0.lock().unwrap().len(), 4);
}

#[cfg(feature = "tokio")]
#[tokio::test]
async fn h2_plain_connect_streams_share_the_actual_connection_association() {
    use bytes::Bytes;
    use http_body_util::Full;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let connections = Arc::new(AtomicUsize::new(0));
    let count = connections.clone();
    let server = tokio::spawn(async move {
        loop {
            let (socket, _) = listener.accept().await.unwrap();
            count.fetch_add(1, Ordering::Relaxed);
            tokio::spawn(async move {
                let _ = hyper::server::conn::http2::Builder::new(aioduct_test_server::TokioExec)
                    .enable_connect_protocol()
                    .serve_connection(
                        aioduct::runtime::tokio_rt::TokioIo::new(socket),
                        hyper::service::service_fn(
                            |mut request: http::Request<hyper::body::Incoming>| async move {
                                if request.method() == http::Method::CONNECT {
                                    assert!(
                                        request
                                            .extensions()
                                            .get::<hyper::ext::Protocol>()
                                            .is_none()
                                    );
                                    tokio::spawn(async move {
                                        let raw = hyper::upgrade::on(&mut request).await.unwrap();
                                        let mut channel = aioduct::UpgradedSend::from(raw);
                                        channel.write_all(b"abc").await.unwrap();
                                        let mut received = Vec::new();
                                        channel.read_to_end(&mut received).await.unwrap();
                                        assert_eq!(received, b"xyz");
                                    });
                                    Ok::<_, std::convert::Infallible>(http::Response::new(
                                        Full::new(Bytes::new()),
                                    ))
                                } else {
                                    Ok(http::Response::new(Full::new(Bytes::from_static(
                                        b"ordinary",
                                    ))))
                                }
                            },
                        ),
                    )
                    .await;
            });
        }
    });
    let recorder = recorder(
        Interest::RESPONSE_CHANNEL_HANDOFF | Interest::CHANNEL_BYTES | Interest::REQUEST_FINISH,
    );
    let client = aioduct::TokioClient::builder()
        .observer(recorder.clone())
        .build()
        .unwrap();
    let url = format!("http://{address}/");
    let mut channels = Vec::new();
    for _ in 0..2 {
        let response = client
            .request(http::Method::CONNECT, &url)
            .unwrap()
            .h2c_prior_knowledge()
            .send()
            .await
            .unwrap();
        assert_eq!(response.version(), http::Version::HTTP_2);
        channels.push(response.upgrade().await.unwrap());
    }
    let ordinary = client
        .get(&url)
        .unwrap()
        .h2c_prior_knowledge()
        .disable_observation()
        .send()
        .await
        .unwrap();
    assert_eq!(ordinary.bytes().await.unwrap(), b"ordinary"[..]);
    for channel in channels {
        exchange(channel).await;
    }
    let facts = recorder.0.lock().unwrap();
    let identities: Vec<_> = facts
        .iter()
        .filter_map(|fact| match fact {
            Fact::Handoff(ChannelHandoffMode::Http2Connect, None, identity) => Some(*identity),
            _ => None,
        })
        .collect();
    assert_eq!(identities.len(), 2);
    assert_eq!(identities[0], identities[1]);
    assert_eq!(connections.load(Ordering::Relaxed), 1);
    assert_eq!(
        facts
            .iter()
            .filter(|fact| **fact == Fact::HttpHandoff)
            .count(),
        2
    );
    assert_eq!(
        facts
            .iter()
            .filter(|fact| **fact == Fact::ReadEof(Some(3)))
            .count(),
        2
    );
    assert_eq!(
        facts
            .iter()
            .filter(|fact| **fact == Fact::Shutdown(Some(3)))
            .count(),
        2
    );
    assert!(!facts.iter().any(|fact| matches!(fact, Fact::Detached(..))));
    server.abort();
}

#[cfg(feature = "tokio")]
#[tokio::test]
async fn dropping_an_unopened_handoff_does_not_invent_started_or_cancelled() {
    let recorder = recorder(
        Interest::RESPONSE_CHANNEL_HANDOFF | Interest::CHANNEL_BYTES | Interest::REQUEST_FINISH,
    );
    let client = aioduct::TokioClient::builder()
        .observer(recorder.clone())
        .build()
        .unwrap();
    let (url, server) = server(false);
    let response = client
        .get(&url)
        .unwrap()
        .header_str("connection", "upgrade")
        .unwrap()
        .header_str("upgrade", "test")
        .unwrap()
        .send()
        .await
        .unwrap();
    drop(response);
    assert_eq!(*recorder.0.lock().unwrap(), vec![Fact::HttpAbandoned]);
    assert!(server.join().unwrap().is_empty());
}

#[cfg(feature = "tokio")]
#[tokio::test]
async fn disabled_channel_has_no_request_or_channel_callbacks() {
    let recorder = recorder(
        Interest::RESPONSE_CHANNEL_HANDOFF | Interest::CHANNEL_BYTES | Interest::REQUEST_FINISH,
    );
    let client = aioduct::TokioClient::builder()
        .observer(recorder.clone())
        .build()
        .unwrap();
    let (url, server) = server(false);
    let response = client
        .get(&url)
        .unwrap()
        .header_str("connection", "upgrade")
        .unwrap()
        .header_str("upgrade", "test")
        .unwrap()
        .disable_observation()
        .send()
        .await
        .unwrap();
    exchange(response.upgrade().await.unwrap()).await;
    assert_eq!(server.join().unwrap(), b"xyz");
    assert!(recorder.0.lock().unwrap().is_empty());
}
