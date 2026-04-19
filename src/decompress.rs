use http::HeaderMap;
use http::header::ACCEPT_ENCODING;

use crate::error::HyperBody;

#[derive(Clone, Debug)]
pub(crate) struct AcceptEncoding {
    #[cfg(feature = "gzip")]
    pub gzip: bool,
    #[cfg(feature = "brotli")]
    pub brotli: bool,
    #[cfg(feature = "zstd")]
    pub zstd: bool,
    #[cfg(feature = "deflate")]
    pub deflate: bool,
}

#[allow(clippy::derivable_impls)]
impl Default for AcceptEncoding {
    fn default() -> Self {
        Self {
            #[cfg(feature = "gzip")]
            gzip: true,
            #[cfg(feature = "brotli")]
            brotli: true,
            #[cfg(feature = "zstd")]
            zstd: true,
            #[cfg(feature = "deflate")]
            deflate: true,
        }
    }
}

impl AcceptEncoding {
    pub fn none() -> Self {
        Self {
            #[cfg(feature = "gzip")]
            gzip: false,
            #[cfg(feature = "brotli")]
            brotli: false,
            #[cfg(feature = "zstd")]
            zstd: false,
            #[cfg(feature = "deflate")]
            deflate: false,
        }
    }

    pub fn header_value(&self) -> Option<http::HeaderValue> {
        #[allow(unused_mut)]
        let mut parts: Vec<&str> = Vec::new();

        #[cfg(feature = "zstd")]
        if self.zstd {
            parts.push("zstd");
        }
        #[cfg(feature = "gzip")]
        if self.gzip {
            parts.push("gzip");
        }
        #[cfg(feature = "deflate")]
        if self.deflate {
            parts.push("deflate");
        }
        #[cfg(feature = "brotli")]
        if self.brotli {
            parts.push("br");
        }

        if parts.is_empty() {
            return None;
        }

        http::HeaderValue::from_str(&parts.join(", ")).ok()
    }

    pub fn is_empty(&self) -> bool {
        #[allow(unused_mut)]
        let mut empty = true;
        #[cfg(feature = "gzip")]
        {
            empty = empty && !self.gzip;
        }
        #[cfg(feature = "brotli")]
        {
            empty = empty && !self.brotli;
        }
        #[cfg(feature = "zstd")]
        {
            empty = empty && !self.zstd;
        }
        #[cfg(feature = "deflate")]
        {
            empty = empty && !self.deflate;
        }
        empty
    }
}

pub(crate) fn set_accept_encoding(headers: &mut HeaderMap, accept: &AcceptEncoding) {
    if !headers.contains_key(ACCEPT_ENCODING) {
        if let Some(value) = accept.header_value() {
            headers.insert(ACCEPT_ENCODING, value);
        }
    }
}

pub(crate) fn maybe_decompress(
    headers: &mut HeaderMap,
    body: HyperBody,
    accept: &AcceptEncoding,
) -> HyperBody {
    if accept.is_empty() {
        return body;
    }

    #[cfg(any(
        feature = "gzip",
        feature = "deflate",
        feature = "brotli",
        feature = "zstd"
    ))]
    {
        decompress_impl(headers, body, accept)
    }

    #[cfg(not(any(
        feature = "gzip",
        feature = "deflate",
        feature = "brotli",
        feature = "zstd"
    )))]
    {
        let _ = headers;
        body
    }
}

// ---------- decompression impl (only compiled when at least one codec is enabled) ----------

#[cfg(any(
    feature = "gzip",
    feature = "deflate",
    feature = "brotli",
    feature = "zstd"
))]
mod imp {
    use std::pin::Pin;
    use std::task::{Context, Poll};

    use bytes::Bytes;
    use http::HeaderMap;
    use http::header::{CONTENT_ENCODING, CONTENT_LENGTH};
    use http_body_util::BodyExt;

    use crate::error::{Error, HyperBody};

    use super::AcceptEncoding;

    enum Encoding {
        #[cfg(feature = "gzip")]
        Gzip,
        #[cfg(feature = "deflate")]
        Deflate,
        #[cfg(feature = "brotli")]
        Brotli,
        #[cfg(feature = "zstd")]
        Zstd,
    }

