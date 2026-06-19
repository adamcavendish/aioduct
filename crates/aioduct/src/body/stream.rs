//! Async byte-stream iterators over response bodies.

use bytes::Bytes;
use http_body_util::BodyExt;

#[cfg(not(target_arch = "wasm32"))]
use crate::clock::Instant;
use crate::error::Error;
#[cfg(not(target_arch = "wasm32"))]
use crate::observer::{self, RequestEvent, RequestPhase, TransferDirection};
#[cfg(not(target_arch = "wasm32"))]
use crate::response::BodyObserverCtx;

use super::RequestBodySend;
#[cfg(not(target_arch = "wasm32"))]
use super::ResponseBodyLocal;

// ── BodyStreamSend ───────────────────────────────────────────────────────────

/// Async iterator over response body data frames.
pub struct BodyStreamSend {
    body: RequestBodySend,
    done: bool,
    /// Trailer headers captured from the body's trailer frame, if any.
    trailers: Option<http::HeaderMap>,
    #[cfg(not(target_arch = "wasm32"))]
    observer_ctx: Option<BodyObserverCtx>,
    #[cfg(not(target_arch = "wasm32"))]
    cumulative_bytes: u64,
    #[cfg(not(target_arch = "wasm32"))]
    transfer_start: Instant,
}

impl std::fmt::Debug for BodyStreamSend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BodyStreamSend").finish()
    }
}

