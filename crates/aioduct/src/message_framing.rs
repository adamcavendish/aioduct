#[cfg(all(test, feature = "tokio"))]
use std::pin::Pin;
#[cfg(all(test, feature = "tokio"))]
use std::task::{Context, Poll};

#[cfg(all(test, feature = "tokio"))]
use bytes::{Buf, Bytes};
use http::header::{CONTENT_LENGTH, HeaderMap, HeaderValue, TRANSFER_ENCODING};
#[cfg(all(test, feature = "tokio"))]
use http_body::Body;
#[cfg(all(test, feature = "tokio"))]
use http_body::Frame;

use crate::error::Error;

pub(crate) fn normalize_content_length(
    headers: &mut HeaderMap,
    context: &str,
) -> Result<Option<u64>, Error> {
    let parsed = parse_content_length(headers, context)?;

    let Some(length) = parsed else {
        return Ok(None);
    };
    headers.remove(CONTENT_LENGTH);
    headers.insert(
        CONTENT_LENGTH,
        HeaderValue::try_from(length.to_string()).map_err(|error| {
            Error::InvalidHeader(format!(
                "invalid normalized {context} Content-Length: {error}"
            ))
        })?,
    );
    Ok(Some(length))
}

fn parse_content_length(headers: &HeaderMap, context: &str) -> Result<Option<u64>, Error> {
    let mut parsed = None;
    for value in headers.get_all(CONTENT_LENGTH) {
        let value = value.to_str().map_err(|error| {
            Error::InvalidHeader(format!("invalid {context} Content-Length: {error}"))
        })?;
        for item in value.split(',') {
            let item = item.trim_matches([' ', '\t']);
            if item.is_empty() || !item.bytes().all(|byte| byte.is_ascii_digit()) {
                return Err(Error::InvalidHeader(format!(
                    "invalid {context} Content-Length `{value}`"
                )));
            }
            let length = item.parse::<u64>().map_err(|_| {
                Error::InvalidHeader(format!("invalid {context} Content-Length `{value}`"))
            })?;
            if parsed.is_some_and(|previous| previous != length) {
                return Err(Error::InvalidHeader(format!(
                    "conflicting {context} Content-Length field values"
                )));
            }
            parsed = Some(length);
        }
    }

    Ok(parsed)
}

pub(crate) fn known_h1_content_length(headers: &HeaderMap) -> Option<u64> {
    if headers.contains_key(TRANSFER_ENCODING) {
        return None;
    }
    parse_content_length(headers, "request").ok().flatten()
}