    fn decompress_buf(encoding: &Encoding, buf: &[u8]) -> Result<Vec<u8>, Error> {
        match encoding {
            #[cfg(feature = "gzip")]
            Encoding::Gzip => {
                use std::io::Read;
                let mut decoder = flate2::read::GzDecoder::new(buf);
                let mut out = Vec::new();
                decoder
                    .read_to_end(&mut out)
                    .map_err(|e| Error::Other(Box::new(e)))?;
                Ok(out)
            }
            #[cfg(feature = "deflate")]
            Encoding::Deflate => {
                use std::io::Read;
                let mut decoder = flate2::read::ZlibDecoder::new(buf);
                let mut out = Vec::new();
                decoder
                    .read_to_end(&mut out)
                    .map_err(|e| Error::Other(Box::new(e)))?;
                Ok(out)
            }
            #[cfg(feature = "brotli")]
            Encoding::Brotli => {
                let mut out = Vec::new();
                brotli::BrotliDecompress(&mut &buf[..], &mut out)
                    .map_err(|e| Error::Other(format!("brotli: {e}").into()))?;
                Ok(out)
            }
            #[cfg(feature = "zstd")]
            Encoding::Zstd => zstd::stream::decode_all(buf).map_err(|e| Error::Other(Box::new(e))),
        }
    }

    struct DecompressBody {
        body: HyperBody,
        encoding: Encoding,
        accumulated: Vec<u8>,
        finished: bool,
    }

    impl http_body::Body for DecompressBody {
        type Data = Bytes;
        type Error = Error;

        fn poll_frame(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
        ) -> Poll<Option<Result<hyper::body::Frame<Bytes>, Error>>> {
            if self.finished {
                return Poll::Ready(None);
            }

            loop {
                match Pin::new(&mut self.body).poll_frame(cx) {
                    Poll::Ready(Some(Ok(frame))) => {
                        if let Ok(data) = frame.into_data() {
                            self.accumulated.extend_from_slice(&data);
                        }
                    }
                    Poll::Ready(Some(Err(e))) => {
                        self.finished = true;
                        return Poll::Ready(Some(Err(e)));
                    }
                    Poll::Ready(None) => {
                        self.finished = true;
                        if self.accumulated.is_empty() {
                            return Poll::Ready(None);
                        }
                        return match decompress_buf(&self.encoding, &self.accumulated) {
                            Ok(out) => {
                                Poll::Ready(Some(Ok(hyper::body::Frame::data(Bytes::from(out)))))
                            }
                            Err(e) => Poll::Ready(Some(Err(e))),
                        };
                    }
                    Poll::Pending => return Poll::Pending,
                }
            }
        }
    }

    pub(super) fn decompress_impl(
        headers: &mut HeaderMap,
        body: HyperBody,
        accept: &AcceptEncoding,
    ) -> HyperBody {
        let encoding = match headers.get(CONTENT_ENCODING) {
            Some(v) => v.as_bytes(),
            None => return body,
        };

        let enc = match encoding {
            #[cfg(feature = "gzip")]
            b"gzip" if accept.gzip => Some(Encoding::Gzip),
            #[cfg(feature = "deflate")]
            b"deflate" if accept.deflate => Some(Encoding::Deflate),
            #[cfg(feature = "brotli")]
            b"br" if accept.brotli => Some(Encoding::Brotli),
            #[cfg(feature = "zstd")]
            b"zstd" if accept.zstd => Some(Encoding::Zstd),
            _ => None,
        };

        match enc {
            Some(encoding) => {
                headers.remove(CONTENT_ENCODING);
                headers.remove(CONTENT_LENGTH);
                let decompress = DecompressBody {
                    body,
                    encoding,
                    accumulated: Vec::new(),
                    finished: false,
                };
                decompress.boxed()
            }
            None => body,
        }
    }
}

#[cfg(any(
    feature = "gzip",
    feature = "deflate",
    feature = "brotli",
    feature = "zstd"
))]
use imp::decompress_impl;
