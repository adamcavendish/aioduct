use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use aioduct::{RetryConfig, TokioClient};
use tokio::fs::File;
use tokio::io::AsyncWriteExt;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::cli::Cli;
use crate::control_file::ControlFile;
use crate::disk_writer::DiskWriter;
use crate::filename;
use crate::piece::storage::PieceStorage;
use crate::piece_grid::PieceGrid;
use crate::progress::DownloadResult;
use crate::progress::ProgressHandle;
use crate::request_config::ExtraRequestConfig;
use crate::segment_man::SegmentMan;
use crate::speed_monitor::SpeedMonitor;
use crate::tui_state;
use crate::worker;

#[derive(Clone)]
pub struct DownloadEngine {
    client: TokioClient,
    cli: Arc<Cli>,
    extra: Arc<ExtraRequestConfig>,
}

pub struct DownloadTask {
    pub url: String,
    pub output: PathBuf,
    pub total_size: Option<u64>,
    pub supports_range: bool,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
}

impl DownloadEngine {
    pub fn new(cli: Arc<Cli>) -> Self {
        let mut builder = TokioClient::builder()
            .timeout(cli.timeout_duration())
            .connect_timeout(cli.connect_timeout_duration());

        if let Some(ref ua) = cli.user_agent {
            builder = builder.user_agent(ua);
        }

        if cli.max_tries > 0 {
            builder = builder.retry(
                RetryConfig::default()
                    .max_retries(cli.max_tries.saturating_sub(1))
                    .initial_backoff(cli.retry_wait_duration())
                    .max_backoff(Duration::from_secs(60)),
            );
        }

        if cli.check_certificate_false {
            builder = builder.danger_accept_invalid_certs();
        }

        if let Some(limit) = cli.max_overall_download_limit {
            builder = builder.max_download_speed(limit);
        }

        if let Some(ref proxy_uri) = cli.all_proxy
            && let Ok(proxy) = aioduct::ProxyConfig::http(proxy_uri)
                .or_else(|_| aioduct::ProxyConfig::socks5(proxy_uri))
        {
            builder = builder.proxy(proxy);
        }

        let extra = Arc::new(ExtraRequestConfig::from_cli(&cli));
        let client = builder.build().unwrap();
        Self { client, cli, extra }
    }

    pub async fn probe(&self, url: &str) -> Result<DownloadTask, aioduct::Error> {
        debug!(url, "probing URL");
        let req = self.client.head(url)?;
        let req = self.extra.apply_to(req);

        let resp = req.send().await?;
        let headers = resp.headers();

        let total_size = resp.content_length();
        let supports_range = headers
            .get("accept-ranges")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.contains("bytes"));

