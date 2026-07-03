use std::error::Error as StdError;
use std::fmt;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Instant;

use bytes::Bytes;
use http_body::{Body, Frame};
use pin_project_lite::pin_project;
use wasmtime_wasi_http::p2::bindings::http::types::ErrorCode;

use crate::policy::{
    ExactOriginPolicy, RejectionObserver, RejectionReason, RequestTrailerPolicy,
    RequestTrailerPolicyError, header_section_size, limit_to_u32, notify_rejection_once,
};

pub(crate) fn map_wasi_body_error(code: ErrorCode) -> aioduct::Error {
    aioduct::Error::Other(Box::new(WasiOutgoingBodyError { code }))
}

pub(crate) fn map_aioduct_error(error: aioduct::Error) -> ErrorCode {
    if let Some(code) = timeout_code_from_aioduct_error(&error) {
        return code;
    }
    if let Some(code) = request_trailer_policy_error_code_from_error(&error) {
        return code;
    }

    match error {
        aioduct::Error::InvalidUrl(_) => ErrorCode::HttpRequestUriInvalid,
        aioduct::Error::HttpsOnly(_) => ErrorCode::HttpRequestDenied,
        aioduct::Error::Tls(_) => ErrorCode::TlsProtocolError,
        aioduct::Error::Hyper(error) => {
            if let Some(code) = request_trailer_policy_error_code_from_error(&error) {
                code
            } else if let Some(limit) = request_body_limit_from_error(&error) {
                ErrorCode::HttpRequestBodySize(Some(limit))
            } else if let Some(code) = wasi_body_error_from_error(&error) {
                code
            } else {
                ErrorCode::HttpProtocolError
            }
        }
        aioduct::Error::Pool(_) => ErrorCode::ConnectionLimitReached,
        aioduct::Error::Io(error) => io_error_code(&error),
        aioduct::Error::Other(source) => {
            if let Some(code) = request_trailer_policy_error_code_from_error(source.as_ref()) {
                code
            } else if let Some(limit) = request_body_limit_from_error(source.as_ref()) {
                ErrorCode::HttpRequestBodySize(Some(limit))
            } else if let Some(code) = wasi_body_error_from_error(source.as_ref()) {
                code
            } else {
                ErrorCode::InternalError(Some("transport".into()))
            }
        }
        aioduct::Error::RemoteAddr { source, .. } => {
            if let Some(error) = source.downcast_ref::<std::io::Error>() {
                io_error_code(error)
            } else {
                ErrorCode::DestinationUnavailable
            }
        }
        _ => ErrorCode::InternalError(Some("transport".into())),
    }
}

pub(crate) fn timeout_code_from_aioduct_error(error: &aioduct::Error) -> Option<ErrorCode> {
    match error {
        aioduct::Error::Timeout => Some(ErrorCode::HttpResponseTimeout),
        aioduct::Error::ConnectTimeout => Some(ErrorCode::ConnectionTimeout),
        aioduct::Error::ReadTimeout => Some(ErrorCode::ConnectionReadTimeout),
        aioduct::Error::WriteTimeout => Some(ErrorCode::ConnectionWriteTimeout),
        aioduct::Error::Hyper(error) => timeout_code_from_error(error),
        aioduct::Error::Other(source) => timeout_code_from_error(source.as_ref()),
        aioduct::Error::RemoteAddr { source, .. } => timeout_code_from_error(source.as_ref()),
        _ => None,
    }
}

