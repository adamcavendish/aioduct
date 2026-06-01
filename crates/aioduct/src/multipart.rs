use bytes::{BufMut, Bytes, BytesMut};

/// Builder for multipart/form-data request bodies.
pub struct Multipart {
    boundary: String,
    parts: Vec<Part>,
}

/// A single part in a multipart body.
pub struct Part {
    name: String,
    filename: Option<String>,
    content_type: Option<String>,
    headers: Vec<(String, String)>,
    body: PartBody,
}

enum PartBody {
    Buffered(Bytes),
    Streaming(crate::body::RequestBodySend),
}

impl std::fmt::Debug for Multipart {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Multipart").finish()
    }
}

impl std::fmt::Debug for Part {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Part").field("name", &self.name).finish()
    }
}

impl Part {
    /// Create a new part with the given field name and text body.
    pub fn text(name: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            filename: None,
            content_type: None,
            headers: Vec::new(),
            body: PartBody::Buffered(Bytes::from(value.into())),
        }
    }

    /// Create a new part with the given field name and bytes body.
    pub fn bytes(name: impl Into<String>, data: impl Into<Bytes>) -> Self {
        Self {
            name: name.into(),
            filename: None,
            content_type: None,
            headers: Vec::new(),
            body: PartBody::Buffered(data.into()),
        }
    }

    /// Create a new part with a streaming body.
    pub fn stream(name: impl Into<String>, body: crate::body::RequestBodySend) -> Self {
        Self {
            name: name.into(),
            filename: None,
            content_type: None,
            headers: Vec::new(),
            body: PartBody::Streaming(body),
        }
    }

    /// Set the filename for this part.
    pub fn file_name(mut self, filename: impl Into<String>) -> Self {
        self.filename = Some(filename.into());
        self
    }

    /// Set the MIME type for this part.
    pub fn mime_str(mut self, mime: impl Into<String>) -> Self {
        self.content_type = Some(mime.into());
        self
    }

    /// Add a custom header to this part.
    pub fn header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }

    fn is_streaming(&self) -> bool {
        matches!(self.body, PartBody::Streaming(_))
    }
}

impl Default for Multipart {
    fn default() -> Self {
        Self::new()
    }
}

impl Multipart {
    /// Create an empty multipart body.
    pub fn new() -> Self {
        Self {
            boundary: generate_boundary(),
            parts: Vec::new(),
        }
    }

    /// Add a text field.
    pub fn text(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.parts.push(Part::text(name, value));
        self
    }

    /// Add a file part with name, filename, content type, and data.
    pub fn file(
        mut self,
        name: impl Into<String>,
        filename: impl Into<String>,
        content_type: impl Into<String>,
        data: impl Into<Bytes>,
    ) -> Self {
        self.parts.push(
            Part::bytes(name, data)
                .file_name(filename)
                .mime_str(content_type),
        );
        self
    }

    /// Add a pre-built [`Part`].
    pub fn part(mut self, part: Part) -> Self {
        self.parts.push(part);
        self
    }

    /// Whether any part has a streaming body.
    pub fn has_streaming_parts(&self) -> bool {
        self.parts.iter().any(|p| p.is_streaming())
    }

    /// Return the MIME boundary string.
    pub fn boundary(&self) -> &str {
        &self.boundary
    }

    /// Return the full `Content-Type` header value including boundary.
    pub fn content_type(&self) -> String {
        format!("multipart/form-data; boundary=\"{}\"", self.boundary)
    }

