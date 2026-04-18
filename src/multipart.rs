use bytes::{BufMut, Bytes, BytesMut};

pub struct Multipart {
    boundary: String,
    parts: Vec<Part>,
}

struct Part {
    name: String,
    filename: Option<String>,
    content_type: Option<String>,
    body: Bytes,
}

impl Default for Multipart {
    fn default() -> Self {
        Self::new()
    }
}

impl Multipart {
    pub fn new() -> Self {
        Self {
            boundary: generate_boundary(),
            parts: Vec::new(),
        }
    }

    pub fn text(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.parts.push(Part {
            name: name.into(),
            filename: None,
            content_type: None,
            body: Bytes::from(value.into()),
        });
        self
    }

    pub fn file(
        mut self,
        name: impl Into<String>,
        filename: impl Into<String>,
        content_type: impl Into<String>,
        data: impl Into<Bytes>,
    ) -> Self {
        self.parts.push(Part {
            name: name.into(),
            filename: Some(filename.into()),
            content_type: Some(content_type.into()),
            body: data.into(),
        });
        self
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
                _ => {
                    buf.put_slice(
                        format!("Content-Disposition: form-data; name=\"{}\"\r\n", part.name)
                            .as_bytes(),
                    );
                }
            }

            buf.put_slice(b"\r\n");
            buf.put_slice(&part.body);
            buf.put_slice(b"\r\n");
        }

        buf.put_slice(format!("--{}--\r\n", self.boundary).as_bytes());
        buf.freeze()
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
