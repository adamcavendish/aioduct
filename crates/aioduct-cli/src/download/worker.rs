use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use aioduct::TokioClient;
use http::{HeaderValue, StatusCode};
use tokio_util::sync::CancellationToken;
use tracing::{debug, trace, warn};

use super::disk_writer::DiskWriter;
use super::progress::ProgressHandle;
use super::request_config::ExtraRequestConfig;
use super::scheduler::GlobalScheduler;
use super::segment_man::SegmentMan;
use super::speed_monitor::SpeedMonitor;
use super::tui_state::{
    DownloadEvent, EventCategory, EventSeverity, SharedEventLog, SharedWorkerStates, WorkerStatus,
    push_typed_event,
};

const READ_TIMEOUT: Duration = Duration::from_secs(30);
const STALL_CHECK_INTERVAL: Duration = Duration::from_secs(10);
const STALL_THRESHOLD_BYTES_PER_SEC: u64 = 1024;

pub struct WorkerContext {
    pub client: TokioClient,
    pub url: String,
    pub expected_total_size: u64,
    pub if_range: Option<HeaderValue>,
    pub extra: Arc<ExtraRequestConfig>,
    pub disk_writer: Arc<DiskWriter>,
    pub segment_man: Arc<SegmentMan>,
    pub speed: Arc<std::sync::Mutex<SpeedMonitor>>,
    pub worker_states: SharedWorkerStates,
    pub events: SharedEventLog,
    pub cancel: CancellationToken,
    pub max_retries: u32,
}

impl WorkerContext {
    pub async fn run(
        &self,
        worker_id: usize,
        progress: &ProgressHandle,
    ) -> Result<(), aioduct::Error> {
        worker_loop(self, worker_id, progress).await
    }
}

