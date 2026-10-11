use super::*;
use std::sync::Mutex;

#[derive(Debug, Clone, PartialEq, Eq)]
enum Fact {
    Eof(Option<u64>),
    Shutdown(Option<u64>),
    Error(ChannelIoOperation, usize, Option<u64>, Option<u64>),
    Detached(ChannelDetachReason, bool, bool, Option<u64>, Option<u64>),
    Started,
    Succeeded(ChannelHandoffMode, Option<String>, ConnectionAssociation),
    Failed,
    Cancelled,
}
#[derive(Clone)]
struct Recorder(Arc<Mutex<Vec<Fact>>>, Interest);
impl Receiver for Recorder {
    fn interest(&self) -> Interest {
        self.1
    }
    fn handoff(&self, event: &ResponseChannelHandoffEvent<'_>) {
        let fact = match event {
            ResponseChannelHandoffEvent::ChannelHandoffStarted { .. } => Fact::Started,
            ResponseChannelHandoffEvent::ResponseChannelHandoff {
                info, connection, ..
            } => Fact::Succeeded(info.mode, info.protocol.map(str::to_owned), *connection),
            ResponseChannelHandoffEvent::ChannelHandoffFailed { .. } => Fact::Failed,
            ResponseChannelHandoffEvent::ChannelHandoffCancelled { .. } => Fact::Cancelled,
        };
        self.0.lock().unwrap().push(fact);
    }
    fn io(&self, event: &ChannelIoEvent<'_>) {
        let fact = match event {
            ChannelIoEvent::ReadEof { read_bytes_so_far } => Fact::Eof(*read_bytes_so_far),
            ChannelIoEvent::WriteShutdown { write_bytes_so_far } => {
                Fact::Shutdown(*write_bytes_so_far)
            }
            ChannelIoEvent::IoError {
                operation,
                error,
                read_bytes_so_far,
                write_bytes_so_far,
            } => {
                let pointer = match error {
                    Error::Io(error) => error
                        .get_ref()
                        .map_or(0, |source| std::ptr::from_ref(source) as *const () as usize),
                    _ => 0,
                };
                Fact::Error(*operation, pointer, *read_bytes_so_far, *write_bytes_so_far)
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
        };
        self.0.lock().unwrap().push(fact);
    }
}
fn recorder(interest: Interest) -> Recorder {
    Recorder(Arc::new(Mutex::new(Vec::new())), interest)
}

#[test]
fn directional_state_and_bytes_are_independent_and_low_frequency() {
    let recorder = recorder(Interest::CHANNEL_LIFECYCLE | Interest::CHANNEL_BYTES);
    let mut channel = Channel::new(&recorder);
    channel.read(0, 0); // Zero capacity is not EOF.
    channel.read(9, 3);
    channel.written(2); // A partial write only accepts two bytes.
    assert!(recorder.0.lock().unwrap().is_empty());
    channel.shutdown();
    channel.shutdown();
    channel.read(9, 1); // The other direction remains active.
    channel.read(9, 0);
    channel.read(9, 0);
    drop(channel);
    assert_eq!(
        *recorder.0.lock().unwrap(),
        vec![Fact::Shutdown(Some(2)), Fact::Eof(Some(4))]
    );
}

#[derive(Debug)]
struct Marker(u64);
impl std::fmt::Display for Marker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}
impl std::error::Error for Marker {}

#[test]
fn original_io_error_is_borrowed_and_recoverable_without_direction_termination() {
    let recorder = recorder(Interest::CHANNEL_LIFECYCLE | Interest::CHANNEL_BYTES);
    let mut channel = Some(Box::new(Channel::new(&recorder)));
    let error = std::io::Error::other(Marker(7));
    let pointer = std::ptr::from_ref(error.get_ref().unwrap()) as *const () as usize;
    let returned = io_error::<(), _>(&channel, ChannelIoOperation::Read, error).unwrap_err();
    assert_eq!(
        std::ptr::from_ref(returned.get_ref().unwrap()) as *const () as usize,
        pointer
    );
    let channel_ref = channel.as_mut().unwrap();
    channel_ref.read(5, 2);
    channel_ref.written(3);
    channel_ref.detach(ChannelDetachReason::IntoInner);
    drop(channel);
    assert_eq!(
        *recorder.0.lock().unwrap(),
        vec![
            Fact::Error(ChannelIoOperation::Read, pointer, Some(0), Some(0)),
            Fact::Detached(
                ChannelDetachReason::IntoInner,
                false,
                false,
                Some(2),
                Some(3)
            ),
        ]
    );
}

#[test]
fn lifecycle_only_is_uncollected_and_drop_is_not_resource_close() {
    let recorder = recorder(Interest::CHANNEL_LIFECYCLE);
    let mut channel = Channel::new(&recorder);
    channel.read(3, 2);
    channel.written(1);
    channel.read(3, 0);
    drop(channel);
    assert_eq!(
        *recorder.0.lock().unwrap(),
        vec![
            Fact::Eof(None),
            Fact::Detached(ChannelDetachReason::Dropped, true, false, None, None)
        ]
    );
}

#[test]
fn counters_saturate_without_affecting_direction_state() {
    let recorder = recorder(Interest::CHANNEL_BYTES);
    let mut channel = Channel::new(&recorder);
    channel.bytes = Some(Box::new((u64::MAX - 1, u64::MAX - 1)));
    channel.read(4, 4);
    channel.written(4);
    channel.read(4, 0);
    channel.shutdown();
    assert_eq!(
        *recorder.0.lock().unwrap(),
        vec![Fact::Eof(Some(u64::MAX)), Fact::Shutdown(Some(u64::MAX))]
    );
}