    pub(crate) fn into_bytes(self) -> Bytes {
        let mut buf = BytesMut::new();

        for part in &self.parts {
            buf.put_slice(format!("--{}\r\n", self.boundary).as_bytes());

            let escaped_name = escape_quote(&part.name);
            match (&part.filename, &part.content_type) {
                (Some(filename), Some(ct)) => {
                    let escaped_filename = escape_quote(filename);
                    buf.put_slice(
                        format!(
                            "Content-Disposition: form-data; name=\"{}\"; filename=\"{}\"\r\n",
                            escaped_name, escaped_filename
                        )
                        .as_bytes(),
                    );
                    buf.put_slice(format!("Content-Type: {ct}\r\n").as_bytes());
                }
                (Some(filename), None) => {
                    let escaped_filename = escape_quote(filename);
                    buf.put_slice(
                        format!(
                            "Content-Disposition: form-data; name=\"{}\"; filename=\"{}\"\r\n",
                            escaped_name, escaped_filename
                        )
                        .as_bytes(),
                    );
                }
                (None, Some(ct)) => {
                    buf.put_slice(
                        format!(
                            "Content-Disposition: form-data; name=\"{}\"\r\n",
                            escaped_name
                        )
                        .as_bytes(),
                    );
                    buf.put_slice(format!("Content-Type: {ct}\r\n").as_bytes());
                }
                (None, None) => {
                    buf.put_slice(
                        format!(
                            "Content-Disposition: form-data; name=\"{}\"\r\n",
                            escaped_name
                        )
                        .as_bytes(),
                    );
                }
            }

            for (name, value) in &part.headers {
                buf.put_slice(format!("{name}: {value}\r\n").as_bytes());
            }

            buf.put_slice(b"\r\n");
            if let PartBody::Buffered(data) = &part.body {
                buf.put_slice(data);
            }
            buf.put_slice(b"\r\n");
        }

        buf.put_slice(format!("--{}--\r\n", self.boundary).as_bytes());
        buf.freeze()
    }

    pub(crate) fn into_streaming_body(self) -> crate::body::RequestBodySend {
        use http_body_util::BodyExt;
        use http_body_util::StreamBody;

        let stream = AsyncStream {
            boundary: self.boundary,
            parts: self.parts.into_iter(),
            state: StreamState::NextPart,
            current_body: None,
        };
        let body = StreamBody::new(stream);
        body.map_err(|e| crate::error::Error::Other(Box::new(e)))
            .boxed_unsync()
    }
}

use std::pin::Pin;
use std::task::{Context, Poll};

enum StreamState {
    NextPart,
    Body,
    Done,
}

struct AsyncStream {
    boundary: String,
    parts: std::vec::IntoIter<Part>,
    state: StreamState,
    current_body: Option<crate::body::RequestBodySend>,
}

impl Unpin for AsyncStream {}

