mod cli;
mod control_file;
mod disk_writer;
mod endgame;
mod engine;
mod file_entry;
mod filename;
mod multi_file_tui;
mod piece;
mod piece_grid;
mod progress;
mod request_config;
mod scheduler;
mod segment_man;
mod speed_monitor;
mod tui_state;
mod webdav;
mod worker;

pub use cli::DownloadArgs;

use std::process::ExitCode;
use std::sync::Arc;

use cli::Cli;
use engine::DownloadEngine;
use progress::ProgressTracker;

struct ExpandedUri {
    url: String,
    known_size: Option<u64>,
    relative_path: Option<String>,
}

pub async fn run(args: DownloadArgs) -> ExitCode {
    let cli = args;

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

    // WebDAV recursive expansion
    let uris: Vec<ExpandedUri> = if cli.recursive {
        expand_webdav_uris(&engine, &cli, &uris).await
    } else {
        uris.into_iter()
            .map(|url| ExpandedUri {
                url,
                known_size: None,
                relative_path: None,
            })
            .collect()
    };

    if uris.is_empty() {
        eprintln!("No downloadable files found.");
        return ExitCode::from(28);
    }

    let tracker = ProgressTracker::new(cli.quiet, cli.plain);

    if cli.dry_run {
        return dry_run(&engine, &uris).await;
    }

    download_multi_mode(&engine, &cli, &tracker, &uris).await
}

async fn dry_run(engine: &DownloadEngine, uris: &[ExpandedUri]) -> ExitCode {
    for eu in uris {
        match engine
            .probe(&eu.url, eu.known_size, eu.relative_path.as_deref())
            .await
        {
            Ok(task) => {
                let size = task
                    .total_size
                    .map(progress::format_size)
                    .unwrap_or_else(|| "unknown".to_string());
                println!(
                    "{}\n  Output: {}\n  Size: {}\n  Range: {}\n",
                    eu.url,
                    task.output.display(),
                    size,
                    if task.supports_range { "yes" } else { "no" },
                );
            }
            Err(e) => {
                eprintln!("{}\n  Error: {e}\n", eu.url);
            }
        }
    }
    ExitCode::SUCCESS
}

async fn download_multi_mode(
    engine: &DownloadEngine,
    cli: &Cli,
    tracker: &ProgressTracker,
    uris: &[ExpandedUri],
) -> ExitCode {
    let mut tasks = Vec::new();
    let mut skipped = 0usize;
    for eu in uris {
        match engine
            .probe(&eu.url, eu.known_size, eu.relative_path.as_deref())
            .await
        {
            Ok(task) => {
                if task.output.exists()
                    && !control_file::ControlFile::control_path(&task.output).exists()
                {
                    if !cli.quiet {
                        eprintln!("[SKIP] {} (already complete)", task.output.display());
                    }
                    skipped += 1;
                    continue;
                }
                if !cli.quiet {
                    let size_str = task
                        .total_size
                        .map(progress::format_size)
                        .unwrap_or_else(|| "unknown".to_string());
                    let range_str = if task.supports_range { "yes" } else { "no" };
                    eprintln!(
                        "[INFO] {} | size: {} | range: {}",
                        task.output.display(),
                        size_str,
                        range_str,
                    );
                }
                tasks.push(task);
            }
            Err(e) => {
                if !cli.quiet {
                    eprintln!("[ERROR] {}: {e}", eu.url);
                }
            }
        }
    }

    if tasks.is_empty() {
        if skipped > 0 {
            if !cli.quiet {
                eprintln!(
                    "[INFO] All {} files already downloaded, nothing to do.",
                    skipped
                );
            }
            return ExitCode::SUCCESS;
        }
        return ExitCode::from(28);
    }

    let results = engine.download_multi(tasks).await;
    tracker.print_summary(&results);

    let has_errors = results.iter().any(|r| r.error.is_some());
    if has_errors {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

async fn expand_webdav_uris(
    engine: &DownloadEngine,
    cli: &Cli,
    uris: &[String],
) -> Vec<ExpandedUri> {
    let extra = request_config::ExtraRequestConfig::from_cli(cli);
    let max_depth = if cli.max_depth == 0 {
        None
    } else {
        Some(cli.max_depth)
    };

    let mut expanded = Vec::new();
    for uri in uris {
        if uri.ends_with('/') {
            if !cli.quiet {
                eprintln!("[INFO] Enumerating WebDAV directory: {uri}");
            }
            match webdav::enumerate(engine.client(), uri, &extra, max_depth).await {
                Ok(files) => {
                    if !cli.quiet {
                        eprintln!("[INFO] Found {} files in {uri}", files.len());
                    }
                    for f in files {
                        expanded.push(ExpandedUri {
                            url: f.url,
                            known_size: f.size,
                            relative_path: Some(f.relative_path),
                        });
                    }
                }
                Err(e) => {
                    eprintln!("[ERROR] WebDAV enumeration failed for {uri}: {e}");
                    expanded.push(ExpandedUri {
                        url: uri.clone(),
                        known_size: None,
                        relative_path: None,
                    });
                }
            }
        } else {
            expanded.push(ExpandedUri {
                url: uri.clone(),
                known_size: None,
                relative_path: None,
            });
        }
    }
    expanded
}

fn init_logging(cli: &Cli) {
    use tracing_subscriber::EnvFilter;

    let level = if cli.log.is_some() && cli.log_level == "warn" {
        "debug"
    } else {
        &cli.log_level
    };
    let filter = EnvFilter::try_new(level).unwrap_or_else(|_| EnvFilter::new("warn"));

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