async fn worker_loop(
    ctx: &WorkerContext,
    worker_id: usize,
    progress: &ProgressHandle,
) -> Result<(), aioduct::Error> {
    loop {
        if ctx.cancel.is_cancelled() {
            break;
        }

        let assignment = match ctx.segment_man.next_piece(worker_id) {
            Some(a) => a,
            None => break,
        };

        trace!(
            worker_id,
            piece = assignment.index,
            offset = assignment.offset,
            length = assignment.length,
            "assigned piece"
        );

        // Update worker state: assigned
        let piece_counter = {
            let mut states = ctx.worker_states.lock().unwrap();
            if let Some(ws) = states.get_mut(worker_id) {
                ws.current_piece = Some(assignment.index);
                ws.assignment_started_at = Some(Instant::now());
                ws.status_changed_at = Instant::now();
                ws.piece_length = assignment.length;
                ws.piece_downloaded.store(0, Ordering::Relaxed);
                ws.status = WorkerStatus::Downloading;
                ws.retries = 0;
                ws.last_error = None;
                Arc::clone(&ws.piece_downloaded)
            } else {
                Arc::new(std::sync::atomic::AtomicU64::new(0))
            }
        };

        let piece_cancel = if ctx.segment_man.is_endgame() {
            push_typed_event(
                &ctx.events,
                DownloadEvent::new(
                    EventSeverity::Info,
                    EventCategory::Assignment,
                    format!("endgame piece #{}", assignment.index),
                )
                .worker(worker_id)
                .piece(assignment.index),
            );
            ctx.segment_man.register_endgame_worker(assignment.index)
        } else {
            ctx.cancel.child_token()
        };

        let mut retries = 0;
        loop {
            if ctx
                .segment_man
                .snapshot_storage(|s| s.is_complete(assignment.index))
            {
                break;
            }

            piece_counter.store(0, Ordering::Relaxed);

            let result = download_piece(PieceDownloadRequest {
                client: &ctx.client,
                url: &ctx.url,
                offset: assignment.offset,
                length: assignment.length,
                expected_total_size: ctx.expected_total_size,
                if_range: ctx.if_range.as_ref(),
                extra: &ctx.extra,
                cancel: &piece_cancel,
                progress_counter: &piece_counter,
            })
            .await;

            match result {
                Ok(data) => {
                    if ctx
                        .segment_man
                        .snapshot_storage(|s| s.is_complete(assignment.index))
                    {
                        break;
                    }
                    ctx.disk_writer
                        .write_at(assignment.offset, &data)
                        .map_err(aioduct::Error::Io)?;
                    ctx.segment_man.complete_piece(assignment.index);
                    progress.add_downloaded(data.len() as u64);
                    ctx.speed.lock().unwrap().record(data.len() as u64);

                    debug!(
                        worker_id,
                        piece = assignment.index,
                        bytes = data.len(),
                        "piece complete"
                    );
                    push_typed_event(
                        &ctx.events,
                        DownloadEvent::new(
                            EventSeverity::Info,
                            EventCategory::Piece,
                            format!("complete ({} bytes)", data.len()),
                        )
                        .worker(worker_id)
                        .piece(assignment.index),
                    );
                    break;
                }
                Err(_) if piece_cancel.is_cancelled() => {
                    trace!(
                        worker_id,
                        piece = assignment.index,
                        "piece cancelled (endgame)"
                    );
                    break;
                }
                Err(_) if ctx.cancel.is_cancelled() => {
                    ctx.segment_man.fail_piece(assignment.index);
                    update_worker_status(&ctx.worker_states, worker_id, WorkerStatus::Done);
                    return Ok(());
                }
                Err(e) => {
                    retries += 1;
                    let error_msg = super::tui_state::sanitize_for_display(&e.to_string());
                    ctx.segment_man
                        .record_piece_retry(assignment.index, error_msg.clone());
                    warn!(
                        worker_id,
                        piece = assignment.index,
                        retries,
                        max_retries = ctx.max_retries,
                        error = %e,
                        "piece download failed, retrying"
                    );
                    push_typed_event(
                        &ctx.events,
                        DownloadEvent::new(
                            EventSeverity::Retry,
                            EventCategory::Retry,
                            format!("retry {retries}/{} - {error_msg}", ctx.max_retries),
                        )
                        .worker(worker_id)
                        .piece(assignment.index),
                    );

                    {
                        let mut states = ctx.worker_states.lock().unwrap();
                        if let Some(ws) = states.get_mut(worker_id) {
                            ws.status = WorkerStatus::Retrying;
                            ws.status_changed_at = Instant::now();
                            ws.retries = retries;
                            ws.last_error = Some(error_msg.clone());
                        }
                    }

                    if retries >= ctx.max_retries {
                        ctx.segment_man
                            .mark_piece_failed(assignment.index, error_msg.clone());
                        push_typed_event(
                            &ctx.events,
                            DownloadEvent::new(
                                EventSeverity::Error,
                                EventCategory::Failure,
                                format!("failed after {} attempts - {error_msg}", ctx.max_retries),
                            )
                            .worker(worker_id)
                            .piece(assignment.index),
                        );
                        update_worker_status(&ctx.worker_states, worker_id, WorkerStatus::Done);
                        return Err(e);
                    }
                    tokio::time::sleep(Duration::from_millis(500 * retries as u64)).await;

                    {
                        let mut states = ctx.worker_states.lock().unwrap();
                        if let Some(ws) = states.get_mut(worker_id) {
                            ws.status = WorkerStatus::Downloading;
                            ws.status_changed_at = Instant::now();
                        }
                    }
                }
            }
        }

        // Update per-worker speed estimate
        {
            let mut states = ctx.worker_states.lock().unwrap();
            if let Some(ws) = states.get_mut(worker_id) {
                ws.speed_bps = ctx.speed.lock().unwrap().speed_bytes_per_sec();
            }
        }
    }

    update_worker_status(&ctx.worker_states, worker_id, WorkerStatus::Done);
    Ok(())
}

fn update_worker_status(
    worker_states: &SharedWorkerStates,
    worker_id: usize,
    status: WorkerStatus,
) {
    let mut states = worker_states.lock().unwrap();
    if let Some(ws) = states.get_mut(worker_id) {
        ws.status = status;
        ws.status_changed_at = Instant::now();
        ws.current_piece = None;
        ws.assignment_started_at = None;
    }
}

struct PieceDownloadRequest<'a> {
    client: &'a TokioClient,
    url: &'a str,
    offset: u64,
    length: u64,
    expected_total_size: u64,
    if_range: Option<&'a HeaderValue>,
    extra: &'a ExtraRequestConfig,
    cancel: &'a CancellationToken,
    progress_counter: &'a Arc<std::sync::atomic::AtomicU64>,
}