impl BodyStreamSend {
    #[cfg(test)]
    pub(crate) fn new(body: RequestBodySend) -> Self {
        Self {
            body,
            done: false,
            trailers: None,
            #[cfg(not(target_arch = "wasm32"))]
            observer_ctx: None,
            #[cfg(not(target_arch = "wasm32"))]
            cumulative_bytes: 0,
            #[cfg(not(target_arch = "wasm32"))]
            transfer_start: Instant::now(),
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn with_observer(body: RequestBodySend, ctx: Option<BodyObserverCtx>) -> Self {
        let transfer_start = ctx
            .as_ref()
            .map(|c| c.response_started)
            .unwrap_or_else(Instant::now);
        Self {
            body,
            done: false,
            trailers: None,
            observer_ctx: ctx,
            cumulative_bytes: 0,
            transfer_start,
        }
    }

    /// Returns the trailer headers received after the body, if any.
    ///
    /// Only populated once the stream has been fully consumed (i.e. `next()`
    /// returned `None`); trailers arrive after the final data frame.
    pub fn trailers(&self) -> Option<&http::HeaderMap> {
        self.trailers.as_ref()
    }

    /// Returns the next chunk of body data, or `None` when complete.
    pub async fn next(&mut self) -> Option<Result<Bytes, Error>> {
        if self.done {
            return None;
        }

        loop {
            match self.body.frame().await {
                Some(Ok(frame)) => match frame.into_data() {
                    Ok(data) => {
                        // Skip empty data frames: a consumer expects `Some` to
                        // carry non-empty data and `None` to mean end of stream.
                        if data.is_empty() {
                            continue;
                        }
                        #[cfg(not(target_arch = "wasm32"))]
                        {
                            let chunk_bytes = data.len() as u64;
                            self.cumulative_bytes += chunk_bytes;
                            if let Some(ctx) = &self.observer_ctx {
                                ctx.observer.on_event(&RequestEvent {
                                    method: ctx.method.clone(),
                                    uri: ctx.uri.clone(),
                                    phase: RequestPhase::BytesTransferred {
                                        direction: TransferDirection::Download,
                                        chunk_bytes,
                                        cumulative_bytes: self.cumulative_bytes,
                                        elapsed: self.transfer_start.elapsed(),
                                    },
                                    at: observer::Instant::now(),
                                });
                            }
                        }
                        return Some(Ok(data));
                    }
                    Err(frame) => {
                        // Capture trailers so callers can read them via
                        // `trailers()` after the stream completes.
                        match frame.into_trailers() {
                            Ok(trailers) => {
                                #[cfg(not(target_arch = "wasm32"))]
                                if let Some(ctx) = &self.observer_ctx {
                                    let headers: Vec<(String, String)> = trailers
                                        .iter()
                                        .map(|(k, v)| {
                                            (
                                                k.as_str().to_owned(),
                                                v.to_str().unwrap_or("<binary>").to_owned(),
                                            )
                                        })
                                        .collect();
                                    ctx.observer.on_event(&RequestEvent {
                                        method: ctx.method.clone(),
                                        uri: ctx.uri.clone(),
                                        phase: RequestPhase::TrailersReceived { headers },
                                        at: observer::Instant::now(),
                                    });
                                }
                                match &mut self.trailers {
                                    Some(existing) => existing.extend(trailers),
                                    None => self.trailers = Some(trailers),
                                }
                            }
                            Err(_non_trailer) => {}
                        }
                    }
                },
                Some(Err(e)) => {
                    self.done = true;
                    #[cfg(not(target_arch = "wasm32"))]
                    self.fire_transfer_aborted(&e);
                    return Some(Err(e));
                }
                None => {
                    self.done = true;
                    #[cfg(not(target_arch = "wasm32"))]
                    self.fire_transfer_complete();
                    return None;
                }
            }
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn fire_transfer_complete(&self) {
        if let Some(ctx) = &self.observer_ctx {
            let transfer_duration = self.transfer_start.elapsed();
            let throughput = if transfer_duration.as_secs_f64() > 0.0 {
                (self.cumulative_bytes as f64 / transfer_duration.as_secs_f64()) as f32
            } else {
                0.0
            };
            ctx.observer.on_event(&RequestEvent {
                method: ctx.method.clone(),
                uri: ctx.uri.clone(),
                phase: RequestPhase::TransferComplete {
                    direction: TransferDirection::Download,
                    total_bytes: self.cumulative_bytes,
                    transfer_duration,
                    throughput_bytes_per_sec: throughput,
                },
                at: observer::Instant::now(),
            });
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn fire_transfer_aborted(&self, error: &crate::error::Error) {
        if let Some(ctx) = &self.observer_ctx {
            ctx.observer.on_event(&RequestEvent {
                method: ctx.method.clone(),
                uri: ctx.uri.clone(),
                phase: RequestPhase::TransferAborted {
                    direction: TransferDirection::Download,
                    bytes_transferred: self.cumulative_bytes,
                    elapsed: self.transfer_start.elapsed(),
                    error: error.to_string(),
                },
                at: observer::Instant::now(),
            });
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl Drop for BodyStreamSend {
    fn drop(&mut self) {
        if !self.done {
            self.done = true;
            self.fire_transfer_aborted(&crate::error::Error::Other("body stream dropped".into()));
        }
    }
}

// ── BodyStreamLocal ──────────────────────────────────────────────────────────

/// Async iterator over response body data frames for `!Send` bodies.
///
/// Used by completion-based runtimes (compio) where the response body is not `Send`.
#[cfg(not(target_arch = "wasm32"))]
pub struct BodyStreamLocal {
    body: ResponseBodyLocal,
    done: bool,
    trailers: Option<http::HeaderMap>,
    observer_ctx: Option<BodyObserverCtx>,
    cumulative_bytes: u64,
    transfer_start: Instant,
}

#[cfg(not(target_arch = "wasm32"))]
impl std::fmt::Debug for BodyStreamLocal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BodyStreamLocal").finish()
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl BodyStreamLocal {
    pub(crate) fn with_observer(body: ResponseBodyLocal, ctx: Option<BodyObserverCtx>) -> Self {
        let transfer_start = ctx
            .as_ref()
            .map(|c| c.response_started)
            .unwrap_or_else(Instant::now);
        Self {
            body,
            done: false,
            trailers: None,
            observer_ctx: ctx,
            cumulative_bytes: 0,
            transfer_start,
        }
    }

    /// Returns the trailer headers received after the body, if any.
    ///
    /// Only populated once the stream has been fully consumed.
    pub fn trailers(&self) -> Option<&http::HeaderMap> {
        self.trailers.as_ref()
    }

    /// Returns the next chunk of body data, or `None` when complete.
    pub async fn next(&mut self) -> Option<Result<Bytes, Error>> {
        use std::pin::Pin;

        if self.done {
            return None;
        }

        loop {
            match Pin::new(&mut self.body).frame().await {
                Some(Ok(frame)) => match frame.into_data() {
                    Ok(data) => {
                        // Skip empty data frames: a consumer expects `Some` to
                        // carry non-empty data and `None` to mean end of stream.
                        if data.is_empty() {
                            continue;
                        }
                        let chunk_bytes = data.len() as u64;
                        self.cumulative_bytes += chunk_bytes;
                        if let Some(ctx) = &self.observer_ctx {
                            ctx.observer.on_event(&RequestEvent {
                                method: ctx.method.clone(),
                                uri: ctx.uri.clone(),
                                phase: RequestPhase::BytesTransferred {
                                    direction: TransferDirection::Download,
                                    chunk_bytes,
                                    cumulative_bytes: self.cumulative_bytes,
                                    elapsed: self.transfer_start.elapsed(),
                                },
                                at: observer::Instant::now(),
                            });
                        }
                        return Some(Ok(data));
                    }
                    Err(frame) => match frame.into_trailers() {
                        Ok(trailers) => {
                            if let Some(ctx) = &self.observer_ctx {
                                let headers: Vec<(String, String)> = trailers
                                    .iter()
                                    .map(|(k, v)| {
                                        (
                                            k.as_str().to_owned(),
                                            v.to_str().unwrap_or("<binary>").to_owned(),
                                        )
                                    })
                                    .collect();
                                ctx.observer.on_event(&RequestEvent {
                                    method: ctx.method.clone(),
                                    uri: ctx.uri.clone(),
                                    phase: RequestPhase::TrailersReceived { headers },
                                    at: observer::Instant::now(),
                                });
                            }
                            match &mut self.trailers {
                                Some(existing) => existing.extend(trailers),
                                None => self.trailers = Some(trailers),
                            }
                        }
                        Err(_non_trailer) => {}
                    },
                },
                Some(Err(e)) => {
                    self.done = true;
                    self.fire_transfer_aborted(&e);
                    return Some(Err(e));
                }
                None => {
                    self.done = true;
                    self.fire_transfer_complete();
                    return None;
                }
            }
        }
    }

    fn fire_transfer_complete(&self) {
        if let Some(ctx) = &self.observer_ctx {
            let transfer_duration = self.transfer_start.elapsed();
            let throughput = if transfer_duration.as_secs_f64() > 0.0 {
                (self.cumulative_bytes as f64 / transfer_duration.as_secs_f64()) as f32
            } else {
                0.0
            };
            ctx.observer.on_event(&RequestEvent {
                method: ctx.method.clone(),
                uri: ctx.uri.clone(),
                phase: RequestPhase::TransferComplete {
                    direction: TransferDirection::Download,
                    total_bytes: self.cumulative_bytes,
                    transfer_duration,
                    throughput_bytes_per_sec: throughput,
                },
                at: observer::Instant::now(),
            });
        }
    }

    fn fire_transfer_aborted(&self, error: &crate::error::Error) {
        if let Some(ctx) = &self.observer_ctx {
            ctx.observer.on_event(&RequestEvent {
                method: ctx.method.clone(),
                uri: ctx.uri.clone(),
                phase: RequestPhase::TransferAborted {
                    direction: TransferDirection::Download,
                    bytes_transferred: self.cumulative_bytes,
                    elapsed: self.transfer_start.elapsed(),
                    error: error.to_string(),
                },
                at: observer::Instant::now(),
            });
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl Drop for BodyStreamLocal {
    fn drop(&mut self) {
        if !self.done {
            self.done = true;
            self.fire_transfer_aborted(&crate::error::Error::Other("body stream dropped".into()));
        }
    }
}
