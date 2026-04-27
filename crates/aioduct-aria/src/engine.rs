use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use aioduct::runtime::TokioRuntime;
use aioduct::{Client, RetryConfig};
use tokio::fs::File;
use tokio::io::AsyncWriteExt;
use tokio::sync::Semaphore;

use crate::cli::Cli;
use crate::filename;
use crate::progress::ProgressHandle;
use crate::request_config::ExtraRequestConfig;
use crate::segment;

pub struct DownloadEngine {
    client: Client<TokioRuntime>,
    cli: Arc<Cli>,
    extra: Arc<ExtraRequestConfig>,
}

pub struct DownloadTask {
    pub url: String,
    pub output: PathBuf,
    pub total_size: Option<u64>,
    pub supports_range: bool,
}

pub struct DownloadResult {
    pub output: PathBuf,
    pub total_size: u64,
    pub error: Option<String>,
}

impl DownloadEngine {
    pub fn new(cli: Arc<Cli>) -> Self {
        let mut builder = Client::<TokioRuntime>::builder()
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

        if let Some(ref proxy_uri) = cli.all_proxy
            && let Ok(proxy) = aioduct::ProxyConfig::http(proxy_uri)
                .or_else(|_| aioduct::ProxyConfig::socks5(proxy_uri))
        {
            builder = builder.proxy(proxy);
        }

        let extra = Arc::new(ExtraRequestConfig::from_cli(&cli));
        let client = builder.build();
        Self { client, cli, extra }
    }

    pub async fn probe(&self, url: &str) -> Result<DownloadTask, aioduct::Error> {
        let req = self.client.head(url)?;
        let req = self.extra.apply_to(req);

        let resp = req.send().await?;
        let headers = resp.headers();

        let total_size = resp.content_length();
        let supports_range = headers
            .get("accept-ranges")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.contains("bytes"));

        let name = filename::from_url_and_headers(url, headers);
        let output = self.resolve_output_path(&name);

        Ok(DownloadTask {
            url: url.to_string(),
            output,
            total_size,
            supports_range,
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

        let resume_offset = if self.cli.continue_download {
            check_resume(&task.output).await
        } else {
            0
        };

        if resume_offset >= total_size {
            progress.set_downloaded(total_size);
            return Ok(total_size);
        }

        if self.cli.file_allocation == "prealloc" && resume_offset == 0 {
            preallocate_file(&task.output, total_size).await?;
        }

        let remaining = total_size - resume_offset;
        let num_segments =
            segment::compute_count(remaining, self.cli.split, self.cli.min_split_size);
        let segments = segment::split_range(resume_offset, total_size - 1, num_segments);

        let downloaded = Arc::new(AtomicU64::new(resume_offset));
        let failed = Arc::new(AtomicBool::new(false));

        progress.set_downloaded(resume_offset);

        let semaphore = Arc::new(Semaphore::new(self.cli.max_connection_per_server));
        let mut handles = Vec::with_capacity(segments.len());

        for seg in segments {
            let client = self.client.clone();
            let url = task.url.clone();
            let output = task.output.clone();
            let downloaded = Arc::clone(&downloaded);
            let failed = Arc::clone(&failed);
            let sem = Arc::clone(&semaphore);
            let progress = progress.clone();
            let extra = Arc::clone(&self.extra);

            handles.push(tokio::spawn(async move {
                // Semaphore is never closed
                let _permit = sem.acquire().await.unwrap();
                if failed.load(Ordering::Relaxed) {
                    return Err(aioduct::Error::Other(
                        "aborted due to earlier failure".into(),
                    ));
                }

                let result = segment::download_segment(&client, &url, &output, &seg, &extra).await;

                match result {
                    Ok(bytes_written) => {
                        let prev = downloaded.fetch_add(bytes_written, Ordering::Relaxed);
                        progress.set_downloaded(prev + bytes_written);
                        Ok(())
                    }
                    Err(e) => {
                        failed.store(true, Ordering::Relaxed);
                        Err(e)
                    }
                }
            }));
        }

        let mut first_error = None;
        for handle in handles {
            if let Err(e) = handle
                .await
                .map_err(|e| aioduct::Error::Other(Box::new(e)))?
                && first_error.is_none()
            {
                first_error = Some(e);
            }
        }

        if let Some(e) = first_error {
            return Err(e);
        }

        Ok(downloaded.load(Ordering::Relaxed))
    }

    fn resolve_output_path(&self, name: &str) -> PathBuf {
        if let Some(ref out) = self.cli.out {
            self.cli.dir.join(out)
        } else {
            let path = self.cli.dir.join(name);
            if !self.cli.allow_overwrite && self.cli.auto_file_renaming && path.exists() {
                filename::auto_rename(&path)
            } else {
                path
            }
        }
    }
}

async fn check_resume(path: &Path) -> u64 {
    match tokio::fs::metadata(path).await {
        Ok(meta) => meta.len(),
        Err(_) => 0,
    }
}

async fn preallocate_file(path: &Path, size: u64) -> Result<(), aioduct::Error> {
    let file = File::create(path).await.map_err(aioduct::Error::Io)?;
    file.set_len(size).await.map_err(aioduct::Error::Io)?;
    Ok(())
}