impl futures_core::Stream for AsyncStream {
    type Item = Result<hyper::body::Frame<Bytes>, std::io::Error>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = &mut *self;
        loop {
            match this.state {
                StreamState::NextPart => {
                    if let Some(part) = this.parts.next() {
                        let mut header_buf = BytesMut::new();
                        header_buf.put_slice(format!("--{}\r\n", this.boundary).as_bytes());

                        let escaped_name = escape_quote(&part.name);
                        match (&part.filename, &part.content_type) {
                            (Some(filename), Some(ct)) => {
                                let escaped_filename = escape_quote(filename);
                                header_buf.put_slice(
                                    format!(
                                        "Content-Disposition: form-data; name=\"{}\"; filename=\"{}\"\r\n",
                                        escaped_name, escaped_filename
                                    )
                                    .as_bytes(),
                                );
                                header_buf.put_slice(format!("Content-Type: {ct}\r\n").as_bytes());
                            }
                            (Some(filename), None) => {
                                let escaped_filename = escape_quote(filename);
                                header_buf.put_slice(
                                    format!(
                                        "Content-Disposition: form-data; name=\"{}\"; filename=\"{}\"\r\n",
                                        escaped_name, escaped_filename
                                    )
                                    .as_bytes(),
                                );
                            }
                            (None, Some(ct)) => {
                                header_buf.put_slice(
                                    format!(
                                        "Content-Disposition: form-data; name=\"{}\"\r\n",
                                        escaped_name
                                    )
                                    .as_bytes(),
                                );
                                header_buf.put_slice(format!("Content-Type: {ct}\r\n").as_bytes());
                            }
                            (None, None) => {
                                header_buf.put_slice(
                                    format!(
                                        "Content-Disposition: form-data; name=\"{}\"\r\n",
                                        escaped_name
                                    )
                                    .as_bytes(),
                                );
                            }
                        }

                        for (name, value) in &part.headers {
                            header_buf.put_slice(format!("{name}: {value}\r\n").as_bytes());
                        }
                        header_buf.put_slice(b"\r\n");

                        match part.body {
                            PartBody::Buffered(data) => {
                                header_buf.put_slice(&data);
                                header_buf.put_slice(b"\r\n");
                                return Poll::Ready(Some(Ok(hyper::body::Frame::data(
                                    header_buf.freeze(),
                                ))));
                            }
                            PartBody::Streaming(body) => {
                                this.current_body = Some(body);
                                this.state = StreamState::Body;
                                return Poll::Ready(Some(Ok(hyper::body::Frame::data(
                                    header_buf.freeze(),
                                ))));
                            }
                        }
                    } else {
                        this.state = StreamState::Done;
                        let trailer = Bytes::from(format!("--{}--\r\n", this.boundary));
                        return Poll::Ready(Some(Ok(hyper::body::Frame::data(trailer))));
                    }
                }
                StreamState::Body => {
                    if let Some(ref mut body) = this.current_body {
                        use http_body::Body;
                        match Pin::new(body).poll_frame(cx) {
                            Poll::Ready(Some(Ok(frame))) => {
                                if let Ok(data) = frame.into_data() {
                                    return Poll::Ready(Some(Ok(hyper::body::Frame::data(data))));
                                }
                                continue;
                            }
                            Poll::Ready(Some(Err(e))) => {
                                this.state = StreamState::Done;
                                return Poll::Ready(Some(Err(std::io::Error::other(
                                    e.to_string(),
                                ))));
                            }
                            Poll::Ready(None) => {
                                this.current_body = None;
                                this.state = StreamState::NextPart;
                                return Poll::Ready(Some(Ok(hyper::body::Frame::data(
                                    Bytes::from_static(b"\r\n"),
                                ))));
                            }
                            Poll::Pending => return Poll::Pending,
                        }
                    } else {
                        this.state = StreamState::NextPart;
                    }
                }
                StreamState::Done => return Poll::Ready(None),
            }
        }
    }
}