async fn download_piece(req: PieceDownloadRequest<'_>) -> Result<Vec<u8>, aioduct::Error> {
    let end = checked_range_end(req.offset, req.length)?;
    let range = format!("bytes={}-{}", req.offset, end);

    let mut http_req = req.client.get(req.url)?;
    http_req = req.extra.apply_to(http_req);
    let range = range
        .parse::<HeaderValue>()
        .map_err(|e| aioduct::Error::Other(Box::new(e)))?;
    http_req = http_req.header(http::header::RANGE, range);
    if let Some(if_range) = req.if_range {
        http_req = http_req.header(http::header::IF_RANGE, if_range.clone());
    }

    let resp = tokio::select! {
        r = http_req.send() => r?,
        _ = req.cancel.cancelled() => {
            return Err(aioduct::Error::Other("cancelled".into()));
        }
    };

    let status = resp.status();
    if status != StatusCode::PARTIAL_CONTENT {
        return Err(aioduct::Error::Other(
            format!("range request returned {status}; expected 206 Partial Content").into(),
        ));
    }
    validate_content_range(
        resp.headers(),
        req.offset,
        req.length,
        req.expected_total_size,
    )?;

    let mut data = Vec::with_capacity(req.length as usize);
    let mut stream = resp.into_bytes_stream();
    let mut stall_check_start = Instant::now();
    let mut stall_check_bytes = 0u64;

    loop {
        tokio::select! {
            chunk = stream.next() => {
                match chunk {
                    Some(Ok(bytes)) => {
                        stall_check_bytes += bytes.len() as u64;
                        data.extend_from_slice(&bytes);
                        req.progress_counter.store(data.len() as u64, Ordering::Relaxed);
                        if data.len() >= req.length as usize {
                            data.truncate(req.length as usize);
                            break;
                        }
                        let elapsed = stall_check_start.elapsed();
                        if elapsed >= STALL_CHECK_INTERVAL {
                            let bps = stall_check_bytes as f64 / elapsed.as_secs_f64();
                            if (bps as u64) < STALL_THRESHOLD_BYTES_PER_SEC {
                                return Err(aioduct::Error::Other(
                                    format!("stall detected: {:.0} B/s over {:.0}s", bps, elapsed.as_secs_f64()).into()
                                ));
                            }
                            stall_check_start = Instant::now();
                            stall_check_bytes = 0;
                        }
                    }
                    Some(Err(e)) => return Err(e),
                    None => break,
                }
            }
            _ = req.cancel.cancelled() => {
                return Err(aioduct::Error::Other("cancelled".into()));
            }
            _ = tokio::time::sleep(READ_TIMEOUT) => {
                return Err(aioduct::Error::Other("read timeout: no data received for 30s".into()));
            }
        }
    }

    if data.len() != req.length as usize {
        return Err(aioduct::Error::Other(
            format!(
                "range response ended early: expected {} bytes, got {}",
                req.length,
                data.len()
            )
            .into(),
        ));
    }

    Ok(data)
}

fn validate_content_range(
    headers: &http::HeaderMap,
    offset: u64,
    length: u64,
    expected_total_size: u64,
) -> Result<(), aioduct::Error> {
    let expected_end = checked_range_end(offset, length)?;
    let value = headers
        .get(http::header::CONTENT_RANGE)
        .ok_or_else(|| aioduct::Error::Other("missing Content-Range header".into()))?
        .to_str()
        .map_err(|e| aioduct::Error::Other(Box::new(e)))?;
    let Some((start, end, total)) = parse_content_range(value) else {
        return Err(aioduct::Error::Other(
            format!("invalid Content-Range header: {value}").into(),
        ));
    };

    if start != offset || end != expected_end {
        return Err(aioduct::Error::Other(
            format!(
                "Content-Range mismatch: expected bytes {offset}-{expected_end}, got {start}-{end}"
            )
            .into(),
        ));
    }
    let total =
        total.ok_or_else(|| aioduct::Error::Other("Content-Range missing total length".into()))?;
    if total != expected_total_size {
        return Err(aioduct::Error::Other(
            format!("Content-Range total mismatch: expected {expected_total_size}, got {total}")
                .into(),
        ));
    }
    if end >= total {
        return Err(aioduct::Error::Other(
            format!("Content-Range end {end} exceeds total length {total}").into(),
        ));
    }

    Ok(())
}

fn checked_range_end(offset: u64, length: u64) -> Result<u64, aioduct::Error> {
    if length == 0 {
        return Err(aioduct::Error::Other("invalid empty range request".into()));
    }
    offset
        .checked_add(length)
        .and_then(|n| n.checked_sub(1))
        .ok_or_else(|| aioduct::Error::Other("invalid range request".into()))
}

fn parse_content_range(value: &str) -> Option<(u64, u64, Option<u64>)> {
    let rest = value.trim().strip_prefix("bytes ")?;
    let (range, total) = rest.split_once('/')?;
    let (start, end) = range.split_once('-')?;
    let start = start.trim().parse::<u64>().ok()?;
    let end = end.trim().parse::<u64>().ok()?;
    if end < start {
        return None;
    }
    let total = match total.trim() {
        "*" => None,
        value => Some(value.parse::<u64>().ok()?),
    };
    Some((start, end, total))
}