        let etag = headers
            .get(http::header::ETAG)
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());
        let last_modified = headers
            .get(http::header::LAST_MODIFIED)
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());

        let name = filename::from_url_and_headers(url, headers);
        let output = self.resolve_output_path(&name);

        info!(
            url,
            ?total_size,
            supports_range,
            ?etag,
            output = %output.display(),
            "probe complete"
        );

        Ok(DownloadTask {
            url: url.to_string(),
            output,
            total_size,
            supports_range,
            etag,
            last_modified,
        })
    }

    pub async fn download(&self, task: &DownloadTask, progress: &ProgressHandle) -> DownloadResult {
        let result = if task.supports_range && task.total_size.is_some_and(|s| s > 0) {
            self.download_segmented(task, progress).await
        } else {
            self.download_single(task, progress).await
        };

        match result {
            Ok(size) => DownloadResult {
                output: task.output.clone(),
                total_size: size,
                error: None,
            },
            Err(e) => DownloadResult {
                output: task.output.clone(),
                total_size: 0,
                error: Some(e.to_string()),
            },
        }
    }

    async fn download_single(
        &self,
        task: &DownloadTask,
        progress: &ProgressHandle,
    ) -> Result<u64, aioduct::Error> {
        let req = self.client.get(&task.url)?;
        let req = self.extra.apply_to(req);

        let resp = req.send().await?;
        let resp = resp.error_for_status()?;

        let total = resp.content_length().unwrap_or(0);
        progress.set_total(total);

        let mut file = File::create(&task.output)
            .await
            .map_err(aioduct::Error::Io)?;

        let mut stream = resp.into_bytes_stream();
        let mut downloaded: u64 = 0;

        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            file.write_all(&chunk).await.map_err(aioduct::Error::Io)?;
            downloaded += chunk.len() as u64;
            progress.set_downloaded(downloaded);
        }

        file.flush().await.map_err(aioduct::Error::Io)?;
        Ok(downloaded)
    }

    async fn download_segmented(
        &self,
        task: &DownloadTask,
        progress: &ProgressHandle,
    ) -> Result<u64, aioduct::Error> {
        let total_size = task
            .total_size
            .ok_or_else(|| aioduct::Error::Other("server did not report content length".into()))?;
        progress.set_total(total_size);

        let control_path = ControlFile::control_path(&task.output);
        let piece_length =
            compute_piece_length(total_size, self.cli.split as u32, self.cli.piece_size);

        info!(
            total_size,
            piece_length,
            pieces = total_size.div_ceil(piece_length as u64),
            "starting segmented download"
        );

        let (storage, created_at) = resume_or_new_storage(
            task,
            &control_path,
            total_size,
            piece_length,
            &self.cli,
            progress,
        );

        if storage.all_complete() {
            progress.set_downloaded(total_size);
            let _ = std::fs::remove_file(&control_path);
            return Ok(total_size);
        }

        let disk_writer = Arc::new(
            DiskWriter::open_or_create(&task.output, total_size).map_err(aioduct::Error::Io)?,
        );

        let segment_man = Arc::new(SegmentMan::new(storage, self.cli.split as u32));
        let speed = Arc::new(std::sync::Mutex::new(SpeedMonitor::new(
            Duration::from_secs(5),
        )));
        let cancel = CancellationToken::new();

        let num_workers = self.cli.split.min(self.cli.max_connection_per_server);
        debug!(num_workers, "spawning workers");

        let worker_states = tui_state::new_worker_states(num_workers);
        let events = tui_state::new_event_log();

        let piece_grid = if !self.cli.plain {
            Some(PieceGrid::start(
                Arc::clone(&segment_man),
                total_size,
                task.output
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned(),
                num_workers,
                Arc::clone(&worker_states),
                Arc::clone(&events),
                cancel.clone(),
            ))
        } else {
            None
        };

        let worker_ctx = Arc::new(worker::WorkerContext {
            client: self.client.clone(),
            url: task.url.clone(),
            extra: Arc::clone(&self.extra),
            disk_writer: Arc::clone(&disk_writer),
            segment_man: Arc::clone(&segment_man),
            speed: Arc::clone(&speed),
            worker_states: Arc::clone(&worker_states),
            events: Arc::clone(&events),
            cancel: cancel.clone(),
            max_retries: self.cli.max_tries,
        });

        let mut handles = Vec::with_capacity(num_workers);
        for worker_id in 0..num_workers {
            let ctx = Arc::clone(&worker_ctx);
            let prog = progress.clone();
            handles.push(tokio::spawn(async move { ctx.run(worker_id, &prog).await }));
        }

        let cf_handle = spawn_checkpoint_task(
            Arc::clone(&segment_man),
            task.url.clone(),
            control_path.clone(),
            task.etag.clone(),
            task.last_modified.clone(),
            created_at.clone(),
            cancel.child_token(),
        );

        // Live progress polling for plain/indicatif mode
        let prog_handle = spawn_progress_poller(
            Arc::clone(&segment_man),
            Arc::clone(&worker_states),
            progress.clone(),
            piece_length,
            total_size,
            cancel.child_token(),
        );

        // Wait for all workers
        let mut first_error = None;
        for handle in handles {
            match handle.await {
                Ok(Ok(())) => {}
                Ok(Err(e)) => {
                    if first_error.is_none() {
                        first_error = Some(e);
                        cancel.cancel();
                    }
                }
                Err(e) => {
                    if first_error.is_none() {
                        first_error = Some(aioduct::Error::Other(Box::new(e)));
                        cancel.cancel();
                    }
                }
            }
        }

        cancel.cancel();
        let _ = cf_handle.await;
        let _ = prog_handle.await;
        if let Some(grid) = piece_grid {
            grid.stop().await;
        }

        if let Some(e) = first_error {
            save_control_file(
                &segment_man,
                &task.url,
                task.etag.as_deref(),
                task.last_modified.as_deref(),
                &created_at,
                &control_path,
            );
            return Err(e);
        }

        disk_writer.sync().map_err(aioduct::Error::Io)?;
        let all_done = segment_man.snapshot_storage(|s| s.all_complete());
        if all_done {
            let _ = std::fs::remove_file(&control_path);
        } else {
            save_control_file(
                &segment_man,
                &task.url,
                task.etag.as_deref(),
                task.last_modified.as_deref(),
                &created_at,
                &control_path,
            );
        }

        Ok(total_size)
    }

    fn resolve_output_path(&self, name: &str) -> PathBuf {
        if let Some(ref out) = self.cli.out {
            self.cli.dir.join(out)
        } else {
            let path = self.cli.dir.join(name);
            if !self.cli.no_continue && ControlFile::control_path(&path).exists() {
                path
            } else if !self.cli.allow_overwrite && self.cli.auto_file_renaming && path.exists() {
                filename::auto_rename(&path)
            } else {
                path
            }
        }
    }
}

