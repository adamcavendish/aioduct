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
    Streaming(crate::error::HyperBody),
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
    pub fn stream(name: impl Into<String>, body: crate::error::HyperBody) -> Self {
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

    pub(crate) fn content_type(&self) -> String {
        format!("multipart/form-data; boundary={}", self.boundary)
    }

    pub(crate) fn into_bytes(self) -> Bytes {
        let mut buf = BytesMut::new();

        for part in &self.parts {
            buf.put_slice(format!("--{}\r\n", self.boundary).as_bytes());

            match (&part.filename, &part.content_type) {
                (Some(filename), Some(ct)) => {
                    buf.put_slice(
                        format!(
                            "Content-Disposition: form-data; name=\"{}\"; filename=\"{}\"\r\n",
                            part.name, filename
                        )
                        .as_bytes(),
                    );
                    buf.put_slice(format!("Content-Type: {ct}\r\n").as_bytes());
                }
                (Some(filename), None) => {
                    buf.put_slice(
                        format!(
                            "Content-Disposition: form-data; name=\"{}\"; filename=\"{}\"\r\n",
                            part.name, filename
                        )
                        .as_bytes(),
                    );
                }
                (None, Some(ct)) => {
                    buf.put_slice(
                        format!("Content-Disposition: form-data; name=\"{}\"\r\n", part.name)
                            .as_bytes(),
                    );
                    buf.put_slice(format!("Content-Type: {ct}\r\n").as_bytes());
                }
                (None, None) => {
                    buf.put_slice(
                        format!("Content-Disposition: form-data; name=\"{}\"\r\n", part.name)
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

    pub(crate) fn into_streaming_body(self) -> crate::error::HyperBody {
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
            .boxed()
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
    current_body: Option<crate::error::HyperBody>,
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

                        match (&part.filename, &part.content_type) {
                            (Some(filename), Some(ct)) => {
                                header_buf.put_slice(
                                    format!(
                                        "Content-Disposition: form-data; name=\"{}\"; filename=\"{}\"\r\n",
                                        part.name, filename
                                    )
                                    .as_bytes(),
                                );
                                header_buf.put_slice(format!("Content-Type: {ct}\r\n").as_bytes());
                            }
                            (Some(filename), None) => {
                                header_buf.put_slice(
                                    format!(
                                        "Content-Disposition: form-data; name=\"{}\"; filename=\"{}\"\r\n",
                                        part.name, filename
                                    )
                                    .as_bytes(),
                                );
                            }
                            (None, Some(ct)) => {
                                header_buf.put_slice(
                                    format!(
                                        "Content-Disposition: form-data; name=\"{}\"\r\n",
                                        part.name
                                    )
                                    .as_bytes(),
                                );
                                header_buf.put_slice(format!("Content-Type: {ct}\r\n").as_bytes());
                            }
                            (None, None) => {
                                header_buf.put_slice(
                                    format!(
                                        "Content-Disposition: form-data; name=\"{}\"\r\n",
                                        part.name
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

fn generate_boundary() -> String {
    use std::time::SystemTime;
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("----aioduct{nanos:x}")
}