pub(crate) fn if_range_header_value(
    etag: Option<&str>,
    last_modified: Option<&str>,
) -> Option<HeaderValue> {
    etag.and_then(|value| {
        let value = value.trim();
        is_strong_etag(value)
            .then(|| HeaderValue::from_str(value).ok())
            .flatten()
    })
    .or_else(|| {
        last_modified.and_then(|value| {
            let value = value.trim();
            (!value.is_empty())
                .then(|| HeaderValue::from_str(value).ok())
                .flatten()
        })
    })
}

pub(crate) fn is_strong_etag(etag: &str) -> bool {
    let etag = etag.trim();
    etag.starts_with('"')
        && !etag
            .get(..2)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("W/"))
}

// ─── Pool Worker (multi-file mode) ─────────────────────────────────────────

pub struct PoolWorkerContext {
    pub client: TokioClient,
    pub extra: Arc<ExtraRequestConfig>,
    pub scheduler: Arc<GlobalScheduler>,
    pub worker_states: SharedWorkerStates,
    pub events: SharedEventLog,
    pub cancel: CancellationToken,
    pub max_retries: u32,
}

impl PoolWorkerContext {
    pub async fn run(&self, worker_id: usize) -> Result<(), aioduct::Error> {
        pool_worker_loop(self, worker_id).await
    }
}