fn escape_quote(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

fn generate_boundary() -> String {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};
    let r1 = RandomState::new().build_hasher().finish();
    let r2 = RandomState::new().build_hasher().finish();
    format!("----aioduct{r1:016x}{r2:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn extract_boundary(ct: &str) -> &str {
        let raw = ct.split("boundary=").nth(1).unwrap();
        raw.trim_matches('"')
    }

    #[test]
    fn content_type_format() {
        let mp = Multipart::new();
        let ct = mp.content_type();
        assert!(ct.starts_with("multipart/form-data; boundary="));
    }

    #[test]
    fn generated_boundary_is_rfc_2046_safe() {
        fn is_boundary_char(byte: u8) -> bool {
            byte.is_ascii_alphanumeric()
                || matches!(
                    byte,
                    b'\''
                        | b'('
                        | b')'
                        | b'+'
                        | b'_'
                        | b','
                        | b'-'
                        | b'.'
                        | b'/'
                        | b':'
                        | b'='
                        | b'?'
                )
        }

        for _ in 0..128 {
            let mp = Multipart::new();
            let boundary = mp.boundary();

            assert!(!boundary.is_empty(), "multipart boundary must not be empty");
            assert!(
                boundary.len() <= 70,
                "multipart boundary must be at most 70 bytes, got {} for {boundary:?}",
                boundary.len()
            );
            assert!(
                boundary.bytes().all(is_boundary_char),
                "multipart boundary contains an unsafe RFC 2046 character: {boundary:?}"
            );
        }
    }

    #[test]
    fn content_type_boundary_matches_body_delimiter() {
        let mp = Multipart::new().text("name", "value");
        let boundary = extract_boundary(&mp.content_type()).to_owned();
        let bytes = mp.into_bytes();
        let body = String::from_utf8(bytes.to_vec()).unwrap();

        assert!(body.starts_with(&format!("--{boundary}\r\n")));
        assert!(body.ends_with(&format!("--{boundary}--\r\n")));
        assert!(!body.contains("----aioduct<boundary>"));
    }

    #[test]
    fn has_streaming_parts_false_for_buffered() {
        let mp = Multipart::new().text("field", "value");
        assert!(!mp.has_streaming_parts());
    }

    #[test]
    fn has_streaming_parts_true_for_stream() {
        let body: crate::body::RequestBodySend = http_body_util::Empty::new()
            .map_err(|never| match never {})
            .boxed_unsync();
        let mp = Multipart::new().part(Part::stream("f", body));
        assert!(mp.has_streaming_parts());
    }

    #[test]
    fn into_bytes_text_field() {
        let mp = Multipart::new().text("name", "value");
        let boundary = extract_boundary(&mp.content_type()).to_owned();
        let bytes = mp.into_bytes();
        let body = String::from_utf8(bytes.to_vec()).unwrap();

        assert!(body.contains(&format!("--{boundary}\r\n")));
        assert!(body.contains("Content-Disposition: form-data; name=\"name\"\r\n"));
        assert!(body.contains("\r\nvalue\r\n"));
        assert!(body.ends_with(&format!("--{boundary}--\r\n")));
    }

    #[test]
    fn into_bytes_file_part() {
        let mp = Multipart::new().file("upload", "test.txt", "text/plain", b"contents".to_vec());
        let boundary = extract_boundary(&mp.content_type()).to_owned();
        let bytes = mp.into_bytes();
        let body = String::from_utf8(bytes.to_vec()).unwrap();

        assert!(body.contains("filename=\"test.txt\""));
        assert!(body.contains("Content-Type: text/plain\r\n"));
        assert!(body.contains("contents"));
        assert!(body.ends_with(&format!("--{boundary}--\r\n")));
    }

    #[test]
    fn into_bytes_no_filename_with_content_type() {
        let part = Part::text("f", "v").mime_str("application/json");
        let mp = Multipart::new().part(part);
        let bytes = mp.into_bytes();
        let body = String::from_utf8(bytes.to_vec()).unwrap();

        assert!(body.contains("name=\"f\""));
        assert!(!body.contains("filename="));
        assert!(body.contains("Content-Type: application/json\r\n"));
    }

    #[test]
    fn into_bytes_filename_without_content_type() {
        let part = Part::text("f", "v").file_name("data.bin");
        let mp = Multipart::new().part(part);
        let bytes = mp.into_bytes();
        let body = String::from_utf8(bytes.to_vec()).unwrap();

        assert!(body.contains("filename=\"data.bin\""));
        assert!(!body.contains("Content-Type:"));
    }

    #[test]
    fn into_bytes_no_filename_no_content_type() {
        let mp = Multipart::new().text("plain", "hi");
        let bytes = mp.into_bytes();
        let body = String::from_utf8(bytes.to_vec()).unwrap();

        assert!(body.contains("name=\"plain\""));
        assert!(!body.contains("filename="));
        assert!(!body.contains("Content-Type:"));
    }

    #[test]
    fn into_bytes_custom_headers() {
        let part = Part::text("f", "v").header("X-Custom", "test-value");
        let mp = Multipart::new().part(part);
        let bytes = mp.into_bytes();
        let body = String::from_utf8(bytes.to_vec()).unwrap();

        assert!(body.contains("X-Custom: test-value\r\n"));
    }

    #[test]
    fn into_bytes_multiple_parts() {
        let mp = Multipart::new().text("a", "1").text("b", "2").file(
            "c",
            "c.txt",
            "text/plain",
            b"3".to_vec(),
        );
        let boundary = extract_boundary(&mp.content_type()).to_owned();
        let bytes = mp.into_bytes();
        let body = String::from_utf8(bytes.to_vec()).unwrap();

        let boundary_count = body.matches(&format!("--{boundary}\r\n")).count();
        assert_eq!(boundary_count, 3);
        assert!(body.contains(&format!("--{boundary}--\r\n")));
    }

    #[test]
    fn default_creates_empty() {
        let mp = Multipart::default();
        assert!(!mp.has_streaming_parts());
        let bytes = mp.into_bytes();
        assert!(!bytes.is_empty());
    }

    #[test]
    fn multipart_debug_impl() {
        let mp = Multipart::new();
        let debug = format!("{:?}", mp);
        assert!(
            debug.contains("Multipart"),
            "Debug should contain struct name"
        );
    }

    #[test]
    fn part_debug_impl() {
        let part = Part::text("my_field", "some value");
        let debug = format!("{:?}", part);
        assert!(debug.contains("Part"), "Debug should contain struct name");
        assert!(
            debug.contains("my_field"),
            "Debug should contain the field name"
        );
    }

    use http_body_util::BodyExt;

    #[test]
    fn part_bytes_creates_buffered() {
        let part = Part::bytes("data", b"hello".to_vec());
        assert!(!part.is_streaming());
        assert_eq!(part.name, "data");
    }

    #[test]
    fn part_stream_creates_streaming() {
        let body: crate::body::RequestBodySend = http_body_util::Empty::new()
            .map_err(|never| match never {})
            .boxed_unsync();
        let part = Part::stream("s", body);
        assert!(part.is_streaming());
        assert_eq!(part.name, "s");
    }

    #[test]
    fn part_builder_methods() {
        let part = Part::text("f", "v")
            .file_name("name.txt")
            .mime_str("text/plain")
            .header("X-A", "1");
        assert_eq!(part.filename.as_deref(), Some("name.txt"));
        assert_eq!(part.content_type.as_deref(), Some("text/plain"));
        assert_eq!(part.headers.len(), 1);
    }

    #[test]
    fn into_bytes_skips_streaming_parts() {
        let data = bytes::Bytes::from("streamed data that should be absent");
        let stream_body: crate::body::RequestBodySend = http_body_util::Full::new(data)
            .map_err(|never| match never {})
            .boxed_unsync();
        let part = Part::stream("field", stream_body);
        let mp = Multipart::new().part(part);
        let bytes = mp.into_bytes();
        let body = String::from_utf8(bytes.to_vec()).unwrap();

        assert!(
            !body.contains("streamed data that should be absent"),
            "streaming part body data must not appear in into_bytes() output, found:\n{body}"
        );
        // Headers for the streaming part should still appear.
        assert!(
            body.contains("name=\"field\""),
            "streaming part headers should appear"
        );
    }

    #[test]
    fn field_name_with_special_chars() {
        let mp = Multipart::new().text("na me", "value");
        let bytes = mp.into_bytes();
        let body = String::from_utf8(bytes.to_vec()).unwrap();

        // The space in the field name should appear literally, not percent-encoded.
        assert!(
            body.contains("name=\"na me\""),
            "field name with space should appear with space intact in Content-Disposition, found:\n{body}"
        );
        assert!(
            !body.contains("na%20me"),
            "field name should not be percent-encoded"
        );
    }

    #[test]
    fn file_name_escape_quote() {
        // Input: file\"name.txt (contains a backslash and a literal quote character)
        let input = "file\\\"name.txt";
        let escaped = escape_quote(input);
        // After escape_quote: backslash -> \\, quote -> \"
        assert_eq!(escaped, "file\\\\\\\"name.txt");

        let part = Part::text("f", "v").file_name(input);
        let mp = Multipart::new().part(part);
        let bytes = mp.into_bytes();
        let body = String::from_utf8(bytes.to_vec()).unwrap();

        // The escaped filename should appear in the serialized output.
        assert!(
            body.contains(&format!("filename=\"{}\"", escaped)),
            "escaped filename should appear in serialized output, found:\n{body}"
        );
    }

    #[test]
    fn duplicate_field_names_both_appear() {
        let mp = Multipart::new().text("dup", "val1").text("dup", "val2");
        let bytes = mp.into_bytes();
        let body = String::from_utf8(bytes.to_vec()).unwrap();

        assert!(
            body.contains("val1"),
            "first duplicate field value should appear, found:\n{body}"
        );
        assert!(
            body.contains("val2"),
            "second duplicate field value should appear, found:\n{body}"
        );
        // Both parts should have the same field name.
        let name_occurrences = body.matches("name=\"dup\"").count();
        assert_eq!(
            name_occurrences, 2,
            "field name 'dup' should appear exactly twice, found {name_occurrences} in:\n{body}"
        );
    }

    #[test]
    fn content_type_always_form_data() {
        let mp = Multipart::new().text("k", "v");
        let ct = mp.content_type();
        assert!(
            ct.starts_with("multipart/form-data"),
            "content_type() must start with multipart/form-data, got: {ct}"
        );
    }
}

