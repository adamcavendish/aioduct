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
    /// Maximum decompressed body size in bytes. `None` means unlimited.
    pub max_decoded_size: Option<u64>,
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
            max_decoded_size: None,
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
            max_decoded_size: None,
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
        total_decoded: u64,
        max_decoded_size: Option<u64>,
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
                                    self.total_decoded += output.len() as u64;
                                    if let Some(max) = self.max_decoded_size
                                        && self.total_decoded > max
                                    {
                                        self.finished = true;
                                        return Poll::Ready(Some(Err(Error::Other(
                                            format!(
                                                "decompressed body exceeds max size of {max} bytes"
                                            )
                                            .into(),
                                        ))));
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
                                Ok(remaining) => {
                                    if !remaining.is_empty() {
                                        self.total_decoded += remaining.len() as u64;
                                        if let Some(max) = self.max_decoded_size
                                            && self.total_decoded > max
                                        {
                                            return Poll::Ready(Some(Err(Error::Other(
                                                format!(
                                                    "decompressed body exceeds max size of {max} bytes"
                                                )
                                                .into(),
                                            ))));
                                        }
                                        Poll::Ready(Some(Ok(hyper::body::Frame::data(
                                            Bytes::from(remaining),
                                        ))))
                                    } else {
                                        Poll::Ready(None)
                                    }
                                }
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
                        total_decoded: 0,
                        max_decoded_size: accept.max_decoded_size,
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
mod tests;