pub(crate) fn validate_response_content_length(
    request_method: &http::Method,
    status: http::StatusCode,
    headers: &mut HeaderMap,
    context: &str,
) -> Result<Option<u64>, Error> {
    if *request_method == http::Method::CONNECT && status.is_success() {
        // RFC 7230 section 3.3.3 requires clients to ignore both fields on a
        // successful CONNECT response, including values that would otherwise
        // be invalid framing metadata.
        headers.remove(CONTENT_LENGTH);
        headers.remove(TRANSFER_ENCODING);
        return Ok(None);
    }

    let length = normalize_content_length(headers, context)?;
    if length.is_some() && (status.is_informational() || status == http::StatusCode::NO_CONTENT) {
        return Err(Error::InvalidHeader(format!(
            "{context} response must not contain Content-Length for {request_method} {status}"
        )));
    }
    if status == http::StatusCode::RESET_CONTENT && length.is_some_and(|length| length != 0) {
        return Err(Error::InvalidHeader(format!(
            "{context} 205 response Content-Length must be zero"
        )));
    }
    Ok(length)
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct BodyLengthValidator {
    expected: Option<u64>,
    seen: u64,
    finished: bool,
}

impl BodyLengthValidator {
    pub(crate) fn from_expected(expected: Option<u64>) -> Self {
        Self {
            expected,
            seen: 0,
            finished: false,
        }
    }

    pub(crate) fn exact(expected: u64) -> Self {
        Self::from_expected(Some(expected))
    }

    pub(crate) fn record(&mut self, count: usize, context: &str) -> Result<(), Error> {
        let count = u64::try_from(count)
            .map_err(|_| Error::InvalidHeader(format!("{context} body length overflow")))?;
        self.seen = self
            .seen
            .checked_add(count)
            .ok_or_else(|| Error::InvalidHeader(format!("{context} body length overflow")))?;
        if let Some(expected) = self.expected
            && self.seen > expected
        {
            return Err(Error::InvalidHeader(format!(
                "{context} body exceeds Content-Length {expected}"
            )));
        }
        Ok(())
    }

    pub(crate) fn finish(&mut self, context: &str) -> Result<(), Error> {
        if self.finished {
            return Ok(());
        }
        self.finished = true;
        if let Some(expected) = self.expected
            && self.seen != expected
        {
            return Err(Error::InvalidHeader(format!(
                "{context} body ended after {} bytes but Content-Length is {expected}",
                self.seen
            )));
        }
        Ok(())
    }

    pub(crate) fn is_end_stream(&self, inner_is_end_stream: bool) -> bool {
        self.finished
            || (inner_is_end_stream && self.expected.is_none_or(|expected| expected == self.seen))
    }

    pub(crate) fn size_hint(&self, inner: http_body::SizeHint) -> http_body::SizeHint {
        if self.finished {
            return http_body::SizeHint::with_exact(0);
        }
        self.expected
            .map(|expected| http_body::SizeHint::with_exact(expected.saturating_sub(self.seen)))
            .unwrap_or(inner)
    }
}

#[cfg(all(test, feature = "tokio"))]
pin_project_lite::pin_project! {
    pub(crate) struct ContentLengthBody<B> {
        #[pin]
        inner: B,
        validator: BodyLengthValidator,
        context: &'static str,
        terminal: bool,
    }
}

#[cfg(all(test, feature = "tokio"))]
impl<B> ContentLengthBody<B>
where
    B: Body<Data = Bytes, Error = Error>,
{
    pub(crate) fn new(inner: B, expected: u64, context: &'static str) -> Result<Self, Error> {
        if let Some(exact) = inner.size_hint().exact()
            && exact != expected
        {
            return Err(Error::InvalidHeader(format!(
                "{context} body length {exact} does not match Content-Length {expected}"
            )));
        }
        Ok(Self {
            inner,
            validator: BodyLengthValidator::exact(expected),
            context,
            terminal: false,
        })
    }
}

#[cfg(all(test, feature = "tokio"))]
impl<B> Body for ContentLengthBody<B>
where
    B: Body<Data = Bytes, Error = Error>,
{
    type Data = Bytes;
    type Error = Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        let mut this = self.project();
        if *this.terminal {
            return Poll::Ready(None);
        }
        match this.inner.as_mut().poll_frame(cx) {
            Poll::Ready(Some(Ok(frame))) => match frame.into_data() {
                Ok(data) => {
                    if let Err(error) = this.validator.record(data.remaining(), this.context) {
                        *this.terminal = true;
                        return Poll::Ready(Some(Err(error)));
                    }
                    Poll::Ready(Some(Ok(Frame::data(data))))
                }
                Err(frame) => match frame.into_trailers() {
                    Ok(trailers) => {
                        if let Err(error) = this.validator.finish(this.context) {
                            *this.terminal = true;
                            return Poll::Ready(Some(Err(error)));
                        }
                        Poll::Ready(Some(Ok(Frame::trailers(trailers))))
                    }
                    Err(_) => {
                        *this.terminal = true;
                        Poll::Ready(Some(Err(Error::Other(
                            "unsupported request body frame".into(),
                        ))))
                    }
                },
            },
            Poll::Ready(Some(Err(error))) => {
                *this.terminal = true;
                Poll::Ready(Some(Err(error)))
            }
            Poll::Ready(None) => {
                *this.terminal = true;
                match this.validator.finish(this.context) {
                    Ok(()) => Poll::Ready(None),
                    Err(error) => Poll::Ready(Some(Err(error))),
                }
            }
            Poll::Pending => Poll::Pending,
        }
    }

    fn is_end_stream(&self) -> bool {
        self.terminal || self.validator.is_end_stream(self.inner.is_end_stream())
    }

    fn size_hint(&self) -> http_body::SizeHint {
        self.validator.size_hint(self.inner.size_hint())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(feature = "tokio")]
    use http_body_util::BodyExt as _;

    fn headers(values: &[&str]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for value in values {
            headers.append(CONTENT_LENGTH, HeaderValue::try_from(*value).unwrap());
        }
        headers
    }

    #[test]
    fn normalizes_identical_content_length_values() {
        for values in [&["5", "5"][..], &["5, 5"][..], &["005", "5"][..]] {
            let mut headers = headers(values);
            assert_eq!(
                normalize_content_length(&mut headers, "test").unwrap(),
                Some(5)
            );
            assert_eq!(headers.get_all(CONTENT_LENGTH).iter().count(), 1);
            assert_eq!(headers[CONTENT_LENGTH], "5");
        }
    }

    #[test]
    fn known_h1_length_requires_unambiguous_content_length_framing() {
        assert_eq!(known_h1_content_length(&headers(&["5", "005"])), Some(5));

        let mut chunked = headers(&["5"]);
        chunked.insert(TRANSFER_ENCODING, HeaderValue::from_static("chunked"));
        assert_eq!(known_h1_content_length(&chunked), None);
        assert_eq!(known_h1_content_length(&headers(&["5", "6"])), None);
        assert_eq!(known_h1_content_length(&headers(&["+5"])), None);
    }

    #[test]
    fn rejects_malformed_conflicting_and_out_of_range_content_length() {
        for values in [
            &[""][..],
            &["5,"][..],
            &["+5"][..],
            &["5 6"][..],
            &["5", "6"][..],
            &["18446744073709551616"][..],
        ] {
            assert!(
                normalize_content_length(&mut headers(values), "test").is_err(),
                "accepted {values:?}"
            );
        }
    }

    #[test]
    fn enforces_response_content_length_restrictions() {
        for (method, status, value) in [
            (http::Method::GET, http::StatusCode::CONTINUE, "0"),
            (http::Method::GET, http::StatusCode::NO_CONTENT, "0"),
            (http::Method::GET, http::StatusCode::RESET_CONTENT, "1"),
        ] {
            assert!(
                validate_response_content_length(&method, status, &mut headers(&[value]), "test",)
                    .is_err(),
                "accepted {method} {status} Content-Length {value}"
            );
        }

        let mut head = headers(&["9", "9"]);
        assert_eq!(
            validate_response_content_length(
                &http::Method::HEAD,
                http::StatusCode::OK,
                &mut head,
                "test",
            )
            .unwrap(),
            Some(9)
        );
        assert_eq!(head[CONTENT_LENGTH], "9");
    }

    #[test]
    fn ignores_all_successful_connect_framing_fields() {
        let statuses = [
            http::StatusCode::OK,
            http::StatusCode::CREATED,
            http::StatusCode::NO_CONTENT,
            http::StatusCode::from_u16(299).unwrap(),
        ];
        let framing_cases = [
            ("malformed", &["invalid"][..], &["gzip,,chunked"][..]),
            ("conflicting", &["0", "7"][..], &["gzip", "chunked"][..]),
            ("zero", &["0"][..], &["chunked"][..]),
            ("nonzero", &["7"][..], &["chunked"][..]),
        ];

        for status in statuses {
            for (case, content_lengths, transfer_encodings) in framing_cases {
                let mut headers = HeaderMap::new();
                for value in content_lengths {
                    headers.append(CONTENT_LENGTH, HeaderValue::from_static(value));
                }
                for value in transfer_encodings {
                    headers.append(TRANSFER_ENCODING, HeaderValue::from_static(value));
                }

                assert_eq!(
                    validate_response_content_length(
                        &http::Method::CONNECT,
                        status,
                        &mut headers,
                        "test",
                    )
                    .unwrap(),
                    None,
                    "accepted framing semantics for {status} {case}",
                );
                assert!(
                    !headers.contains_key(CONTENT_LENGTH),
                    "retained Content-Length for {status} {case}",
                );
                assert!(
                    !headers.contains_key(TRANSFER_ENCODING),
                    "retained Transfer-Encoding for {status} {case}",
                );
            }
        }
    }

    #[cfg(feature = "tokio")]
    #[tokio::test]
    async fn content_length_body_accepts_exact_and_rejects_short_or_long_streams() {
        let exact =
            http_body_util::Full::new(Bytes::from_static(b"body")).map_err(|never| match never {});
        let exact = ContentLengthBody::new(exact, 4, "test request").unwrap();
        assert_eq!(exact.collect().await.unwrap().to_bytes(), "body");

        let short = http_body_util::StreamBody::new(futures_util::stream::iter([Ok::<_, Error>(
            Frame::data(Bytes::from_static(b"short")),
        )]));
        let mut short = Box::pin(ContentLengthBody::new(short, 6, "test request").unwrap());
        assert!(!short.is_end_stream());
        assert!(
            std::future::poll_fn(|cx| short.as_mut().poll_frame(cx))
                .await
                .unwrap()
                .is_ok()
        );
        let error = std::future::poll_fn(|cx| short.as_mut().poll_frame(cx))
            .await
            .unwrap()
            .unwrap_err();
        assert!(error.to_string().contains("ended after 5 bytes"));
        assert!(
            std::future::poll_fn(|cx| short.as_mut().poll_frame(cx))
                .await
                .is_none()
        );

        let chunks = futures_util::stream::iter([Ok::<_, Error>(Frame::data(Bytes::from_static(
            b"too long",
        )))]);
        let long = http_body_util::StreamBody::new(chunks);
        let error = ContentLengthBody::new(long, 3, "test request")
            .unwrap()
            .collect()
            .await
            .unwrap_err();
        assert!(error.to_string().contains("exceeds Content-Length 3"));
    }

    #[test]
    fn body_length_validator_uses_declared_length_for_size_and_end_stream() {
        let validator = BodyLengthValidator::exact(3);
        assert_eq!(
            validator.size_hint(http_body::SizeHint::new()).exact(),
            Some(3)
        );
        assert!(!validator.is_end_stream(true));

        let mut validator = validator;
        validator.record(3, "test").unwrap();
        assert!(validator.is_end_stream(true));
        assert_eq!(
            validator.size_hint(http_body::SizeHint::new()).exact(),
            Some(0)
        );
        validator.finish("test").unwrap();
        assert!(validator.is_end_stream(false));
        validator.finish("test").unwrap();
    }
}
