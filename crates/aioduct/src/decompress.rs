use http::HeaderMap;
#[cfg(test)]
use http::header::ACCEPT_ENCODING;

use crate::body::RequestBodySend;

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

#[cfg(test)]
pub(crate) fn set_accept_encoding(headers: &mut HeaderMap, accept: &AcceptEncoding) {
    if !headers.contains_key(ACCEPT_ENCODING)
        && let Some(value) = accept.header_value()
    {
        headers.insert(ACCEPT_ENCODING, value);
    }
}

pub(crate) fn maybe_decompress(
    headers: &mut HeaderMap,
    body: RequestBodySend,
    accept: &AcceptEncoding,
) -> RequestBodySend {
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

    use crate::body::RequestBodySend;
    use crate::error::Error;

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
        body: RequestBodySend,
        decoder: Option<StreamDecoder>,
        finished: bool,
        has_data: bool,
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
                        match frame.into_data() {
                            Ok(data) => {
                                if self.decoder.is_some() {
                                    self.has_data = true;
                                    // SAFETY: we just checked is_some() above and nothing
                                    // removes the decoder between the check and this line.
                                    #[allow(clippy::unwrap_used)]
                                    let decoder = self.decoder.as_mut().unwrap();
                                    if let Err(e) = decoder.write_chunk(&data) {
                                        self.finished = true;
                                        return Poll::Ready(Some(Err(e)));
                                    }
                                    let output = decoder.take_output();
                                    if output.is_empty() {
                                        continue;
                                    }
                                    return Poll::Ready(Some(Ok(hyper::body::Frame::data(
                                        Bytes::from(output),
                                    ))));
                                } else {
                                    return Poll::Ready(Some(Ok(hyper::body::Frame::data(data))));
                                }
                            }
                            Err(frame) => return Poll::Ready(Some(Ok(frame))),
                        }
                    }
                    Poll::Ready(Some(Err(e))) => {
                        self.finished = true;
                        return Poll::Ready(Some(Err(e)));
                    }
                    Poll::Ready(None) => {
                        self.finished = true;
                        if let Some(decoder) = self.decoder.take() {
                            if !self.has_data {
                                return Poll::Ready(None);
                            }
                            return match decoder.finish() {
                                Ok(remaining) if !remaining.is_empty() => Poll::Ready(Some(Ok(
                                    hyper::body::Frame::data(Bytes::from(remaining)),
                                ))),
                                Ok(_) => Poll::Ready(None),
                                Err(e) => Poll::Ready(Some(Err(e))),
                            };
                        } else {
                            return Poll::Ready(None);
                        }
                    }
                    Poll::Pending => return Poll::Pending,
                }
            }
        }
    }

    fn make_decoder(
        encoding: &str,
        accept: &AcceptEncoding,
    ) -> Option<Result<StreamDecoder, Error>> {
        #[cfg(feature = "gzip")]
        if (encoding.eq_ignore_ascii_case("gzip") || encoding.eq_ignore_ascii_case("x-gzip"))
            && accept.gzip
        {
            return Some(Ok(StreamDecoder::new_gzip()));
        }
        #[cfg(feature = "deflate")]
        if encoding.eq_ignore_ascii_case("deflate") && accept.deflate {
            return Some(Ok(StreamDecoder::new_deflate()));
        }
        #[cfg(feature = "brotli")]
        if encoding.eq_ignore_ascii_case("br") && accept.brotli {
            return Some(Ok(StreamDecoder::new_brotli()));
        }
        #[cfg(feature = "zstd")]
        if encoding.eq_ignore_ascii_case("zstd") && accept.zstd {
            return Some(StreamDecoder::new_zstd());
        }
        None
    }

    pub(super) fn decompress_impl(
        headers: &mut HeaderMap,
        body: RequestBodySend,
        accept: &AcceptEncoding,
    ) -> RequestBodySend {
        let encoding_str = match headers.get(CONTENT_ENCODING) {
            Some(v) => String::from_utf8_lossy(v.as_bytes()).into_owned(),
            None => return body,
        };

        let encodings: Vec<&str> = encoding_str
            .split(',')
            .map(str::trim)
            .filter(|e| !e.eq_ignore_ascii_case("identity") && !e.is_empty())
            .collect();

        if encodings.is_empty() {
            return body;
        }

        let mut current_body = body;
        let mut decoded_count = 0;

        for encoding in encodings.iter().rev() {
            match make_decoder(encoding, accept) {
                Some(Ok(decoder)) => {
                    decoded_count += 1;
                    let decompress = DecompressBody {
                        body: current_body,
                        decoder: Some(decoder),
                        finished: false,
                        has_data: false,
                    };
                    current_body = decompress.boxed_unsync();
                }
                Some(Err(_)) => return current_body,
                None => break,
            }
        }

        if decoded_count > 0 {
            headers.remove(CONTENT_LENGTH);
            if decoded_count >= encodings.len() {
                headers.remove(CONTENT_ENCODING);
            } else {
                let remaining = &encodings[..encodings.len() - decoded_count];
                if let Ok(val) = remaining.join(", ").parse() {
                    headers.insert(CONTENT_ENCODING, val);
                }
            }
        }

        current_body
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
    use http::header::ACCEPT_ENCODING;

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

    #[cfg(feature = "gzip")]
    #[test]
    fn accept_encoding_includes_gzip() {
        let ae = AcceptEncoding::default();
        assert!(ae.gzip);
        let hv = ae.header_value().unwrap();
        let val = hv.to_str().unwrap();
        assert!(val.contains("gzip"));
    }

    #[cfg(all(feature = "gzip", feature = "tokio"))]
    #[tokio::test]
    async fn maybe_decompress_gzip_round_trip() {
        use bytes::Bytes;
        use flate2::Compression;
        use flate2::write::GzEncoder;
        use http::header::{CONTENT_ENCODING, CONTENT_LENGTH};
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

        let body: RequestBodySend = http_body_util::Full::new(Bytes::from(compressed))
            .map_err(|never| match never {})
            .boxed_unsync();
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

    #[cfg(all(feature = "deflate", feature = "tokio"))]
    #[tokio::test]
    async fn maybe_decompress_deflate_round_trip() {
        use bytes::Bytes;
        use flate2::Compression;
        use flate2::write::ZlibEncoder;
        use http::header::{CONTENT_ENCODING, CONTENT_LENGTH};
        use http_body_util::BodyExt;
        use std::io::Write;

        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::fast());
        encoder.write_all(b"hello deflate").unwrap();
        let compressed = encoder.finish().unwrap();

        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_ENCODING, "deflate".parse().unwrap());
        headers.insert(
            CONTENT_LENGTH,
            compressed.len().to_string().parse().unwrap(),
        );

        let body: RequestBodySend = http_body_util::Full::new(Bytes::from(compressed))
            .map_err(|never| match never {})
            .boxed_unsync();
        let ae = AcceptEncoding::default();
        let result_body = maybe_decompress(&mut headers, body, &ae);

        assert!(!headers.contains_key(CONTENT_ENCODING));
        assert!(!headers.contains_key(CONTENT_LENGTH));

        let collected = result_body.collect().await.unwrap().to_bytes();
        assert_eq!(&collected[..], b"hello deflate");
    }

    #[cfg(all(feature = "brotli", feature = "tokio"))]
    #[tokio::test]
    async fn maybe_decompress_brotli_round_trip() {
        use bytes::Bytes;
        use http::header::{CONTENT_ENCODING, CONTENT_LENGTH};
        use http_body_util::BodyExt;
        use std::io::Write;

        let mut compressed = Vec::new();
        {
            let mut encoder = brotli::CompressorWriter::new(&mut compressed, 4096, 1, 22);
            encoder.write_all(b"hello brotli").unwrap();
        }

        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_ENCODING, "br".parse().unwrap());
        headers.insert(
            CONTENT_LENGTH,
            compressed.len().to_string().parse().unwrap(),
        );

        let body: RequestBodySend = http_body_util::Full::new(Bytes::from(compressed))
            .map_err(|never| match never {})
            .boxed_unsync();
        let ae = AcceptEncoding::default();
        let result_body = maybe_decompress(&mut headers, body, &ae);

        assert!(!headers.contains_key(CONTENT_ENCODING));

        let collected = result_body.collect().await.unwrap().to_bytes();
        assert_eq!(&collected[..], b"hello brotli");
    }

    #[cfg(all(feature = "zstd", feature = "tokio"))]
    #[tokio::test]
    async fn maybe_decompress_zstd_round_trip() {
        use bytes::Bytes;
        use http::header::{CONTENT_ENCODING, CONTENT_LENGTH};
        use http_body_util::BodyExt;

        let data = b"hello zstd";
        let compressed = zstd::encode_all(&data[..], 1).unwrap();

        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_ENCODING, "zstd".parse().unwrap());
        headers.insert(
            CONTENT_LENGTH,
            compressed.len().to_string().parse().unwrap(),
        );

        let body: RequestBodySend = http_body_util::Full::new(Bytes::from(compressed))
            .map_err(|never| match never {})
            .boxed_unsync();
        let ae = AcceptEncoding::default();
        let result_body = maybe_decompress(&mut headers, body, &ae);

        assert!(!headers.contains_key(CONTENT_ENCODING));

        let collected = result_body.collect().await.unwrap().to_bytes();
        assert_eq!(&collected[..], b"hello zstd");
    }

    #[cfg(all(feature = "gzip", feature = "tokio"))]
    #[tokio::test]
    async fn maybe_decompress_empty_gzip_body() {
        use http::header::CONTENT_ENCODING;
        use http_body_util::BodyExt;

        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_ENCODING, "gzip".parse().unwrap());

        let body: RequestBodySend = http_body_util::Empty::new()
            .map_err(|never| match never {})
            .boxed_unsync();
        let ae = AcceptEncoding::default();
        let result_body = maybe_decompress(&mut headers, body, &ae);

        let collected = result_body.collect().await.unwrap().to_bytes();
        assert!(collected.is_empty());
    }

    #[cfg(feature = "tokio")]
    #[tokio::test]
    async fn maybe_decompress_unknown_encoding_passthrough() {
        use bytes::Bytes;
        use http::header::CONTENT_ENCODING;
        use http_body_util::BodyExt;

        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_ENCODING, "identity".parse().unwrap());

        let body: RequestBodySend = http_body_util::Full::new(Bytes::from("raw"))
            .map_err(|never| match never {})
            .boxed_unsync();
        let ae = AcceptEncoding::default();
        let result_body = maybe_decompress(&mut headers, body, &ae);

        assert!(headers.contains_key(CONTENT_ENCODING));
        let collected = result_body.collect().await.unwrap().to_bytes();
        assert_eq!(&collected[..], b"raw");
    }

    #[cfg(all(feature = "gzip", feature = "tokio"))]
    #[tokio::test]
    async fn maybe_decompress_gzip_disabled_passthrough() {
        use bytes::Bytes;
        use http::header::CONTENT_ENCODING;
        use http_body_util::BodyExt;

        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_ENCODING, "gzip".parse().unwrap());

        let raw = b"not actually gzip";
        let body: RequestBodySend = http_body_util::Full::new(Bytes::from(&raw[..]))
            .map_err(|never| match never {})
            .boxed_unsync();
        let ae = AcceptEncoding {
            gzip: false,
            ..AcceptEncoding::default()
        };
        let result_body = maybe_decompress(&mut headers, body, &ae);

        assert!(headers.contains_key(CONTENT_ENCODING));
        let collected = result_body.collect().await.unwrap().to_bytes();
        assert_eq!(&collected[..], raw);
    }

    #[cfg(all(feature = "gzip", feature = "tokio"))]
    #[tokio::test]
    async fn maybe_decompress_gzip_finish_with_no_remaining() {
        // Test the path where decoder.finish() returns an empty Vec (line 309)
        // This happens when all decompressed output was already flushed during
        // intermediate write_chunk + take_output calls.
        use bytes::Bytes;
        use flate2::Compression;
        use flate2::write::GzEncoder;
        use http::header::CONTENT_ENCODING;
        use http_body_util::BodyExt;
        use std::io::Write;

        // Create a large enough payload that gzip will flush during streaming
        let data = "A".repeat(65536);
        let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
        encoder.write_all(data.as_bytes()).unwrap();
        let compressed = encoder.finish().unwrap();

        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_ENCODING, "gzip".parse().unwrap());

        let body: RequestBodySend = http_body_util::Full::new(Bytes::from(compressed))
            .map_err(|never| match never {})
            .boxed_unsync();
        let ae = AcceptEncoding::default();
        let result_body = maybe_decompress(&mut headers, body, &ae);

        let collected = result_body.collect().await.unwrap().to_bytes();
        assert_eq!(collected.len(), 65536);
        assert!(collected.iter().all(|&b| b == b'A'));
    }

    #[cfg(all(feature = "deflate", feature = "tokio"))]
    #[tokio::test]
    async fn maybe_decompress_corrupt_deflate_returns_error() {
        use bytes::Bytes;
        use http::header::CONTENT_ENCODING;
        use http_body_util::BodyExt;

        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_ENCODING, "deflate".parse().unwrap());

        let body: RequestBodySend =
            http_body_util::Full::new(Bytes::from(vec![0xFF, 0xFF, 0xFF, 0xFF, 0xFF]))
                .map_err(|never| match never {})
                .boxed_unsync();
        let ae = AcceptEncoding::default();
        let result_body = maybe_decompress(&mut headers, body, &ae);

        let result = result_body.collect().await;
        assert!(
            result.is_err(),
            "corrupt deflate data should produce an error"
        );
    }

    #[cfg(all(feature = "brotli", feature = "tokio"))]
    #[tokio::test]
    async fn maybe_decompress_corrupt_brotli_returns_error() {
        use bytes::Bytes;
        use http::header::CONTENT_ENCODING;
        use http_body_util::BodyExt;

        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_ENCODING, "br".parse().unwrap());

        let body: RequestBodySend =
            http_body_util::Full::new(Bytes::from(vec![0xFF, 0xFF, 0xFF, 0xFF, 0xFF]))
                .map_err(|never| match never {})
                .boxed_unsync();
        let ae = AcceptEncoding::default();
        let result_body = maybe_decompress(&mut headers, body, &ae);

        let result = result_body.collect().await;
        assert!(
            result.is_err(),
            "corrupt brotli data should produce an error"
        );
    }

    #[cfg(all(feature = "gzip", feature = "tokio"))]
    #[tokio::test]
    async fn maybe_decompress_body_with_trailers_frame() {
        // Test the path where a frame is NOT data (e.g., trailers frame)
        // This should hit line 290-291 (cx.waker().wake_by_ref() + Pending)
        use bytes::Bytes;
        use flate2::Compression;
        use flate2::write::GzEncoder;
        use http::header::CONTENT_ENCODING;
        use http_body_util::BodyExt;
        use std::io::Write;
        use std::pin::Pin;
        use std::task::{Context, Poll};

        // A custom body that yields a data frame, then a trailers frame, then ends
        struct TrailersBody {
            state: u8,
            data: Bytes,
        }

        impl http_body::Body for TrailersBody {
            type Data = Bytes;
            type Error = crate::error::Error;

            fn poll_frame(
                mut self: Pin<&mut Self>,
                _cx: &mut Context<'_>,
            ) -> Poll<Option<Result<hyper::body::Frame<Bytes>, Self::Error>>> {
                match self.state {
                    0 => {
                        self.state = 1;
                        let data = self.data.clone();
                        Poll::Ready(Some(Ok(hyper::body::Frame::data(data))))
                    }
                    1 => {
                        self.state = 2;
                        // Emit a trailers frame
                        let mut trailers = http::HeaderMap::new();
                        trailers.insert("x-checksum", "abc123".parse().unwrap());
                        Poll::Ready(Some(Ok(hyper::body::Frame::trailers(trailers))))
                    }
                    _ => Poll::Ready(None),
                }
            }
        }

        let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
        encoder.write_all(b"with trailers").unwrap();
        let compressed = encoder.finish().unwrap();

        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_ENCODING, "gzip".parse().unwrap());

        let body: RequestBodySend = TrailersBody {
            state: 0,
            data: Bytes::from(compressed),
        }
        .boxed_unsync();
        let ae = AcceptEncoding::default();
        let result_body = maybe_decompress(&mut headers, body, &ae);

        let collected = result_body.collect().await.unwrap().to_bytes();
        assert_eq!(&collected[..], b"with trailers");
    }

    #[cfg(all(feature = "gzip", feature = "tokio"))]
    #[tokio::test]
    async fn maybe_decompress_upstream_body_error() {
        // Test the path where the upstream body returns an error (lines 294-296)
        use bytes::Bytes;
        use http::header::CONTENT_ENCODING;
        use http_body_util::BodyExt;
        use std::pin::Pin;
        use std::task::{Context, Poll};

        struct ErrorBody {
            errored: bool,
        }

        impl http_body::Body for ErrorBody {
            type Data = Bytes;
            type Error = crate::error::Error;

            fn poll_frame(
                mut self: Pin<&mut Self>,
                _cx: &mut Context<'_>,
            ) -> Poll<Option<Result<hyper::body::Frame<Bytes>, Self::Error>>> {
                if !self.errored {
                    self.errored = true;
                    Poll::Ready(Some(Err(crate::error::Error::Io(std::io::Error::new(
                        std::io::ErrorKind::BrokenPipe,
                        "simulated upstream error",
                    )))))
                } else {
                    Poll::Ready(None)
                }
            }
        }

        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_ENCODING, "gzip".parse().unwrap());

        let body: RequestBodySend = ErrorBody { errored: false }.boxed_unsync();
        let ae = AcceptEncoding::default();
        let result_body = maybe_decompress(&mut headers, body, &ae);

        let result = result_body.collect().await;
        assert!(result.is_err(), "upstream body error should propagate");
    }

    #[cfg(all(feature = "gzip", feature = "tokio"))]
    #[tokio::test]
    async fn maybe_decompress_multi_encoding_with_identity() {
        use bytes::Bytes;
        use flate2::Compression;
        use flate2::write::GzEncoder;
        use http::header::CONTENT_ENCODING;
        use http_body_util::BodyExt;
        use std::io::Write;

        let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
        encoder.write_all(b"multi-encoding").unwrap();
        let compressed = encoder.finish().unwrap();

        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_ENCODING, "identity, gzip".parse().unwrap());

        let body: RequestBodySend = http_body_util::Full::new(Bytes::from(compressed))
            .map_err(|never| match never {})
            .boxed_unsync();
        let ae = AcceptEncoding::default();
        let result_body = maybe_decompress(&mut headers, body, &ae);

        let collected = result_body.collect().await.unwrap().to_bytes();
        assert_eq!(&collected[..], b"multi-encoding");
    }

    #[cfg(all(feature = "gzip", feature = "tokio"))]
    #[tokio::test]
    async fn maybe_decompress_corrupt_gzip_returns_error() {
        use bytes::Bytes;
        use http::header::CONTENT_ENCODING;
        use http_body_util::BodyExt;

        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_ENCODING, "gzip".parse().unwrap());

        let body: RequestBodySend =
            http_body_util::Full::new(Bytes::from(vec![0xFF, 0xFF, 0xFF, 0xFF, 0xFF]))
                .map_err(|never| match never {})
                .boxed_unsync();
        let ae = AcceptEncoding::default();
        let result_body = maybe_decompress(&mut headers, body, &ae);

        let result = result_body.collect().await;
        assert!(result.is_err(), "corrupt gzip data should produce an error");
    }

    #[cfg(all(feature = "gzip", feature = "deflate", feature = "tokio"))]
    #[tokio::test]
    async fn maybe_decompress_double_layer_deflate_then_gzip() {
        use bytes::Bytes;
        use flate2::Compression;
        use flate2::write::{GzEncoder, ZlibEncoder};
        use http::header::{CONTENT_ENCODING, CONTENT_LENGTH};
        use http_body_util::BodyExt;
        use std::io::Write;

        let original = b"double-compressed payload";

        // First compress with deflate, then gzip the result
        let mut deflate_enc = ZlibEncoder::new(Vec::new(), Compression::fast());
        deflate_enc.write_all(original).unwrap();
        let deflated = deflate_enc.finish().unwrap();

        let mut gzip_enc = GzEncoder::new(Vec::new(), Compression::fast());
        gzip_enc.write_all(&deflated).unwrap();
        let compressed = gzip_enc.finish().unwrap();

        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_ENCODING, "deflate, gzip".parse().unwrap());
        headers.insert(
            CONTENT_LENGTH,
            compressed.len().to_string().parse().unwrap(),
        );

        let body: RequestBodySend = http_body_util::Full::new(Bytes::from(compressed))
            .map_err(|never| match never {})
            .boxed_unsync();
        let ae = AcceptEncoding::default();
        let result_body = maybe_decompress(&mut headers, body, &ae);

        assert!(!headers.contains_key(CONTENT_ENCODING));
        assert!(!headers.contains_key(CONTENT_LENGTH));

        let collected = result_body.collect().await.unwrap().to_bytes();
        assert_eq!(&collected[..], original);
    }

    #[cfg(all(feature = "gzip", feature = "tokio"))]
    #[tokio::test]
    async fn maybe_decompress_partial_decode_preserves_remaining_header() {
        use bytes::Bytes;
        use flate2::Compression;
        use flate2::write::GzEncoder;
        use http::header::CONTENT_ENCODING;
        use http_body_util::BodyExt;
        use std::io::Write;

        // Simulate: Content-Encoding: custom-enc, gzip
        // gzip is decoded but custom-enc is unknown — only gzip removed from header
        let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
        encoder.write_all(b"partial test").unwrap();
        let compressed = encoder.finish().unwrap();

        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_ENCODING, "custom-enc, gzip".parse().unwrap());

        let body: RequestBodySend = http_body_util::Full::new(Bytes::from(compressed))
            .map_err(|never| match never {})
            .boxed_unsync();
        let ae = AcceptEncoding::default();
        let result_body = maybe_decompress(&mut headers, body, &ae);

        // custom-enc should remain in the header
        let remaining = headers.get(CONTENT_ENCODING).unwrap().to_str().unwrap();
        assert_eq!(remaining, "custom-enc");

        let collected = result_body.collect().await.unwrap().to_bytes();
        assert_eq!(&collected[..], b"partial test");
    }

    /// #135: Decompression should not return Poll::Pending + self-wake when
    /// the decoder buffers data without producing output.
    ///
    /// When the inner body yields data that the decoder buffers without output,
    /// poll_frame should loop internally to pull more data rather than returning
    /// Pending and relying on a self-wake.
    #[cfg(all(feature = "gzip", feature = "tokio"))]
    #[tokio::test]
    async fn no_spurious_pending_on_buffered_decode() {
        use bytes::Bytes;
        use flate2::Compression;
        use flate2::write::GzEncoder;
        use http::header::{CONTENT_ENCODING, CONTENT_LENGTH};
        use http_body::Body;
        use http_body_util::BodyExt;
        use std::io::Write;
        use std::pin::Pin;
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::task::Poll;

        let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
        encoder
            .write_all(b"hello world! this is test data for decompression")
            .unwrap();
        let compressed = encoder.finish().unwrap();

        // Split into 1-byte chunks — early chunks (gzip header) won't produce output.
        let chunks: Vec<Bytes> = compressed.iter().map(|&b| Bytes::from(vec![b])).collect();
        let chunk_count = chunks.len();
        let chunks_iter = Arc::new(std::sync::Mutex::new(chunks.into_iter()));

        let wake_count = Arc::new(AtomicUsize::new(0));
        let wake_count2 = Arc::clone(&wake_count);

        let body = futures_util::stream::poll_fn(
            move |_cx| -> Poll<Option<Result<hyper::body::Frame<Bytes>, crate::error::Error>>> {
                match chunks_iter.lock().unwrap().next() {
                    Some(chunk) => Poll::Ready(Some(Ok(hyper::body::Frame::data(chunk)))),
                    None => Poll::Ready(None),
                }
            },
        );
        let body: RequestBodySend = http_body_util::StreamBody::new(body)
            .map_err(|e| e)
            .boxed_unsync();

        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_ENCODING, "gzip".parse().unwrap());
        headers.insert(CONTENT_LENGTH, chunk_count.to_string().parse().unwrap());

        let ae = AcceptEncoding::default();
        let result_body = maybe_decompress(&mut headers, body, &ae);

        // Use a custom waker to count spurious wakes.
        let waker = {
            use std::task::{RawWaker, RawWakerVTable, Waker};
            let data = Arc::into_raw(Arc::clone(&wake_count2)) as *const ();
            unsafe fn clone_fn(data: *const ()) -> RawWaker {
                unsafe { Arc::increment_strong_count(data as *const AtomicUsize) };
                RawWaker::new(data, &VTABLE)
            }
            unsafe fn wake_fn(data: *const ()) {
                let arc = unsafe { Arc::from_raw(data as *const AtomicUsize) };
                arc.fetch_add(1, Ordering::SeqCst);
            }
            unsafe fn wake_by_ref_fn(data: *const ()) {
                unsafe { &*(data as *const AtomicUsize) }.fetch_add(1, Ordering::SeqCst);
            }
            unsafe fn drop_fn(data: *const ()) {
                unsafe { Arc::decrement_strong_count(data as *const AtomicUsize) };
            }
            static VTABLE: RawWakerVTable =
                RawWakerVTable::new(clone_fn, wake_fn, wake_by_ref_fn, drop_fn);
            unsafe { Waker::from_raw(RawWaker::new(data, &VTABLE)) }
        };

        let mut cx = std::task::Context::from_waker(&waker);
        let mut body: Pin<Box<RequestBodySend>> = Box::pin(result_body);
        let mut output = Vec::new();
        let mut pending_count = 0usize;

        loop {
            match body.as_mut().poll_frame(&mut cx) {
                Poll::Ready(Some(Ok(frame))) => {
                    if let Ok(data) = frame.into_data() {
                        output.extend_from_slice(&data[..]);
                    }
                }
                Poll::Ready(Some(Err(e))) => panic!("unexpected error: {e}"),
                Poll::Ready(None) => break,
                Poll::Pending => {
                    pending_count += 1;
                    if pending_count > 1000 {
                        panic!("too many Pending returns — likely busy-spin bug");
                    }
                }
            }
        }

        assert_eq!(output, b"hello world! this is test data for decompression");
        // With the fix: poll_frame should never return Pending because the inner
        // body always has data ready. It should loop internally until output is
        // produced or the inner body returns Pending/None.
        assert_eq!(
            pending_count, 0,
            "poll_frame returned Pending {pending_count} times — should loop internally when inner body has data"
        );
    }
}
