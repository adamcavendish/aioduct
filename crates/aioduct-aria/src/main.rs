mod cli;
mod engine;
mod filename;
mod progress;
mod request_config;
mod segment;

use std::process::ExitCode;
use std::sync::Arc;

use clap::Parser;
use tokio::sync::Semaphore;

use cli::Cli;
use engine::{DownloadEngine, DownloadResult};
use progress::ProgressTracker;

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();

    init_logging(&cli);

    let uris = match cli.all_uris() {
        Ok(uris) => uris,
        Err(e) => {
            eprintln!("Error reading input file: {e}");
            return ExitCode::from(28);
        }
    };

    if uris.is_empty() {
        eprintln!("No URIs specified. Use --help for usage.");
        return ExitCode::from(28);
    }

    if cli.out.is_some() && uris.len() > 1 {
        eprintln!("Error: -o/--out can only be used with a single URI");
        return ExitCode::from(28);
    }

    if let Err(e) = tokio::fs::create_dir_all(&cli.dir).await {
        eprintln!("Error creating output directory: {e}");
        return ExitCode::from(15);
    }

    let cli = Arc::new(cli);
    let engine = DownloadEngine::new(Arc::clone(&cli));
    let tracker = ProgressTracker::new(cli.quiet);

    if cli.dry_run {
        return dry_run(&engine, &uris).await;
    }

    let semaphore = Arc::new(Semaphore::new(cli.max_concurrent_downloads));
    let mut results: Vec<DownloadResult> = Vec::with_capacity(uris.len());

    let mut handles = Vec::new();

    for url in &uris {
        let permit = semaphore.clone().acquire_owned().await.unwrap();

        let task = match engine.probe(url).await {
            Ok(t) => t,
            Err(e) => {
                if !cli.quiet {
                    eprintln!("[ERROR] {url}: {e}");
                }
                results.push(DownloadResult {
                    url: url.clone(),
                    output: cli.dir.join("unknown"),
                    total_size: 0,
                    error: Some(e.to_string()),
                });
                continue;
            }
        };

        if !cli.quiet {
            let size_str = task
                .total_size
                .map(progress::format_size)
                .unwrap_or_else(|| "unknown".to_string());
            let range_str = if task.supports_range { "yes" } else { "no" };
            eprintln!(
                "[INFO] {} | size: {} | range: {} | segments: {}",
                task.output.display(),
                size_str,
                range_str,
                if task.supports_range { cli.split } else { 1 },
            );
        }

        let progress = tracker.add_download(url, &task.output.display().to_string());

        let engine_client = DownloadEngine::new(Arc::clone(&cli));
        handles.push(tokio::spawn(async move {
            let result = engine_client.download(&task, &progress).await;
            if result.error.is_some() {
                progress.finish_err(result.error.as_deref().unwrap_or("unknown"));
            } else {
                progress.finish_ok();
            }
            drop(permit);
            result
        }));
    }

    for handle in handles {
        match handle.await {
            Ok(result) => results.push(result),
            Err(e) => {
                eprintln!("[ERROR] task panicked: {e}");
            }
        }
    }

    tracker.print_summary(&results);

    let has_errors = results.iter().any(|r| r.error.is_some());
    if has_errors {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

async fn dry_run(engine: &DownloadEngine, uris: &[String]) -> ExitCode {
    for url in uris {
        match engine.probe(url).await {
            Ok(task) => {
                let size = task
                    .total_size
                    .map(progress::format_size)
                    .unwrap_or_else(|| "unknown".to_string());
                println!(
                    "{}\n  Output: {}\n  Size: {}\n  Range: {}\n",
                    url,
                    task.output.display(),
                    size,
                    if task.supports_range { "yes" } else { "no" },
                );
            }
            Err(e) => {
                eprintln!("{url}\n  Error: {e}\n");
            }
        }
    }
    ExitCode::SUCCESS
}

fn init_logging(cli: &Cli) {
    use tracing_subscriber::EnvFilter;

    let filter = EnvFilter::try_new(&cli.log_level).unwrap_or_else(|_| EnvFilter::new("warn"));

    let builder = tracing_subscriber::fmt().with_env_filter(filter);

    match &cli.log {
        Some(path) if path.display().to_string() == "-" => {
            builder.with_writer(std::io::stdout).init();
        }
        Some(path) => {
            let file = std::fs::File::create(path).expect("failed to create log file");
            builder
                .with_writer(std::sync::Mutex::new(file))
                .with_ansi(false)
                .init();
        }
        None => {
            builder.with_writer(std::io::stderr).init();
        }
    }
}
