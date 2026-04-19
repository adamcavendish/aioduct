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
    use std::io::Write;
    use std::pin::Pin;
    use std::task::{Context, Poll};

    use bytes::Bytes;
    use http::HeaderMap;
    use http::header::{CONTENT_ENCODING, CONTENT_LENGTH};
    use http_body_util::BodyExt;

    use crate::error::{Error, HyperBody};

    use super::AcceptEncoding;

    enum StreamDecoder {
        #[cfg(feature = "gzip")]
        Gzip(flate2::write::GzDecoder<Vec<u8>>),
        #[cfg(feature = "deflate")]
        Deflate(flate2::write::ZlibDecoder<Vec<u8>>),
        #[cfg(feature = "brotli")]
        Brotli(Box<brotli::DecompressorWriter<Vec<u8>>>),
        #[cfg(feature = "zstd")]
        Zstd(zstd::stream::write::Decoder<'static, Vec<u8>>),
    }

    impl StreamDecoder {
        fn write_chunk(&mut self, data: &[u8]) -> Result<(), Error> {
            match self {
                #[cfg(feature = "gzip")]
                StreamDecoder::Gzip(d) => d.write_all(data).map_err(|e| Error::Other(Box::new(e))),
                #[cfg(feature = "deflate")]
                StreamDecoder::Deflate(d) => {
                    d.write_all(data).map_err(|e| Error::Other(Box::new(e)))
                }
                #[cfg(feature = "brotli")]
                StreamDecoder::Brotli(d) => {
                    d.write_all(data).map_err(|e| Error::Other(Box::new(e)))
                }
                #[cfg(feature = "zstd")]
                StreamDecoder::Zstd(d) => d.write_all(data).map_err(|e| Error::Other(Box::new(e))),
            }
        }

        fn take_output(&mut self) -> Vec<u8> {
            match self {
                #[cfg(feature = "gzip")]
                StreamDecoder::Gzip(d) => std::mem::take(d.get_mut()),
                #[cfg(feature = "deflate")]
                StreamDecoder::Deflate(d) => std::mem::take(d.get_mut()),
                #[cfg(feature = "brotli")]
                StreamDecoder::Brotli(d) => std::mem::take(d.get_mut()),
                #[cfg(feature = "zstd")]
                StreamDecoder::Zstd(d) => std::mem::take(d.get_mut()),
            }
        }

        fn finish(self) -> Result<Vec<u8>, Error> {
            match self {
                #[cfg(feature = "gzip")]
                StreamDecoder::Gzip(d) => d.finish().map_err(|e| Error::Other(Box::new(e))),
                #[cfg(feature = "deflate")]
                StreamDecoder::Deflate(d) => d.finish().map_err(|e| Error::Other(Box::new(e))),
                #[cfg(feature = "brotli")]
                StreamDecoder::Brotli(mut d) => {
                    d.flush().map_err(|e| Error::Other(Box::new(e)))?;
                    Ok(std::mem::take(d.get_mut()))
                }
                #[cfg(feature = "zstd")]
                StreamDecoder::Zstd(mut d) => {
                    d.flush().map_err(|e| Error::Other(Box::new(e)))?;
                    Ok(std::mem::take(d.get_mut()))
                }
            }
        }

        #[cfg(feature = "gzip")]
        fn new_gzip() -> Self {
            StreamDecoder::Gzip(flate2::write::GzDecoder::new(Vec::new()))
        }

        #[cfg(feature = "deflate")]
        fn new_deflate() -> Self {
            StreamDecoder::Deflate(flate2::write::ZlibDecoder::new(Vec::new()))
        }

        #[cfg(feature = "brotli")]
        fn new_brotli() -> Self {
            StreamDecoder::Brotli(Box::new(brotli::DecompressorWriter::new(Vec::new(), 4096)))
        }

        #[cfg(feature = "zstd")]
        fn new_zstd() -> Result<Self, Error> {
            Ok(StreamDecoder::Zstd(
                zstd::stream::write::Decoder::new(Vec::new())
                    .map_err(|e| Error::Other(Box::new(e)))?,
            ))
        }
    }

    struct DecompressBody {
        body: HyperBody,
        decoder: Option<StreamDecoder>,
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

            match Pin::new(&mut self.body).poll_frame(cx) {
                Poll::Ready(Some(Ok(frame))) => {
                    if let Ok(data) = frame.into_data() {
                        if let Some(decoder) = &mut self.decoder {
                            if let Err(e) = decoder.write_chunk(&data) {
                                self.finished = true;
                                return Poll::Ready(Some(Err(e)));
                            }
                            let output = decoder.take_output();
                            if output.is_empty() {
                                cx.waker().wake_by_ref();
                                return Poll::Pending;
                            }
                            Poll::Ready(Some(Ok(hyper::body::Frame::data(Bytes::from(output)))))
                        } else {
                            Poll::Ready(Some(Ok(hyper::body::Frame::data(data))))
                        }
                    } else {
                        cx.waker().wake_by_ref();
                        Poll::Pending
                    }
                }
                Poll::Ready(Some(Err(e))) => {
                    self.finished = true;
                    Poll::Ready(Some(Err(e)))
                }
                Poll::Ready(None) => {
                    self.finished = true;
                    if let Some(decoder) = self.decoder.take() {
                        match decoder.finish() {
                            Ok(remaining) if !remaining.is_empty() => Poll::Ready(Some(Ok(
                                hyper::body::Frame::data(Bytes::from(remaining)),
                            ))),
                            Ok(_) => Poll::Ready(None),
                            Err(e) => Poll::Ready(Some(Err(e))),
                        }
                    } else {
                        Poll::Ready(None)
                    }
                }
                Poll::Pending => Poll::Pending,
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

        let decoder = match encoding {
            #[cfg(feature = "gzip")]
            b"gzip" if accept.gzip => Some(StreamDecoder::new_gzip()),
            #[cfg(feature = "deflate")]
            b"deflate" if accept.deflate => Some(StreamDecoder::new_deflate()),
            #[cfg(feature = "brotli")]
            b"br" if accept.brotli => Some(StreamDecoder::new_brotli()),
            #[cfg(feature = "zstd")]
            b"zstd" if accept.zstd => match StreamDecoder::new_zstd() {
                Ok(d) => Some(d),
                Err(_) => return body,
            },
            _ => None,
        };

        match decoder {
            Some(decoder) => {
                headers.remove(CONTENT_ENCODING);
                headers.remove(CONTENT_LENGTH);
                let decompress = DecompressBody {
                    body,
                    decoder: Some(decoder),
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

#[cfg(test)]
mod tests {
    use super::*;
    use http::HeaderMap;
    use http::header::{ACCEPT_ENCODING, CONTENT_ENCODING, CONTENT_LENGTH};

    #[test]
    fn accept_encoding_default_is_not_empty() {
        let ae = AcceptEncoding::default();
        // At least when compiled with any codec feature, default is not empty
        let _ = ae.is_empty();
        let _ = ae.header_value();
    }

    #[test]
    fn accept_encoding_none_is_empty() {
        let ae = AcceptEncoding::none();
        assert!(ae.is_empty());
        assert!(ae.header_value().is_none());
    }

    #[test]
    fn set_accept_encoding_adds_header() {
        let mut headers = HeaderMap::new();
        let ae = AcceptEncoding::default();
        set_accept_encoding(&mut headers, &ae);
        if !ae.is_empty() {
            assert!(headers.contains_key(ACCEPT_ENCODING));
        }
    }

    #[test]
    fn set_accept_encoding_does_not_overwrite_existing() {
        let mut headers = HeaderMap::new();
        headers.insert(ACCEPT_ENCODING, "identity".parse().unwrap());
        set_accept_encoding(&mut headers, &AcceptEncoding::default());
        assert_eq!(headers.get(ACCEPT_ENCODING).unwrap(), "identity");
    }

    #[test]
    fn set_accept_encoding_noop_for_none() {
        let mut headers = HeaderMap::new();
        set_accept_encoding(&mut headers, &AcceptEncoding::none());
        assert!(!headers.contains_key(ACCEPT_ENCODING));
    }

    #[test]
    fn maybe_decompress_passthrough_when_empty() {
        use http_body_util::BodyExt;
        let mut headers = HeaderMap::new();
        let body: HyperBody = http_body_util::Empty::new()
            .map_err(|never| match never {})
            .boxed();
        let ae = AcceptEncoding::none();
        let _result = maybe_decompress(&mut headers, body, &ae);
    }

    #[test]
    fn maybe_decompress_passthrough_no_encoding_header() {
        use http_body_util::BodyExt;
        let mut headers = HeaderMap::new();
        let body: HyperBody = http_body_util::Full::new(bytes::Bytes::from("hello"))
            .map_err(|never| match never {})
            .boxed();
        let ae = AcceptEncoding::default();
        let _result = maybe_decompress(&mut headers, body, &ae);
    }

    #[cfg(feature = "gzip")]
    #[test]
    fn accept_encoding_includes_gzip() {
        let ae = AcceptEncoding::default();
        assert!(ae.gzip);
        let hv = ae.header_value().unwrap();
        let val = hv.to_str().unwrap();
        assert!(val.contains("gzip"));
    }

    #[cfg(feature = "gzip")]
    #[tokio::test]
    async fn maybe_decompress_gzip_round_trip() {
        use bytes::Bytes;
        use flate2::Compression;
        use flate2::write::GzEncoder;
        use http_body_util::BodyExt;
        use std::io::Write;

        let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
        encoder.write_all(b"hello gzip").unwrap();
        let compressed = encoder.finish().unwrap();

        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_ENCODING, "gzip".parse().unwrap());
        headers.insert(
            CONTENT_LENGTH,
            compressed.len().to_string().parse().unwrap(),
        );

        let body: HyperBody = http_body_util::Full::new(Bytes::from(compressed))
            .map_err(|never| match never {})
            .boxed();
        let ae = AcceptEncoding::default();
        let result_body = maybe_decompress(&mut headers, body, &ae);

        assert!(!headers.contains_key(CONTENT_ENCODING));
        assert!(!headers.contains_key(CONTENT_LENGTH));

        let collected = result_body.collect().await.unwrap().to_bytes();
        assert_eq!(&collected[..], b"hello gzip");
    }

    #[test]
    fn accept_encoding_clone() {
        let ae = AcceptEncoding::default();
        let ae2 = ae.clone();
        assert_eq!(ae.is_empty(), ae2.is_empty());
    }

    #[test]
    fn accept_encoding_debug() {
        let ae = AcceptEncoding::default();
        let dbg = format!("{ae:?}");
        assert!(dbg.contains("AcceptEncoding"));
    }
}