async fn pool_worker_loop(ctx: &PoolWorkerContext, worker_id: usize) -> Result<(), aioduct::Error> {
    loop {
        if ctx.cancel.is_cancelled() {
            break;
        }

        let assignment = match ctx.scheduler.next_work(worker_id) {
            Some(a) => a,
            None => {
                if ctx.scheduler.all_complete() {
                    break;
                }
                tokio::select! {
                    _ = ctx.scheduler.work_available().notified() => continue,
                    _ = ctx.cancel.cancelled() => break,
                }
            }
        };

        trace!(
            worker_id,
            file_id = assignment.file_id,
            piece = assignment.piece.index,
            offset = assignment.piece.offset,
            length = assignment.piece.length,
            "pool worker assigned piece"
        );

        let piece_counter = {
            let mut states = ctx.worker_states.lock().unwrap();
            if let Some(ws) = states.get_mut(worker_id) {
                ws.file_id = Some(assignment.file_id);
                ws.file_name = assignment.file_name.clone();
                ws.current_piece = Some(assignment.piece.index);
                ws.assignment_started_at = Some(Instant::now());
                ws.status_changed_at = Instant::now();
                ws.piece_length = assignment.piece.length;
                ws.piece_downloaded.store(0, Ordering::Relaxed);
                ws.status = WorkerStatus::Downloading;
                ws.retries = 0;
                ws.last_error = None;
                Arc::clone(&ws.piece_downloaded)
            } else {
                Arc::new(std::sync::atomic::AtomicU64::new(0))
            }
        };

        let mut retries = 0u32;
        loop {
            piece_counter.store(0, Ordering::Relaxed);

            let result = download_piece(PieceDownloadRequest {
                client: &ctx.client,
                url: &assignment.url,
                offset: assignment.piece.offset,
                length: assignment.piece.length,
                expected_total_size: assignment.total_size,
                if_range: assignment.if_range.as_ref(),
                extra: &ctx.extra,
                cancel: &ctx.cancel,
                progress_counter: &piece_counter,
            })
            .await;

            match result {
                Ok(data) => {
                    assignment
                        .disk_writer
                        .write_at(assignment.piece.offset, &data)
                        .map_err(aioduct::Error::Io)?;
                    ctx.scheduler
                        .complete_piece(assignment.file_id, assignment.piece.index);

                    debug!(
                        worker_id,
                        file_id = assignment.file_id,
                        piece = assignment.piece.index,
                        bytes = data.len(),
                        "pool piece complete"
                    );
                    push_typed_event(
                        &ctx.events,
                        DownloadEvent::new(
                            EventSeverity::Info,
                            EventCategory::Piece,
                            format!("complete ({} bytes)", data.len()),
                        )
                        .file(assignment.file_id, assignment.file_name.clone())
                        .worker(worker_id)
                        .piece(assignment.piece.index),
                    );
                    break;
                }
                Err(_) if ctx.cancel.is_cancelled() => {
                    ctx.scheduler
                        .fail_piece(assignment.file_id, assignment.piece.index);
                    update_worker_status(&ctx.worker_states, worker_id, WorkerStatus::Done);
                    return Ok(());
                }
                Err(e) => {
                    retries += 1;
                    let error_msg = super::tui_state::sanitize_for_display(&e.to_string());
                    ctx.scheduler.record_piece_retry(
                        assignment.file_id,
                        assignment.piece.index,
                        error_msg.clone(),
                    );
                    warn!(
                        worker_id,
                        file_id = assignment.file_id,
                        piece = assignment.piece.index,
                        retries,
                        max_retries = ctx.max_retries,
                        error = %e,
                        "pool piece download failed, retrying"
                    );
                    push_typed_event(
                        &ctx.events,
                        DownloadEvent::new(
                            EventSeverity::Retry,
                            EventCategory::Retry,
                            format!("retry {retries}/{} - {error_msg}", ctx.max_retries),
                        )
                        .file(assignment.file_id, assignment.file_name.clone())
                        .worker(worker_id)
                        .piece(assignment.piece.index),
                    );

                    {
                        let mut states = ctx.worker_states.lock().unwrap();
                        if let Some(ws) = states.get_mut(worker_id) {
                            ws.status = WorkerStatus::Retrying;
                            ws.status_changed_at = Instant::now();
                            ws.retries = retries;
                            ws.last_error = Some(error_msg.clone());
                        }
                    }

                    if retries >= ctx.max_retries {
                        ctx.scheduler.mark_piece_failed(
                            assignment.file_id,
                            assignment.piece.index,
                            error_msg.clone(),
                        );
                        push_typed_event(
                            &ctx.events,
                            DownloadEvent::new(
                                EventSeverity::Error,
                                EventCategory::Failure,
                                format!("failed after {} attempts - {error_msg}", ctx.max_retries),
                            )
                            .file(assignment.file_id, assignment.file_name.clone())
                            .worker(worker_id)
                            .piece(assignment.piece.index),
                        );
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(500 * retries as u64)).await;

                    {
                        let mut states = ctx.worker_states.lock().unwrap();
                        if let Some(ws) = states.get_mut(worker_id) {
                            ws.status = WorkerStatus::Downloading;
                            ws.status_changed_at = Instant::now();
                        }
                    }
                }
            }
        }
    }

    update_worker_status(&ctx.worker_states, worker_id, WorkerStatus::Done);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_content_range() {
        assert_eq!(
            parse_content_range("bytes 10-19/100"),
            Some((10, 19, Some(100)))
        );
        assert_eq!(parse_content_range("bytes 10-19/*"), Some((10, 19, None)));
        assert_eq!(parse_content_range("items 10-19/100"), None);
        assert_eq!(parse_content_range("bytes 19-10/100"), None);
    }

    #[test]
    fn validates_expected_content_range() {
        let mut headers = http::HeaderMap::new();
        headers.insert(
            http::header::CONTENT_RANGE,
            http::HeaderValue::from_static("bytes 20-29/100"),
        );

        assert!(validate_content_range(&headers, 20, 10, 100).is_ok());
        assert!(validate_content_range(&headers, 30, 10, 100).is_err());
        assert!(validate_content_range(&headers, 20, 10, 101).is_err());
        assert!(validate_content_range(&headers, 20, 0, 100).is_err());
    }

    #[test]
    fn rejects_unknown_content_range_total() {
        let mut headers = http::HeaderMap::new();
        headers.insert(
            http::header::CONTENT_RANGE,
            http::HeaderValue::from_static("bytes 20-29/*"),
        );

        assert!(validate_content_range(&headers, 20, 10, 100).is_err());
    }

    #[test]
    fn selects_if_range_validator() {
        assert_eq!(
            if_range_header_value(Some("\"strong\""), Some("Mon, 01 Jan 2024 00:00:00 GMT")),
            Some(HeaderValue::from_static("\"strong\""))
        );
        assert_eq!(
            if_range_header_value(Some("W/\"weak\""), Some("Mon, 01 Jan 2024 00:00:00 GMT")),
            Some(HeaderValue::from_static("Mon, 01 Jan 2024 00:00:00 GMT"))
        );
        assert_eq!(
            if_range_header_value(Some("unquoted"), Some("Mon, 01 Jan 2024 00:00:00 GMT")),
            Some(HeaderValue::from_static("Mon, 01 Jan 2024 00:00:00 GMT"))
        );
        assert_eq!(if_range_header_value(Some("W/\"weak\""), None), None);
    }
}
