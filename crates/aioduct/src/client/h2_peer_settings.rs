use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use futures_channel::oneshot;
use hyper::rt::{Read, ReadBuf, ReadBufCursor, Write};

const FRAME_HEADER_LEN: usize = 9;
const SETTING_LEN: usize = 6;
const SETTINGS_FRAME_TYPE: u8 = 0x04;
const SETTINGS_ACK_FLAG: u8 = 0x01;
const SETTINGS_ENABLE_PUSH: u16 = 0x02;
const SETTINGS_INITIAL_WINDOW_SIZE: u16 = 0x04;
const SETTINGS_MAX_FRAME_SIZE: u16 = 0x05;
const SETTINGS_ENABLE_CONNECT_PROTOCOL: u16 = 0x08;
const MAX_FLOW_CONTROL_WINDOW: u32 = 0x7fff_ffff;
const OBSERVATION_BUFFER_SIZE: usize = 8_192;
const CONFIRMATION_TIMEOUT: Duration = Duration::from_millis(200);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum H2PeerSettingsRequirement {
    NotRequired,
    Required,
}

impl H2PeerSettingsRequirement {
    pub(crate) fn is_required(self) -> bool {
        self == Self::Required
    }
}

/// Completion signal for the server's HTTP/2 connection preface.
///
/// RFC 9113 Section 3.4 requires the server's first frame to be a SETTINGS
/// frame. Hyper's client handshake can finish before that frame arrives, so
/// adaptive h2c discovery observes the transport until the peer has actually
/// supplied the complete frame.
pub(crate) struct H2PeerSettingsConfirmation {
    receiver: oneshot::Receiver<bool>,
}

impl H2PeerSettingsConfirmation {
    pub(crate) async fn confirmed(self) -> bool {
        self.receiver.await.unwrap_or(false)
    }

    pub(crate) async fn confirmed_within<R: crate::runtime::RuntimeCompletion>(self) -> bool {
        let confirmation = async { Ok::<_, crate::error::Error>(self.confirmed().await) };
        crate::timeout::Timeout::WithTimeout {
            future: confirmation,
            sleep: R::sleep(CONFIRMATION_TIMEOUT),
        }
        .await
        .unwrap_or(false)
    }
}

pub(crate) fn observe_h2_peer_settings<S>(
    stream: S,
    receive_max_frame_size: usize,
) -> (H2PeerSettingsIo<S>, H2PeerSettingsConfirmation) {
    let (sender, receiver) = oneshot::channel();
    (
        H2PeerSettingsIo {
            stream,
            detector: SettingsFrameDetector::new(receive_max_frame_size),
            sender: Some(sender),
            scratch: Some(vec![0; OBSERVATION_BUFFER_SIZE].into_boxed_slice()),
        },
        H2PeerSettingsConfirmation { receiver },
    )
}

pub(crate) struct H2PeerSettingsIo<S> {
    stream: S,
    detector: SettingsFrameDetector,
    sender: Option<oneshot::Sender<bool>>,
    scratch: Option<Box<[u8]>>,
}

impl<S> H2PeerSettingsIo<S> {
    fn finish_observation(&mut self, confirmed: bool) {
        if let Some(sender) = self.sender.take() {
            let _ = sender.send(confirmed);
        }
        self.scratch = None;
    }
}

impl<S> Drop for H2PeerSettingsIo<S> {
    fn drop(&mut self) {
        self.finish_observation(false);
    }
}

