use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use aioduct::TokioClient;
use tokio_util::sync::CancellationToken;
use tracing::{debug, trace, warn};

use super::disk_writer::DiskWriter;
use super::progress::ProgressHandle;
use super::request_config::ExtraRequestConfig;
use super::scheduler::GlobalScheduler;
use super::segment_man::SegmentMan;
use super::speed_monitor::SpeedMonitor;
use super::tui_state::{SharedEventLog, SharedWorkerStates, WorkerStatus, push_event};

const READ_TIMEOUT: Duration = Duration::from_secs(30);
const STALL_CHECK_INTERVAL: Duration = Duration::from_secs(10);
const STALL_THRESHOLD_BYTES_PER_SEC: u64 = 1024;

pub struct WorkerContext {
    pub client: TokioClient,
    pub url: String,
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
                ws.piece_length = assignment.length;
                ws.piece_downloaded.store(0, Ordering::Relaxed);
                ws.status = WorkerStatus::Downloading;
                ws.retries = 0;
                Arc::clone(&ws.piece_downloaded)
            } else {
                Arc::new(std::sync::atomic::AtomicU64::new(0))
            }
        };

        let piece_cancel = if ctx.segment_man.is_endgame() {
            push_event(
                &ctx.events,
                format!("W{worker_id}: endgame piece #{}", assignment.index),
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

            let result = download_piece(
                &ctx.client,
                &ctx.url,
                assignment.offset,
                assignment.length,
                &ctx.extra,
                &piece_cancel,
                &piece_counter,
            )
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
                    push_event(
                        &ctx.events,
                        format!(
                            "W{worker_id}: piece #{} complete ({} bytes)",
                            assignment.index,
                            data.len()
                        ),
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
                    warn!(
                        worker_id,
                        piece = assignment.index,
                        retries,
                        max_retries = ctx.max_retries,
                        error = %e,
                        "piece download failed, retrying"
                    );
                    push_event(
                        &ctx.events,
                        format!(
                            "W{worker_id}: piece #{} retry {}/{} — {}",
                            assignment.index, retries, ctx.max_retries, e
                        ),
                    );

                    {
                        let mut states = ctx.worker_states.lock().unwrap();
                        if let Some(ws) = states.get_mut(worker_id) {
                            ws.status = WorkerStatus::Retrying;
                            ws.retries = retries;
                        }
                    }

                    if retries >= ctx.max_retries {
                        ctx.segment_man.fail_piece(assignment.index);
                        push_event(
                            &ctx.events,
                            format!(
                                "W{worker_id}: piece #{} FAILED after {} attempts",
                                assignment.index, ctx.max_retries
                            ),
                        );
                        update_worker_status(&ctx.worker_states, worker_id, WorkerStatus::Done);
                        return Err(e);
                    }
                    tokio::time::sleep(Duration::from_millis(500 * retries as u64)).await;

                    {
                        let mut states = ctx.worker_states.lock().unwrap();
                        if let Some(ws) = states.get_mut(worker_id) {
                            ws.status = WorkerStatus::Downloading;
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
        ws.current_piece = None;
    }
}

async fn download_piece(
    client: &TokioClient,
    url: &str,
    offset: u64,
    length: u64,
    extra: &ExtraRequestConfig,
    cancel: &CancellationToken,
    progress_counter: &Arc<std::sync::atomic::AtomicU64>,
) -> Result<Vec<u8>, aioduct::Error> {
    let end = offset + length - 1;
    let range = format!("bytes={offset}-{end}");

    let mut req = client.get(url)?;
    if let Ok(v) = range.parse::<http::HeaderValue>() {
        req = req.header(http::header::RANGE, v);
    }
    req = extra.apply_to(req);

    let resp = tokio::select! {
        r = req.send() => r?,
        _ = cancel.cancelled() => {
            return Err(aioduct::Error::Other("cancelled".into()));
        }
    };

    let status = resp.status();
    if !status.is_success() && status.as_u16() != 206 {
        return Err(aioduct::Error::Status(status));
    }

    let mut data = Vec::with_capacity(length as usize);
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
                        progress_counter.store(data.len() as u64, Ordering::Relaxed);
                        if data.len() >= length as usize {
                            data.truncate(length as usize);
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
            _ = cancel.cancelled() => {
                return Err(aioduct::Error::Other("cancelled".into()));
            }
            _ = tokio::time::sleep(READ_TIMEOUT) => {
                return Err(aioduct::Error::Other("read timeout: no data received for 30s".into()));
            }
        }
    }

    Ok(data)
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
                ws.file_name = assignment.url.rsplit('/').next().unwrap_or("").to_string();
                ws.current_piece = Some(assignment.piece.index);
                ws.piece_length = assignment.piece.length;
                ws.piece_downloaded.store(0, Ordering::Relaxed);
                ws.status = WorkerStatus::Downloading;
                ws.retries = 0;
                Arc::clone(&ws.piece_downloaded)
            } else {
                Arc::new(std::sync::atomic::AtomicU64::new(0))
            }
        };

        let mut retries = 0u32;
        loop {
            piece_counter.store(0, Ordering::Relaxed);

            let result = download_piece(
                &ctx.client,
                &assignment.url,
                assignment.piece.offset,
                assignment.piece.length,
                &ctx.extra,
                &ctx.cancel,
                &piece_counter,
            )
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
                    push_event(
                        &ctx.events,
                        format!(
                            "W{worker_id}: file#{} piece #{} complete ({} bytes)",
                            assignment.file_id,
                            assignment.piece.index,
                            data.len()
                        ),
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
                    warn!(
                        worker_id,
                        file_id = assignment.file_id,
                        piece = assignment.piece.index,
                        retries,
                        max_retries = ctx.max_retries,
                        error = %e,
                        "pool piece download failed, retrying"
                    );
                    push_event(
                        &ctx.events,
                        format!(
                            "W{worker_id}: file#{} piece #{} retry {}/{} — {}",
                            assignment.file_id, assignment.piece.index, retries, ctx.max_retries, e
                        ),
                    );

                    {
                        let mut states = ctx.worker_states.lock().unwrap();
                        if let Some(ws) = states.get_mut(worker_id) {
                            ws.status = WorkerStatus::Retrying;
                            ws.retries = retries;
                        }
                    }

                    if retries >= ctx.max_retries {
                        ctx.scheduler
                            .fail_piece(assignment.file_id, assignment.piece.index);
                        push_event(
                            &ctx.events,
                            format!(
                                "W{worker_id}: file#{} piece #{} FAILED after {} attempts",
                                assignment.file_id, assignment.piece.index, ctx.max_retries
                            ),
                        );
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(500 * retries as u64)).await;

                    {
                        let mut states = ctx.worker_states.lock().unwrap();
                        if let Some(ws) = states.get_mut(worker_id) {
                            ws.status = WorkerStatus::Downloading;
                        }
                    }
                }
            }
        }
    }

    update_worker_status(&ctx.worker_states, worker_id, WorkerStatus::Done);
    Ok(())
}