#[test]
fn handoff_result_and_cancel_are_mutually_exclusive() {
    for result in 0..3 {
        let recorder = recorder(Interest::RESPONSE_CHANNEL_HANDOFF);
        let metadata = HandoffMetadata {
            mode: ChannelHandoffMode::Http1Connect,
            protocol: None,
            status: http::StatusCode::OK,
            connection: Some(ConnectionAssociation::allocate()),
        };
        let mut handoff = Handoff::start(recorder.clone(), metadata);
        match result {
            0 => {
                drop(handoff);
                assert_eq!(
                    *recorder.0.lock().unwrap(),
                    vec![Fact::Started, Fact::Cancelled]
                );
            }
            1 => {
                handoff.failed(&Error::Io(std::io::Error::other(Marker(9))));
                drop(handoff);
                assert_eq!(
                    *recorder.0.lock().unwrap(),
                    vec![Fact::Started, Fact::Failed]
                );
            }
            _ => {
                assert!(handoff.succeeded().is_none());
                assert!(matches!(
                    recorder.0.lock().unwrap().as_slice(),
                    [
                        Fact::Started,
                        Fact::Succeeded(ChannelHandoffMode::Http1Connect, None, _)
                    ]
                ));
            }
        }
    }
}

#[test]
fn channel_lifecycle_only_does_not_require_handoff_association() {
    let recorder = recorder(Interest::CHANNEL_LIFECYCLE);
    let metadata = HandoffMetadata {
        mode: ChannelHandoffMode::Http2Connect,
        protocol: None,
        status: http::StatusCode::OK,
        connection: None,
    };
    let handoff = Handoff::start(recorder.clone(), metadata);
    let channel = handoff.succeeded().expect("channel lifecycle is selected");
    drop(channel);
    assert_eq!(recorder.0.lock().unwrap().len(), 1);
    assert!(matches!(
        recorder.0.lock().unwrap().first(),
        Some(Fact::Detached(
            ChannelDetachReason::Dropped,
            false,
            false,
            None,
            None
        ))
    ));
}

#[test]
fn terminal_callback_panic_does_not_generate_a_second_detach() {
    #[derive(Clone)]
    struct Panicking(Arc<Mutex<usize>>);
    impl Receiver for Panicking {
        fn interest(&self) -> Interest {
            Interest::CHANNEL_LIFECYCLE
        }
        fn handoff(&self, _: &ResponseChannelHandoffEvent<'_>) {}
        fn io(&self, _: &ChannelIoEvent<'_>) {
            *self.0.lock().unwrap() += 1;
            panic!("exporter panic");
        }
    }
    let calls = Arc::new(Mutex::new(0));
    let mut channel = Channel::new(&Panicking(calls.clone()));
    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        channel.detach(ChannelDetachReason::IntoInner)
    }));
    assert!(panic.is_err());
    drop(channel);
    assert_eq!(*calls.lock().unwrap(), 1);
}

#[test]
fn accepted_wire_modes_are_not_inferred_from_status_alone() {
    let headers = http::HeaderMap::new();
    let cases = [
        (
            http::Version::HTTP_11,
            http::StatusCode::SWITCHING_PROTOCOLS,
            http::Method::GET,
            true,
            false,
            Some(ChannelHandoffMode::Http1Upgrade),
        ),
        (
            http::Version::HTTP_11,
            http::StatusCode::OK,
            http::Method::CONNECT,
            false,
            false,
            Some(ChannelHandoffMode::Http1Connect),
        ),
        (
            http::Version::HTTP_2,
            http::StatusCode::OK,
            http::Method::CONNECT,
            false,
            false,
            Some(ChannelHandoffMode::Http2Connect),
        ),
        (
            http::Version::HTTP_2,
            http::StatusCode::OK,
            http::Method::CONNECT,
            false,
            true,
            Some(ChannelHandoffMode::Http2ExtendedConnect),
        ),
        (
            http::Version::HTTP_11,
            http::StatusCode::SWITCHING_PROTOCOLS,
            http::Method::GET,
            false,
            false,
            None,
        ),
        (
            http::Version::HTTP_2,
            http::StatusCode::FORBIDDEN,
            http::Method::CONNECT,
            false,
            true,
            None,
        ),
        (
            http::Version::HTTP_2,
            http::StatusCode::OK,
            http::Method::GET,
            false,
            false,
            None,
        ),
        (
            http::Version::HTTP_3,
            http::StatusCode::OK,
            http::Method::CONNECT,
            false,
            true,
            None,
        ),
    ];
    for (version, status, method, upgrade_offered, extended, expected) in cases {
        let request = HandoffRequest {
            method,
            upgrade_offered,
            association_requested: true,
            protocol: extended.then(|| hyper::ext::Protocol::from_static("websocket")),
        };
        let metadata = HandoffMetadata::selected(
            &request,
            version,
            status,
            &headers,
            ConnectionAssociation::allocate,
        );
        assert_eq!(metadata.as_ref().map(|metadata| metadata.mode), expected);
        if extended && let Some(metadata) = metadata {
            assert_eq!(metadata.info().protocol, Some("websocket"));
        }
    }
    let request = HandoffRequest {
        method: http::Method::CONNECT,
        upgrade_offered: false,
        protocol: None,
        association_requested: false,
    };
    let metadata = HandoffMetadata::selected(
        &request,
        http::Version::HTTP_11,
        http::StatusCode::OK,
        &headers,
        || panic!("channel-only must not allocate a connection identity"),
    )
    .unwrap();
    assert!(metadata.connection.is_none());
}