fn now_string() -> String {
    use std::time::SystemTime;
    let d = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    format!("{}", d.as_secs())
}

fn resume_or_new_storage(
    task: &DownloadTask,
    control_path: &std::path::Path,
    total_size: u64,
    piece_length: u32,
    cli: &Cli,
    progress: &ProgressHandle,
) -> (PieceStorage, String) {
    if cli.no_continue {
        return (PieceStorage::new(total_size, piece_length), now_string());
    }

    match ControlFile::load(control_path) {
        Ok(cf) if cf.total_length == total_size && cf.piece_length == piece_length => {
            let etag_matches = match (&cf.etag, &task.etag) {
                (Some(a), Some(b)) => a == b,
                _ => true,
            };
            if !etag_matches {
                warn!("etag mismatch, starting fresh");
                return (PieceStorage::new(total_size, piece_length), now_string());
            }
            if let Some(storage) = cf.to_storage() {
                let completed = storage.completed_count();
                let already = completed as u64 * piece_length as u64;
                progress.set_downloaded(already.min(total_size));
                info!(completed, "resuming from control file");
                (storage, cf.created_at)
            } else {
                (PieceStorage::new(total_size, piece_length), now_string())
            }
        }
        _ => (PieceStorage::new(total_size, piece_length), now_string()),
    }
}

fn spawn_checkpoint_task(
    segment_man: Arc<SegmentMan>,
    url: String,
    control_path: PathBuf,
    etag: Option<String>,
    last_modified: Option<String>,
    created_at: String,
    cancel: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(5));
        loop {
            tokio::select! {
                _ = interval.tick() => {
                    segment_man.snapshot_storage(|storage| {
                        let cf = ControlFile::from_storage(
                            storage,
                            &url,
                            etag.as_deref(),
                            last_modified.as_deref(),
                            &created_at,
                        );
                        let _ = cf.save(&control_path);
                    });
                }
                _ = cancel.cancelled() => break,
            }
        }
    })
}

fn spawn_progress_poller(
    segment_man: Arc<SegmentMan>,
    worker_states: tui_state::SharedWorkerStates,
    progress: ProgressHandle,
    piece_length: u32,
    total_size: u64,
    cancel: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_millis(200));
        loop {
            tokio::select! {
                _ = interval.tick() => {
                    let completed_bytes = segment_man
                        .snapshot_storage(|s| s.completed_count()) as u64
                        * piece_length as u64;
                    let inflight_bytes: u64 = worker_states
                        .lock()
                        .unwrap()
                        .iter()
                        .filter(|w| w.current_piece.is_some())
                        .map(|w| w.downloaded_bytes())
                        .sum();
                    let total = (completed_bytes + inflight_bytes).min(total_size);
                    progress.set_downloaded(total);
                }
                _ = cancel.cancelled() => break,
            }
        }
    })
}

fn save_control_file(
    segment_man: &SegmentMan,
    url: &str,
    etag: Option<&str>,
    last_modified: Option<&str>,
    created_at: &str,
    control_path: &std::path::Path,
) {
    segment_man.snapshot_storage(|storage| {
        let cf = ControlFile::from_storage(storage, url, etag, last_modified, created_at);
        let _ = cf.save(control_path);
    });
}

fn compute_piece_length(total_length: u64, split_count: u32, user_override: Option<u64>) -> u32 {
    const MIN_PIECE: u32 = 256 * 1024;
    const MAX_PIECE: u32 = 16 * 1024 * 1024;

    if let Some(size) = user_override {
        return (size as u32).max(MIN_PIECE);
    }

    let raw = total_length / split_count.max(1) as u64;
    (raw as u32).clamp(MIN_PIECE, MAX_PIECE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compute_piece_length_basic() {
        assert_eq!(
            compute_piece_length(100 * 1024 * 1024, 4, None),
            16 * 1024 * 1024
        );
        assert_eq!(compute_piece_length(4 * 1024 * 1024, 4, None), 1024 * 1024);
        assert_eq!(compute_piece_length(512 * 1024, 4, None), 256 * 1024);
    }

    #[test]
    fn compute_piece_length_user_override() {
        assert_eq!(
            compute_piece_length(100 * 1024 * 1024, 4, Some(2 * 1024 * 1024)),
            2 * 1024 * 1024
        );
    }
}
