use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use aioduct::{RetryConfig, TokioClient};
use tokio::fs::File;
use tokio::io::AsyncWriteExt;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use super::checksum::{self, ChecksumSpec, SharedChecksumStatus};
use super::cli::Cli;
use super::control_file::ControlFile;
use super::disk_writer::DiskWriter;
use super::file_entry::{FileEntry, FileId};
use super::filename;
use super::multi_file_tui::MultiFileTui;
use super::piece::storage::PieceStorage;
use super::piece_grid::{PieceGrid, PieceGridTarget};
use super::progress::DownloadResult;
use super::progress::ProgressHandle;
use super::request_config::ExtraRequestConfig;
use super::scheduler::GlobalScheduler;
use super::segment_man::SegmentMan;
use super::speed_monitor::SpeedMonitor;
use super::tui_state;
use super::worker;

#[derive(Clone)]
pub struct DownloadEngine {
    client: TokioClient,
    cli: Arc<Cli>,
    extra: Arc<ExtraRequestConfig>,
    checksum: Option<ChecksumSpec>,
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
    pub fn client(&self) -> &TokioClient {
        &self.client
    }

    pub fn has_checksum(&self) -> bool {
        self.checksum.is_some()
    }

    pub async fn verify_existing_output(&self, output: &std::path::Path) -> DownloadResult {
        let total_size = std::fs::metadata(output)
            .map(|meta| meta.len())
            .unwrap_or(0);
        self.verify_output_path(output, total_size, None, None, None)
            .await
    }

    pub fn new(cli: Arc<Cli>, checksum: Option<ChecksumSpec>) -> Self {
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
            && let Some(proxy) = crate::util::parse_proxy_url(proxy_uri)
        {
            builder = builder.proxy(proxy);
        }

        let extra = Arc::new(ExtraRequestConfig::from_cli(&cli));
        let client = builder.build().unwrap();
        Self {
            client,
            cli,
            extra,
            checksum,
        }
    }