#[cfg(all(test, feature = "tokio"))]
mod streaming_tests {
    use super::*;
    use http_body_util::BodyExt;

    async fn collect_streaming(mp: Multipart) -> String {
        let body = mp.into_streaming_body();
        let collected = body.collect().await.unwrap().to_bytes();
        String::from_utf8(collected.to_vec()).unwrap()
    }

    #[tokio::test]
    async fn streaming_buffered_text_field() {
        let mp = Multipart::new().text("name", "value");
        let boundary = mp
            .content_type()
            .split("boundary=")
            .nth(1)
            .unwrap()
            .trim_matches('"')
            .to_owned();
        let body = collect_streaming(mp).await;

        assert!(body.contains(&format!("--{boundary}\r\n")));
        assert!(body.contains("Content-Disposition: form-data; name=\"name\"\r\n"));
        assert!(body.contains("value\r\n"));
        assert!(body.ends_with(&format!("--{boundary}--\r\n")));
    }

    #[tokio::test]
    async fn streaming_file_part_with_filename_and_content_type() {
        let mp = Multipart::new().file("upload", "test.txt", "text/plain", b"contents".to_vec());
        let body = collect_streaming(mp).await;

        assert!(body.contains("filename=\"test.txt\""));
        assert!(body.contains("Content-Type: text/plain\r\n"));
        assert!(body.contains("contents"));
    }