fn timeout_code_from_error(error: &(dyn StdError + 'static)) -> Option<ErrorCode> {
    let mut current = Some(error);
    while let Some(error) = current {
        if let Some(error) = error.downcast_ref::<aioduct::Error>()
            && let Some(code) = timeout_code_from_aioduct_error(error)
        {
            return Some(code);
        }
        if let Some(error) = error.downcast_ref::<std::io::Error>()
            && error.kind() == std::io::ErrorKind::TimedOut
        {
            return Some(ErrorCode::ConnectionTimeout);
        }
        current = error.source();
    }
    None
}

fn io_error_code(error: &std::io::Error) -> ErrorCode {
    match error.kind() {
        std::io::ErrorKind::ConnectionRefused => ErrorCode::ConnectionRefused,
        std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::BrokenPipe => {
            ErrorCode::ConnectionTerminated
        }
        std::io::ErrorKind::TimedOut => ErrorCode::ConnectionTimeout,
        std::io::ErrorKind::NotFound => ErrorCode::DestinationNotFound,
        _ => ErrorCode::DestinationUnavailable,
    }
}

pub(crate) fn request_body_limit_from_error(error: &(dyn StdError + 'static)) -> Option<u64> {
    let mut current = Some(error);
    while let Some(error) = current {
        if let Some(limit) = error.downcast_ref::<RequestBodyLimitExceeded>() {
            return Some(limit.limit);
        }
        if let Some(aioduct::Error::Other(source)) = error.downcast_ref::<aioduct::Error>()
            && let Some(limit) = source.downcast_ref::<RequestBodyLimitExceeded>()
        {
            return Some(limit.limit);
        }
        current = error.source();
    }
    None
}

fn request_trailer_policy_error_code_from_error(
    error: &(dyn StdError + 'static),
) -> Option<ErrorCode> {
    let mut current = Some(error);
    while let Some(error) = current {
        if let Some(error) = error.downcast_ref::<RequestTrailerPolicyError>() {
            return Some(error.to_error_code());
        }
        if let Some(aioduct::Error::Other(source)) = error.downcast_ref::<aioduct::Error>()
            && let Some(error) = source.downcast_ref::<RequestTrailerPolicyError>()
        {
            return Some(error.to_error_code());
        }
        current = error.source();
    }
    None
}

fn wasi_body_error_from_error(error: &(dyn StdError + 'static)) -> Option<ErrorCode> {
    let mut current = Some(error);
    while let Some(error) = current {
        if let Some(error) = error.downcast_ref::<WasiOutgoingBodyError>() {
            return Some(error.code.clone());
        }
        if let Some(aioduct::Error::Other(source)) = error.downcast_ref::<aioduct::Error>()
            && let Some(error) = source.downcast_ref::<WasiOutgoingBodyError>()
        {
            return Some(error.code.clone());
        }
        current = error.source();
    }
    None
}

#[derive(Debug)]
struct WasiOutgoingBodyError {
    code: ErrorCode,
}

impl fmt::Display for WasiOutgoingBodyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "WASI outgoing body error: {:?}", self.code)
    }
}

impl std::error::Error for WasiOutgoingBodyError {}

#[derive(Debug)]
struct RequestBodyLimitExceeded {
    limit: u64,
}

impl fmt::Display for RequestBodyLimitExceeded {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "WASI request body exceeded limit {}", self.limit)
    }
}

impl std::error::Error for RequestBodyLimitExceeded {}

pin_project! {
    pub(crate) struct RequestLimitBody<B> {
        #[pin]
        inner: B,
        body_limit: Option<u64>,
        seen: u64,
        trailer_policy: Option<RequestTrailerPolicy>,
        rejection_observer: Option<RejectionObserver>,
        rejected: bool,
    }
}

impl<B> RequestLimitBody<B> {
    pub(crate) fn new_policy(
        inner: B,
        body_limit: Option<u64>,
        policy: &ExactOriginPolicy,
    ) -> Self {
        Self {
            inner,
            body_limit,
            seen: 0,
            trailer_policy: Some(RequestTrailerPolicy::from_policy(policy)),
            rejection_observer: policy.rejection_observer.clone(),
            rejected: false,
        }
    }
}

impl<B> Body for RequestLimitBody<B>
where
    B: Body<Data = Bytes, Error = aioduct::Error>,
{
    type Data = Bytes;
    type Error = aioduct::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        let mut this = self.project();
        match this.inner.as_mut().poll_frame(cx) {
            Poll::Ready(Some(Ok(frame))) => {
                if let Some(data) = frame.data_ref()
                    && let Some(limit) = this.body_limit
                {
                    let len = u64::try_from(data.len()).unwrap_or(u64::MAX);
                    if this.seen.saturating_add(len) > *limit {
                        notify_rejection_once(
                            this.rejection_observer,
                            this.rejected,
                            RejectionReason::BodyLimit,
                        );
                        return Poll::Ready(Some(Err(aioduct::Error::Other(Box::new(
                            RequestBodyLimitExceeded { limit: *limit },
                        )))));
                    }
                    *this.seen = this.seen.saturating_add(len);
                }
                if let Some(trailers) = frame.trailers_ref()
                    && let Some(policy) = this.trailer_policy
                    && let Err(error) =
                        policy.check(trailers, this.rejection_observer, this.rejected)
                {
                    return Poll::Ready(Some(Err(error)));
                }
                Poll::Ready(Some(Ok(frame)))
            }
            other => other,
        }
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> http_body::SizeHint {
        self.inner.size_hint()
    }
}