    pub async fn probe(
        &self,
        url: &str,
        known_size: Option<u64>,
        relative_path: Option<&str>,
    ) -> Result<DownloadTask, aioduct::Error> {
        debug!(url, "probing URL");

        let (total_size, supports_range, etag, last_modified, name) = if let Some(size) = known_size
        {
            // WebDAV already told us the size; skip HEAD to avoid stalling
            // on servers that don't handle HEAD well for large files.
            let name = filename::from_url(url);
            (Some(size), true, None, None, name)
        } else {
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

            (total_size, supports_range, etag, last_modified, name)
        };

        let output = self.resolve_output_path(&name, relative_path);

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
            Ok(size) => {
                self.verify_completed_download(task, size, None, None, None)
                    .await
            }
            Err(e) => DownloadResult {
                output: task.output.clone(),
                total_size: 0,
                error: Some(super::tui_state::sanitize_for_display(&e.to_string())),
                checksum: None,
            },
        }
    }

    pub async fn download_multi(&self, tasks: Vec<DownloadTask>) -> Vec<DownloadResult> {
        let num_workers = self.cli.split.min(self.cli.max_connection_per_server);
        let scheduler = Arc::new(GlobalScheduler::new(num_workers));
        let cancel = CancellationToken::new();
        let worker_states = tui_state::new_worker_states(num_workers);
        let events = tui_state::new_event_log();

        let mut results: Vec<Option<DownloadResult>> = (0..tasks.len()).map(|_| None).collect();
        let mut file_id_to_task_idx: Vec<(FileId, usize)> = Vec::new();
        let mut file_created_at: Vec<(FileId, String)> = Vec::new();
        let mut non_range_handles: Vec<(usize, tokio::task::JoinHandle<DownloadResult>)> =
            Vec::new();

        for (idx, task) in tasks.iter().enumerate() {
            if task.supports_range && task.total_size.is_some_and(|s| s > 0) {
                let total_size = task.total_size.unwrap();
                let piece_length =
                    compute_piece_length(total_size, self.cli.split as u32, self.cli.piece_size);
                let control_path = ControlFile::control_path(&task.output);
                let checksum_status = checksum::shared_status(self.checksum.as_ref());
                let (storage, created_at, resume_skipped_pieces) = resume_or_new_storage(
                    task,
                    &control_path,
                    total_size,
                    piece_length,
                    &self.cli,
                    &super::progress::ProgressHandle::hidden(),
                );

                if storage.all_complete() {
                    let result = self
                        .verify_completed_download(
                            task,
                            total_size,
                            Some(&checksum_status),
                            Some(&events),
                            Some(idx as FileId),
                        )
                        .await;
                    if result.error.is_none() {
                        let _ = std::fs::remove_file(&control_path);
                    }
                    results[idx] = Some(result);
                    continue;
                }

                if let Some(parent) = task.output.parent()
                    && let Err(e) = std::fs::create_dir_all(parent)
                {
                    results[idx] = Some(DownloadResult {
                        output: task.output.clone(),
                        total_size: 0,
                        error: Some(super::tui_state::sanitize_for_display(&e.to_string())),
                        checksum: None,
                    });
                    continue;
                }

                let disk_writer = match DiskWriter::open_or_create(&task.output, total_size) {
                    Ok(dw) => Arc::new(dw),
                    Err(e) => {
                        results[idx] = Some(DownloadResult {
                            output: task.output.clone(),
                            total_size: 0,
                            error: Some(super::tui_state::sanitize_for_display(&e.to_string())),
                            checksum: None,
                        });
                        continue;
                    }
                };

                let file_id = idx as FileId;
                let segment_man = Arc::new(SegmentMan::new(storage, self.cli.split as u32));
                let filename = task
                    .output
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned();

                let entry = FileEntry {
                    id: file_id,
                    url: task.url.clone(),
                    output: task.output.clone(),
                    filename,
                    total_size,
                    piece_length,
                    segment_man,
                    disk_writer,
                    control_path,
                    supports_range: true,
                    etag: task.etag.clone(),
                    last_modified: task.last_modified.clone(),
                    created_at: created_at.clone(),
                    resume_skipped_pieces,
                    checksum_status,
                };

                scheduler.add_file(entry);
                file_id_to_task_idx.push((file_id, idx));
                file_created_at.push((file_id, created_at));
            } else {
                // Non-range files: spawn concurrent download
                let engine = self.clone();
                let url = task.url.clone();
                let output = task.output.clone();
                non_range_handles.push((
                    idx,
                    tokio::spawn(async move {
                        let progress = super::progress::ProgressHandle::hidden();
                        let req = match engine.client.get(&url) {
                            Ok(r) => engine.extra.apply_to(r),
                            Err(e) => {
                                return DownloadResult {
                                    output,
                                    total_size: 0,
                                    error: Some(super::tui_state::sanitize_for_display(
                                        &e.to_string(),
                                    )),
                                    checksum: None,
                                };
                            }
                        };
                        match req.send().await {
                            Ok(resp) => {
                                let total = resp.content_length().unwrap_or(0);
                                progress.set_total(total);
                                let mut file = match File::create(&output).await {
                                    Ok(f) => f,
                                    Err(e) => {
                                        return DownloadResult {
                                            output,
                                            total_size: 0,
                                            error: Some(super::tui_state::sanitize_for_display(
                                                &e.to_string(),
                                            )),
                                            checksum: None,
                                        };
                                    }
                                };
                                let mut stream = resp.into_bytes_stream();
                                let mut downloaded: u64 = 0;
                                while let Some(chunk) = stream.next().await {
                                    match chunk {
                                        Ok(bytes) => {
                                            if let Err(e) = file.write_all(&bytes).await {
                                                return DownloadResult {
                                                    output,
                                                    total_size: 0,
                                                    error: Some(
                                                        super::tui_state::sanitize_for_display(
                                                            &e.to_string(),
                                                        ),
                                                    ),
                                                    checksum: None,
                                                };
                                            }
                                            downloaded += bytes.len() as u64;
                                        }
                                        Err(e) => {
                                            return DownloadResult {
                                                output,
                                                total_size: downloaded,
                                                error: Some(
                                                    super::tui_state::sanitize_for_display(
                                                        &e.to_string(),
                                                    ),
                                                ),
                                                checksum: None,
                                            };
                                        }
                                    }
                                }
                                let _ = file.flush().await;
                                engine
                                    .verify_output_path(&output, downloaded, None, None, None)
                                    .await
                            }
                            Err(e) => DownloadResult {
                                output,
                                total_size: 0,
                                error: Some(super::tui_state::sanitize_for_display(&e.to_string())),
                                checksum: None,
                            },
                        }
                    }),
                ));
            }
        }

        // If nothing to download in pool mode, await non-range and return
        if file_id_to_task_idx.is_empty() {
            for (idx, handle) in non_range_handles {
                match handle.await {
                    Ok(r) => results[idx] = Some(r),
                    Err(e) => {
                        results[idx] = Some(DownloadResult {
                            output: tasks[idx].output.clone(),
                            total_size: 0,
                            error: Some(super::tui_state::sanitize_for_display(&e.to_string())),
                            checksum: None,
                        });
                    }
                }
            }
            return results.into_iter().flatten().collect();
        }

        // Start multi-file TUI
        let multi_tui = if !self.cli.plain {
            Some(MultiFileTui::start(
                Arc::clone(&scheduler),
                Arc::clone(&worker_states),
                Arc::clone(&events),
                cancel.clone(),
                num_workers,
                tasks.len(),
            ))
        } else {
            None
        };

        // Spawn multi-file checkpoint task
        let checkpoint_scheduler = Arc::clone(&scheduler);
        let checkpoint_cancel = cancel.child_token();
        // Build checkpoint metadata indexed by file_id
        #[allow(clippy::type_complexity)]
        let mut checkpoint_meta: Vec<Option<(Option<String>, Option<String>, String)>> =
            vec![None; tasks.len()];
        for (file_id, idx) in &file_id_to_task_idx {
            let t = &tasks[*idx];
            let ca = file_created_at
                .iter()
                .find(|(id, _)| id == file_id)
                .map(|(_, s)| s.clone())
                .unwrap_or_default();
            checkpoint_meta[*file_id as usize] =
                Some((t.etag.clone(), t.last_modified.clone(), ca));
        }
        let checkpoint_handle = tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(5));
            loop {
                tokio::select! {
                    _ = interval.tick() => {
                        checkpoint_scheduler.for_each_active_file(|file| {
                            let fid = file.id as usize;
                            if let Some(Some((etag, last_mod, created_at))) = checkpoint_meta.get(fid) {
                                file.segment_man.snapshot_storage(|storage| {
                                    let cf = ControlFile::from_storage(
                                        storage,
                                        &file.url,
                                        etag.as_deref(),
                                        last_mod.as_deref(),
                                        created_at,
                                    );
                                    let _ = cf.save(&file.control_path);
                                });
                            }
                        });
                    }
                    _ = checkpoint_cancel.cancelled() => break,
                }
            }
        });

        // Spawn pool workers
        let pool_ctx = Arc::new(worker::PoolWorkerContext {
            client: self.client.clone(),
            extra: Arc::clone(&self.extra),
            scheduler: Arc::clone(&scheduler),
            worker_states: Arc::clone(&worker_states),
            events: Arc::clone(&events),
            cancel: cancel.clone(),
            max_retries: self.cli.max_tries,
        });

        let mut handles = Vec::with_capacity(num_workers);
        for worker_id in 0..num_workers {
            let ctx = Arc::clone(&pool_ctx);
            handles.push(tokio::spawn(async move { ctx.run(worker_id).await }));
        }

        // Wait for all workers
        for handle in handles {
            match handle.await {
                Ok(Ok(())) => {}
                Ok(Err(e)) => {
                    warn!(error = %e, "pool worker error");
                }
                Err(e) => {
                    warn!(error = %e, "pool worker panicked");
                }
            }
        }

        cancel.cancel();
        let _ = checkpoint_handle.await;
        if let Some(tui) = multi_tui {
            tui.stop().await;
        }

        // Clean up control files for completed files, save for incomplete
        for (file_id, _) in &file_id_to_task_idx {
            let fid = *file_id as usize;
            let task = &tasks[fid];
            let control_path = ControlFile::control_path(&task.output);
            let snap = scheduler
                .snapshot_files()
                .iter()
                .find(|s| s.id == *file_id)
                .cloned();
            if let Some(snap) = snap
                && snap.remaining_pieces == 0
                && self.checksum.is_none()
            {
                let _ = std::fs::remove_file(&control_path);
            }
        }

        // Collect results for pool files
        let snapshots = scheduler.snapshot_files();
        for (file_id, _task_idx) in &file_id_to_task_idx {
            let idx = *file_id as usize;
            if let Some(snap) = snapshots.iter().find(|s| s.id == *file_id) {
                let task = &tasks[idx];
                let error = if snap.remaining_pieces > 0 {
                    Some("download incomplete".to_string())
                } else {
                    None
                };
                results[idx] = if error.is_some() {
                    Some(DownloadResult {
                        output: task.output.clone(),
                        total_size: snap.total_size,
                        error,
                        checksum: None,
                    })
                } else {
                    Some(
                        self.verify_completed_download(
                            task,
                            snap.total_size,
                            None,
                            Some(&events),
                            Some(*file_id),
                        )
                        .await,
                    )
                };
                if let Some(result) = &results[idx]
                    && result.error.is_none()
                {
                    let control_path = ControlFile::control_path(&task.output);
                    let _ = std::fs::remove_file(&control_path);
                }
            }
        }

        // Await non-range downloads
        for (idx, handle) in non_range_handles {
            match handle.await {
                Ok(r) => results[idx] = Some(r),
                Err(e) => {
                    results[idx] = Some(DownloadResult {
                        output: tasks[idx].output.clone(),
                        total_size: 0,
                        error: Some(super::tui_state::sanitize_for_display(&e.to_string())),
                        checksum: None,
                    });
                }
            }
        }

        results.into_iter().flatten().collect()
    }

    async fn verify_completed_download(
        &self,
        task: &DownloadTask,
        total_size: u64,
        checksum_status: Option<&SharedChecksumStatus>,
        events: Option<&tui_state::SharedEventLog>,
        file_id: Option<FileId>,
    ) -> DownloadResult {
        self.verify_output_path(&task.output, total_size, checksum_status, events, file_id)
            .await
    }

    async fn verify_output_path(
        &self,
        output: &std::path::Path,
        total_size: u64,
        checksum_status: Option<&SharedChecksumStatus>,
        events: Option<&tui_state::SharedEventLog>,
        file_id: Option<FileId>,
    ) -> DownloadResult {
        let Some(spec) = self.checksum.as_ref() else {
            return DownloadResult {
                output: output.to_path_buf(),
                total_size,
                error: None,
                checksum: None,
            };
        };

        let file_name = output
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        if let Some(status) = checksum_status {
            checksum::set_status(status, format!("{} verifying", spec.algorithm_label()));
        }
        push_checksum_event(
            events,
            tui_state::EventSeverity::Info,
            file_id,
            &file_name,
            format!("verifying {}", spec.algorithm_label()),
        );

        match checksum::verify_file(output, spec).await {
            Ok(report) => {
                let status = report.status_label();
                if let Some(checksum_status) = checksum_status {
                    checksum::set_status(checksum_status, status.clone());
                }
                push_checksum_event(
                    events,
                    tui_state::EventSeverity::Info,
                    file_id,
                    &file_name,
                    status.clone(),
                );
                DownloadResult {
                    output: output.to_path_buf(),
                    total_size,
                    error: None,
                    checksum: Some(status),
                }
            }
            Err(checksum::ChecksumError::Mismatch(report)) => {
                let status = report.status_label();
                let message = super::tui_state::sanitize_for_display(&report.summary());
                if let Some(checksum_status) = checksum_status {
                    checksum::set_status(checksum_status, status.clone());
                }
                push_checksum_event(
                    events,
                    tui_state::EventSeverity::Error,
                    file_id,
                    &file_name,
                    message.clone(),
                );
                DownloadResult {
                    output: output.to_path_buf(),
                    total_size,
                    error: Some(message),
                    checksum: Some(status),
                }
            }
            Err(e) => {
                let message = super::tui_state::sanitize_for_display(&e.to_string());
                if let Some(checksum_status) = checksum_status {
                    checksum::set_status(
                        checksum_status,
                        format!("{} failed", spec.algorithm_label()),
                    );
                }
                push_checksum_event(
                    events,
                    tui_state::EventSeverity::Error,
                    file_id,
                    &file_name,
                    message.clone(),
                );
                DownloadResult {
                    output: output.to_path_buf(),
                    total_size,
                    error: Some(message),
                    checksum: None,
                }
            }
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

        if let Some(parent) = task.output.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(aioduct::Error::Io)?;
        }

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

        let (storage, created_at, _resume_skipped_pieces) = resume_or_new_storage(
            task,
            &control_path,
            total_size,
            piece_length,
            &self.cli,
            progress,
        );

        if storage.all_complete() {
            progress.set_downloaded(total_size);
            if self.checksum.is_none() {
                let _ = std::fs::remove_file(&control_path);
            }
            return Ok(total_size);
        }

        if let Some(parent) = task.output.parent() {
            std::fs::create_dir_all(parent).map_err(aioduct::Error::Io)?;
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
        let checksum_status = checksum::shared_status(self.checksum.as_ref());

        let piece_grid = if !self.cli.plain {
            let filename = task
                .output
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            Some(PieceGrid::start(
                Arc::clone(&segment_man),
                total_size,
                PieceGridTarget {
                    url: task.url.clone(),
                    output: task.output.clone(),
                    filename,
                    control_path: control_path.clone(),
                    supports_range: task.supports_range,
                    etag: task.etag.clone(),
                    last_modified: task.last_modified.clone(),
                    created_at: created_at.clone(),
                    resume_skipped_pieces: _resume_skipped_pieces,
                    allocation: "preallocated",
                    checksum_status: Arc::clone(&checksum_status),
                },
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

    fn resolve_output_path(&self, name: &str, relative_path: Option<&str>) -> PathBuf {
        if let Some(ref out) = self.cli.out {
            self.cli.dir.join(out)
        } else {
            let path = match relative_path {
                Some(rel) => self.cli.dir.join(rel),
                None => self.cli.dir.join(name),
            };
            if !self.cli.no_resume && ControlFile::control_path(&path).exists() {
                path
            } else if !self.cli.allow_overwrite && self.cli.auto_file_renaming && path.exists() {
                filename::auto_rename(&path)
            } else {
                path
            }
        }
    }
}

fn push_checksum_event(
    events: Option<&tui_state::SharedEventLog>,
    severity: tui_state::EventSeverity,
    file_id: Option<FileId>,
    file_name: &str,
    message: String,
) {
    let Some(events) = events else {
        return;
    };
    let mut event =
        tui_state::DownloadEvent::new(severity, tui_state::EventCategory::Checksum, message);
    if let Some(file_id) = file_id {
        event = event.file(file_id, file_name.to_string());
    }
    tui_state::push_typed_event(events, event);
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
) -> (PieceStorage, String, u32) {
    if cli.no_resume {
        return (PieceStorage::new(total_size, piece_length), now_string(), 0);
    }

    match ControlFile::load(control_path) {
        Ok(cf) if cf.total_length == total_size && cf.piece_length == piece_length => {
            let etag_matches = match (&cf.etag, &task.etag) {
                (Some(a), Some(b)) => a == b,
                _ => true,
            };
            if !etag_matches {
                warn!("etag mismatch, starting fresh");
                return (PieceStorage::new(total_size, piece_length), now_string(), 0);
            }
            if let Some(storage) = cf.to_storage() {
                let completed = storage.completed_count();
                let already = completed as u64 * piece_length as u64;
                progress.set_downloaded(already.min(total_size));
                info!(completed, "resuming from control file");
                (storage, cf.created_at, completed)
            } else {
                (PieceStorage::new(total_size, piece_length), now_string(), 0)
            }
        }
        _ => (PieceStorage::new(total_size, piece_length), now_string(), 0),
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
    const MIN_PIECE: u32 = 64 * 1024;
    const MAX_PIECE: u32 = 4 * 1024 * 1024;
    const TARGET_PIECES_PER_SPLIT: u32 = 4;

    if let Some(size) = user_override {
        return size.clamp(MIN_PIECE as u64, u32::MAX as u64) as u32;
    }

    let target_pieces = split_count.max(1).saturating_mul(TARGET_PIECES_PER_SPLIT) as u64;
    let raw = total_length.div_ceil(target_pieces);
    raw.clamp(MIN_PIECE as u64, MAX_PIECE as u64) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compute_piece_length_basic() {
        assert_eq!(compute_piece_length(100 * 1024 * 1024, 8, None), 3_276_800);
        assert_eq!(compute_piece_length(10 * 1024 * 1024, 8, None), 327_680);
        assert_eq!(
            compute_piece_length(1024 * 1024 * 1024, 8, None),
            4 * 1024 * 1024
        );
        assert_eq!(compute_piece_length(512 * 1024, 8, None), 64 * 1024);
    }

    #[test]
    fn compute_piece_length_user_override() {
        assert_eq!(
            compute_piece_length(100 * 1024 * 1024, 4, Some(2 * 1024 * 1024)),
            2 * 1024 * 1024
        );
        assert_eq!(
            compute_piece_length(100 * 1024 * 1024, 4, Some(32 * 1024)),
            64 * 1024
        );
        assert_eq!(
            compute_piece_length(100 * 1024 * 1024, 4, Some(16 * 1024 * 1024)),
            16 * 1024 * 1024
        );
    }
}