    #[tokio::test]
    async fn streaming_filename_without_content_type() {
        let part = Part::text("f", "v").file_name("data.bin");
        let mp = Multipart::new().part(part);
        let body = collect_streaming(mp).await;

        assert!(body.contains("filename=\"data.bin\""));
        assert!(!body.contains("Content-Type:"));
    }

    #[tokio::test]
    async fn streaming_content_type_without_filename() {
        let part = Part::text("f", "v").mime_str("application/json");
        let mp = Multipart::new().part(part);
        let body = collect_streaming(mp).await;

        assert!(body.contains("name=\"f\""));
        assert!(!body.contains("filename="));
        assert!(body.contains("Content-Type: application/json\r\n"));
    }

    #[tokio::test]
    async fn streaming_no_filename_no_content_type() {
        let mp = Multipart::new().text("plain", "hi");
        let body = collect_streaming(mp).await;

        assert!(body.contains("name=\"plain\""));
        assert!(!body.contains("filename="));
        assert!(!body.contains("Content-Type:"));
    }

    #[tokio::test]
    async fn streaming_custom_headers() {
        let part = Part::text("f", "v").header("X-Custom", "test-value");
        let mp = Multipart::new().part(part);
        let body = collect_streaming(mp).await;

        assert!(body.contains("X-Custom: test-value\r\n"));
    }

    #[tokio::test]
    async fn streaming_multiple_buffered_parts() {
        let mp = Multipart::new().text("a", "1").text("b", "2").file(
            "c",
            "c.txt",
            "text/plain",
            b"3".to_vec(),
        );
        let boundary = mp
            .content_type()
            .split("boundary=")
            .nth(1)
            .unwrap()
            .trim_matches('"')
            .to_owned();
        let body = collect_streaming(mp).await;

        let boundary_count = body.matches(&format!("--{boundary}\r\n")).count();
        assert_eq!(boundary_count, 3);
        assert!(body.contains(&format!("--{boundary}--\r\n")));
    }