impl<S> Read for H2PeerSettingsIo<S>
where
    S: Read + Unpin,
{
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        mut destination: ReadBufCursor<'_>,
    ) -> Poll<Result<(), std::io::Error>> {
        let this = self.get_mut();
        if this.sender.is_none() {
            return Pin::new(&mut this.stream).poll_read(cx, destination);
        }

        let Some(scratch_len) = this.scratch.as_ref().map(|scratch| scratch.len()) else {
            return Pin::new(&mut this.stream).poll_read(cx, destination);
        };
        let capacity = destination.remaining().min(scratch_len);
        if capacity == 0 {
            return Poll::Ready(Ok(()));
        }

        let poll = {
            let Some(scratch) = this.scratch.as_mut() else {
                return Pin::new(&mut this.stream).poll_read(cx, destination);
            };
            let mut source = ReadBuf::new(&mut scratch[..capacity]);
            match Pin::new(&mut this.stream).poll_read(cx, source.unfilled()) {
                Poll::Ready(Ok(())) => Poll::Ready(Ok(source.filled().len())),
                Poll::Ready(Err(error)) => Poll::Ready(Err(error)),
                Poll::Pending => Poll::Pending,
            }
        };

        match poll {
            Poll::Ready(Ok(0)) => {
                this.finish_observation(false);
                Poll::Ready(Ok(()))
            }
            Poll::Ready(Ok(length)) => {
                let Some(scratch) = this.scratch.as_ref() else {
                    this.finish_observation(false);
                    return Poll::Ready(Err(std::io::Error::other(
                        "HTTP/2 SETTINGS observation buffer was lost",
                    )));
                };
                let outcome = {
                    let bytes = &scratch[..length];
                    destination.put_slice(bytes);
                    this.detector.observe(bytes)
                };
                if let Some(confirmed) = outcome {
                    this.finish_observation(confirmed);
                }
                Poll::Ready(Ok(()))
            }
            Poll::Ready(Err(error)) => {
                this.finish_observation(false);
                Poll::Ready(Err(error))
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

impl<S> Write for H2PeerSettingsIo<S>
where
    S: Write + Unpin,
{
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buffer: &[u8],
    ) -> Poll<Result<usize, std::io::Error>> {
        Pin::new(&mut self.get_mut().stream).poll_write(cx, buffer)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), std::io::Error>> {
        Pin::new(&mut self.get_mut().stream).poll_flush(cx)
    }

    fn poll_shutdown(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Result<(), std::io::Error>> {
        Pin::new(&mut self.get_mut().stream).poll_shutdown(cx)
    }

    fn is_write_vectored(&self) -> bool {
        self.stream.is_write_vectored()
    }

    fn poll_write_vectored(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buffers: &[std::io::IoSlice<'_>],
    ) -> Poll<Result<usize, std::io::Error>> {
        Pin::new(&mut self.get_mut().stream).poll_write_vectored(cx, buffers)
    }
}

struct SettingsFrameDetector {
    header: [u8; FRAME_HEADER_LEN],
    header_len: usize,
    payload_remaining: Option<usize>,
    setting: [u8; SETTING_LEN],
    setting_len: usize,
    enable_connect_protocol: bool,
    receive_max_frame_size: usize,
}

impl SettingsFrameDetector {
    fn new(receive_max_frame_size: usize) -> Self {
        Self {
            header: [0; FRAME_HEADER_LEN],
            header_len: 0,
            payload_remaining: None,
            setting: [0; SETTING_LEN],
            setting_len: 0,
            enable_connect_protocol: false,
            receive_max_frame_size,
        }
    }

    fn observe(&mut self, mut bytes: &[u8]) -> Option<bool> {
        if self.payload_remaining.is_none() {
            let header_remaining = FRAME_HEADER_LEN - self.header_len;
            let copied = header_remaining.min(bytes.len());
            self.header[self.header_len..self.header_len + copied]
                .copy_from_slice(&bytes[..copied]);
            self.header_len += copied;
            bytes = &bytes[copied..];
            if self.header_len < FRAME_HEADER_LEN {
                return None;
            }

            let payload_len = usize::from(self.header[0]) << 16
                | usize::from(self.header[1]) << 8
                | usize::from(self.header[2]);
            let stream_id = u32::from_be_bytes([
                self.header[5],
                self.header[6],
                self.header[7],
                self.header[8],
            ]) & 0x7fff_ffff;
            let valid = self.header[3] == SETTINGS_FRAME_TYPE
                && self.header[4] & SETTINGS_ACK_FLAG == 0
                && stream_id == 0
                && payload_len <= self.receive_max_frame_size
                && payload_len.is_multiple_of(6);
            if !valid {
                return Some(false);
            }
            self.payload_remaining = Some(payload_len);
        }

        let remaining = self.payload_remaining?;
        let consumed = bytes.len().min(remaining);
        for byte in &bytes[..consumed] {
            self.setting[self.setting_len] = *byte;
            self.setting_len += 1;
            if self.setting_len == SETTING_LEN {
                if !self.valid_server_setting() {
                    return Some(false);
                }
                self.setting_len = 0;
            }
        }

        let remaining = remaining - consumed;
        self.payload_remaining = Some(remaining);
        if remaining == 0 {
            debug_assert_eq!(self.setting_len, 0);
            Some(true)
        } else {
            None
        }
    }

    fn valid_server_setting(&mut self) -> bool {
        let identifier = u16::from_be_bytes([self.setting[0], self.setting[1]]);
        let value = u32::from_be_bytes([
            self.setting[2],
            self.setting[3],
            self.setting[4],
            self.setting[5],
        ]);
        match identifier {
            // RFC 9113 Section 6.5.2 permits a server to omit ENABLE_PUSH or
            // explicitly send 0, but a client must reject any value of 1.
            SETTINGS_ENABLE_PUSH => value == 0,
            SETTINGS_INITIAL_WINDOW_SIZE => value <= MAX_FLOW_CONTROL_WINDOW,
            SETTINGS_MAX_FRAME_SIZE => (crate::http2::Http2Config::MIN_MAX_FRAME_SIZE
                ..=crate::http2::Http2Config::MAX_MAX_FRAME_SIZE)
                .contains(&value),
            // RFC 8441 Section 3 restricts ENABLE_CONNECT_PROTOCOL to 0 or 1
            // and forbids reverting to 0 after 1 has been observed. The
            // initial frame can contain duplicate settings, so retain this
            // connection-level state while processing every occurrence.
            SETTINGS_ENABLE_CONNECT_PROTOCOL => match value {
                0 => !self.enable_connect_protocol,
                1 => {
                    self.enable_connect_protocol = true;
                    true
                }
                _ => false,
            },
            // The remaining RFC 9113 settings accept the full u32 value range;
            // unknown extension settings are ignored. Duplicate identifiers
            // are processed in wire order, so every occurrence is validated.
            _ => true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::SettingsFrameDetector;

    fn settings_frame(settings: &[(u16, u32)]) -> Vec<u8> {
        let payload_len = settings.len() * 6;
        let mut frame = vec![0_u8; 9 + payload_len];
        frame[..3].copy_from_slice(&[
            ((payload_len >> 16) & 0xff) as u8,
            ((payload_len >> 8) & 0xff) as u8,
            (payload_len & 0xff) as u8,
        ]);
        frame[3] = 0x04;
        for (chunk, (identifier, value)) in frame[9..].chunks_exact_mut(6).zip(settings) {
            chunk[..2].copy_from_slice(&identifier.to_be_bytes());
            chunk[2..].copy_from_slice(&value.to_be_bytes());
        }
        frame
    }

    fn detector() -> SettingsFrameDetector {
        SettingsFrameDetector::new(crate::http2::Http2Config::DEFAULT_MAX_FRAME_SIZE)
    }

    #[test]
    fn confirms_only_after_the_complete_initial_settings_frame() {
        let mut detector = detector();
        assert_eq!(detector.observe(&[0, 0, 6, 4]), None);
        assert_eq!(detector.observe(&[0, 0, 0, 0, 0, 0, 1]), None);
        assert_eq!(detector.observe(&[0, 0, 0, 100]), Some(true));
    }

    #[test]
    fn rejects_an_http1_response_head() {
        let mut detector = detector();
        assert_eq!(
            detector.observe(b"HTTP/1.1 400 Bad Request\r\n"),
            Some(false)
        );
    }

    #[test]
    fn rejects_non_settings_and_acknowledgement_prefaces() {
        let mut data = [0; 9];
        data[3] = 1;
        assert_eq!(detector().observe(&data), Some(false));

        data[3] = 4;
        data[4] = 1;
        assert_eq!(detector().observe(&data), Some(false));
    }

    #[test]
    fn rejects_invalid_settings_frame_shape() {
        let mut data = [0; 9];
        data[2] = 1;
        data[3] = 4;
        assert_eq!(detector().observe(&data), Some(false));

        data[2] = 0;
        data[8] = 1;
        assert_eq!(detector().observe(&data), Some(false));
    }

    #[test]
    fn accepts_a_fragmented_settings_frame_up_to_the_configured_receive_limit() {
        let payload_len = 16_386usize;
        let mut frame = vec![0_u8; 9 + payload_len];
        frame[..3].copy_from_slice(&[0x00, 0x40, 0x02]);
        frame[3] = 0x04;

        let mut detector = SettingsFrameDetector::new(32_768);
        for chunk in frame.chunks(997).take(frame.len().div_ceil(997) - 1) {
            assert_eq!(detector.observe(chunk), None);
        }
        let consumed = (frame.len().div_ceil(997) - 1) * 997;
        assert_eq!(detector.observe(&frame[consumed..]), Some(true));
    }

    #[test]
    fn rejects_a_settings_frame_above_the_configured_receive_limit() {
        let mut header = [0_u8; 9];
        header[..3].copy_from_slice(&[0x00, 0x40, 0x02]);
        header[3] = 0x04;

        assert_eq!(detector().observe(&header), Some(false));
    }

    #[test]
    fn validates_every_constrained_server_setting() {
        for invalid in [
            (0x02, 1),
            (0x02, 2),
            (0x04, 0x8000_0000),
            (0x05, 16_383),
            (0x05, 16_777_216),
            (0x08, 2),
        ] {
            assert_eq!(
                detector().observe(&settings_frame(&[invalid])),
                Some(false),
                "setting {invalid:?} must be rejected"
            );
        }

        assert_eq!(
            detector().observe(&settings_frame(&[
                (0x01, u32::MAX),
                (0x02, 0),
                (0x03, u32::MAX),
                (0x04, 0x7fff_ffff),
                (0x05, 16_384),
                (0x05, 16_777_215),
                (0x06, u32::MAX),
                (0x08, 0),
                (0x08, 1),
                (0x08, 1),
                (0xffff, u32::MAX),
            ])),
            Some(true)
        );
    }

    #[test]
    fn duplicate_settings_are_processed_in_wire_order() {
        assert_eq!(
            detector().observe(&settings_frame(&[
                (0x04, 65_535),
                (0x04, 1_048_576),
                (0x05, 16_384),
                (0x05, 32_768),
            ])),
            Some(true)
        );
        assert_eq!(
            detector().observe(&settings_frame(&[(0x05, 16_383), (0x05, 16_384),])),
            Some(false),
            "a later valid duplicate cannot repair a connection error"
        );
        assert_eq!(
            detector().observe(&settings_frame(&[(0x02, 0), (0x02, 1)])),
            Some(false),
            "the final duplicate must also be validated"
        );
        assert_eq!(
            detector().observe(&settings_frame(&[(0x08, 0), (0x08, 1)])),
            Some(true),
            "ENABLE_CONNECT_PROTOCOL may transition from 0 to 1"
        );
        assert_eq!(
            detector().observe(&settings_frame(&[(0x08, 1), (0x08, 0)])),
            Some(false),
            "ENABLE_CONNECT_PROTOCOL must not transition from 1 to 0"
        );
    }

    #[test]
    fn validates_fragmented_setting_values_before_confirmation() {
        let frame = settings_frame(&[(0x04, 0x8000_0000)]);
        let mut detector = detector();
        for byte in &frame[..frame.len() - 1] {
            assert_eq!(detector.observe(std::slice::from_ref(byte)), None);
        }
        assert_eq!(detector.observe(&frame[frame.len() - 1..]), Some(false));
    }
}