pin_project! {
    pub(crate) struct ResponseLimitBody<B> {
        #[pin]
        inner: B,
        body_limit: Option<u64>,
        header_limit: Option<usize>,
        seen: u64,
        rejection_observer: Option<RejectionObserver>,
        rejected: bool,
    }
}

impl<B> ResponseLimitBody<B> {
    pub(crate) fn new_policy(
        inner: B,
        body_limit: Option<u64>,
        header_limit: Option<usize>,
        rejection_observer: Option<RejectionObserver>,
    ) -> Self {
        Self {
            inner,
            body_limit,
            header_limit,
            seen: 0,
            rejection_observer,
            rejected: false,
        }
    }
}

impl<B> Body for ResponseLimitBody<B>
where
    B: Body<Data = Bytes, Error = ErrorCode>,
{
    type Data = Bytes;
    type Error = ErrorCode;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        let mut this = self.project();
        match this.inner.as_mut().poll_frame(cx) {
            Poll::Ready(Some(Ok(frame))) => {
                if let Some(data) = frame.data_ref()
                    && let Some(limit) = this.body_limit
                {
                    let len = u64::try_from(data.len()).unwrap_or(u64::MAX);
                    if this.seen.saturating_add(len) > *limit {
                        notify_rejection_once(
                            this.rejection_observer,
                            this.rejected,
                            RejectionReason::BodyLimit,
                        );
                        return Poll::Ready(Some(Err(ErrorCode::HttpResponseBodySize(Some(
                            *limit,
                        )))));
                    }
                    *this.seen = this.seen.saturating_add(len);
                }
                if let Some(trailers) = frame.trailers_ref()
                    && let Some(limit) = this.header_limit
                    && header_section_size(trailers) > *limit
                {
                    notify_rejection_once(
                        this.rejection_observer,
                        this.rejected,
                        RejectionReason::HeaderLimit,
                    );
                    return Poll::Ready(Some(Err(ErrorCode::HttpResponseTrailerSectionSize(
                        Some(limit_to_u32(*limit)),
                    ))));
                }
                Poll::Ready(Some(Ok(frame)))
            }
            other => other,
        }
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> http_body::SizeHint {
        self.inner.size_hint()
    }
}

pin_project! {
    pub(crate) struct DeadlineBody<B> {
        #[pin]
        inner: B,
        deadline: Instant,
        rejection_observer: Option<RejectionObserver>,
        rejected: bool,
        #[pin]
        timer: Option<async_io::Timer>,
    }
}

impl<B> DeadlineBody<B> {
    pub(crate) fn new(
        inner: B,
        deadline: Instant,
        rejection_observer: Option<RejectionObserver>,
    ) -> Self {
        Self {
            inner,
            deadline,
            rejection_observer,
            rejected: false,
            timer: None,
        }
    }
}

impl<B> Body for DeadlineBody<B>
where
    B: Body<Data = Bytes, Error = ErrorCode>,
{
    type Data = Bytes;
    type Error = ErrorCode;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        let mut this = self.project();
        if Instant::now() >= *this.deadline {
            notify_rejection_once(
                this.rejection_observer,
                this.rejected,
                RejectionReason::Deadline,
            );
            return Poll::Ready(Some(Err(ErrorCode::HttpResponseTimeout)));
        }

        match this.inner.as_mut().poll_frame(cx) {
            Poll::Ready(result) => Poll::Ready(result),
            Poll::Pending => {
                if this.timer.as_ref().get_ref().is_none() {
                    this.timer.set(Some(async_io::Timer::at(*this.deadline)));
                }
                if let Some(timer) = this.timer.as_mut().as_pin_mut()
                    && let Poll::Ready(_) = timer.poll(cx)
                {
                    this.timer.set(None);
                    notify_rejection_once(
                        this.rejection_observer,
                        this.rejected,
                        RejectionReason::Deadline,
                    );
                    return Poll::Ready(Some(Err(ErrorCode::HttpResponseTimeout)));
                }
                Poll::Pending
            }
        }
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> http_body::SizeHint {
        self.inner.size_hint()
    }
}