    #[tokio::test]
    async fn streaming_with_stream_body() {
        let data = bytes::Bytes::from("streamed data");
        let stream_body: crate::body::RequestBodySend = http_body_util::Full::new(data)
            .map_err(|never| match never {})
            .boxed_unsync();

        let part = Part::stream("file", stream_body)
            .file_name("stream.bin")
            .mime_str("application/octet-stream");
        let mp = Multipart::new().part(part);
        let body = collect_streaming(mp).await;

        assert!(body.contains("filename=\"stream.bin\""));
        assert!(body.contains("Content-Type: application/octet-stream\r\n"));
        assert!(body.contains("streamed data"));
    }

    #[tokio::test]
    async fn streaming_mixed_buffered_and_stream() {
        let stream_body: crate::body::RequestBodySend =
            http_body_util::Full::new(bytes::Bytes::from("stream content"))
                .map_err(|never| match never {})
                .boxed_unsync();

        let mp = Multipart::new()
            .text("text_field", "text_value")
            .part(Part::stream("stream_field", stream_body).file_name("f.bin"));
        let boundary = mp
            .content_type()
            .split("boundary=")
            .nth(1)
            .unwrap()
            .trim_matches('"')
            .to_owned();
        let body = collect_streaming(mp).await;

        assert!(body.contains("text_value"));
        assert!(body.contains("stream content"));
        assert!(body.ends_with(&format!("--{boundary}--\r\n")));
    }

    #[tokio::test]
    async fn streaming_empty_multipart() {
        let mp = Multipart::new();
        let boundary = mp
            .content_type()
            .split("boundary=")
            .nth(1)
            .unwrap()
            .trim_matches('"')
            .to_owned();
        let body = collect_streaming(mp).await;

        assert_eq!(body, format!("--{boundary}--\r\n"));
    }

    #[tokio::test]
    async fn streaming_body_with_trailers_frame() {
        // Test the path where a streaming body returns a non-data frame (trailers).
        // The stream should skip it (continue) and eventually get the body data.
        use std::pin::Pin;
        use std::task::{Context, Poll};

        struct TrailerThenDataBody {
            sent_trailer: bool,
            sent_data: bool,
        }

        impl http_body::Body for TrailerThenDataBody {
            type Data = bytes::Bytes;
            type Error = crate::error::Error;

            fn poll_frame(
                mut self: Pin<&mut Self>,
                _cx: &mut Context<'_>,
            ) -> Poll<Option<Result<hyper::body::Frame<Self::Data>, Self::Error>>> {
                if !self.sent_trailer {
                    self.sent_trailer = true;
                    // Return a trailers frame (not data) - this triggers the `continue` path
                    let mut map = http::HeaderMap::new();
                    map.insert("x-trailer", http::HeaderValue::from_static("value"));
                    Poll::Ready(Some(Ok(hyper::body::Frame::trailers(map))))
                } else if !self.sent_data {
                    self.sent_data = true;
                    Poll::Ready(Some(Ok(hyper::body::Frame::data(bytes::Bytes::from(
                        "actual data",
                    )))))
                } else {
                    Poll::Ready(None)
                }
            }
        }

        let body: crate::body::RequestBodySend = TrailerThenDataBody {
            sent_trailer: false,
            sent_data: false,
        }
        .boxed_unsync();

        let part = Part::stream("field", body);
        let mp = Multipart::new().part(part);
        let body_out = collect_streaming(mp).await;

        assert!(
            body_out.contains("actual data"),
            "should contain the actual data after skipping trailer frame"
        );
    }

    #[tokio::test]
    async fn streaming_error_propagation() {
        use std::pin::Pin;
        use std::task::{Context, Poll};

        struct ErrorBody;
        impl http_body::Body for ErrorBody {
            type Data = bytes::Bytes;
            type Error = crate::error::Error;

            fn poll_frame(
                self: Pin<&mut Self>,
                _cx: &mut Context<'_>,
            ) -> Poll<Option<Result<hyper::body::Frame<Self::Data>, Self::Error>>> {
                Poll::Ready(Some(Err(crate::error::Error::Other("test error".into()))))
            }
        }

        let error_body: crate::body::RequestBodySend = ErrorBody.boxed_unsync();
        let part = Part::stream("err", error_body);
        let mp = Multipart::new().part(part);
        let body = mp.into_streaming_body();

        let result = body.collect().await;
        assert!(result.is_err());
    }
}
