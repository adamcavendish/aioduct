use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use indicatif::{MultiProgress, ProgressBar, ProgressStyle};

use crate::engine::DownloadResult;

pub struct ProgressTracker {
    multi: MultiProgress,
    quiet: bool,
    start: Instant,
}

#[derive(Clone)]
pub struct ProgressHandle {
    bar: ProgressBar,
    downloaded: Arc<AtomicU64>,
    total: Arc<AtomicU64>,
}

impl ProgressTracker {
    pub fn new(quiet: bool) -> Self {
        Self {
            multi: MultiProgress::new(),
            quiet,
            start: Instant::now(),
        }
    }

    pub fn add_download(&self, _url: &str, filename: &str) -> ProgressHandle {
        let bar = if self.quiet {
            ProgressBar::hidden()
        } else {
            let bar = self.multi.add(ProgressBar::new(0));
            bar.set_style(
                ProgressStyle::default_bar()
                    .template(
                        "{spinner:.green} [{bar:30.cyan/dim}] {bytes}/{total_bytes} ({bytes_per_sec}) ETA {eta} | {msg}",
                    )
                    .unwrap()
                    .progress_chars("=>-"),
            );
            bar.set_message(truncate_str(filename, 30).to_string());
            bar.enable_steady_tick(std::time::Duration::from_millis(100));
            bar
        };

        ProgressHandle {
            bar,
            downloaded: Arc::new(AtomicU64::new(0)),
            total: Arc::new(AtomicU64::new(0)),
        }
    }

    pub fn print_summary(&self, results: &[DownloadResult]) {
        if self.quiet {
            return;
        }

        let elapsed = self.start.elapsed();
        let ok_count = results.iter().filter(|r| r.error.is_none()).count();
        let err_count = results.iter().filter(|r| r.error.is_some()).count();
        let total_bytes: u64 = results.iter().map(|r| r.total_size).sum();

        println!();
        println!("Download Results:");
        println!("{:<6}|{:<5}|{:>12}|path/URI", "gid", "stat", "avg speed");
        println!("======+=====+============+==============================");

        for (i, r) in results.iter().enumerate() {
            let gid = format!("{:04x}", i);
            let stat = if r.error.is_some() { "ERR" } else { "OK" };
            let speed = if elapsed.as_secs() > 0 && r.error.is_none() {
                format_speed(r.total_size as f64 / elapsed.as_secs_f64())
            } else {
                "-".to_string()
            };
            let path = r.output.display().to_string();
            println!(" {gid:>4}| {stat:>3}|{speed:>11} | {path}");
            if let Some(ref err) = r.error {
                println!("      |     |            |  Error: {err}");
            }
        }

        println!();
        println!(
            "Status: {} completed, {} failed | Total: {} | Time: {:.1}s",
            ok_count,
            err_count,
            format_size(total_bytes),
            elapsed.as_secs_f64(),
        );
    }
}

impl ProgressHandle {
    pub fn set_total(&self, total: u64) {
        self.total.store(total, Ordering::Relaxed);
        self.bar.set_length(total);
    }

    pub fn set_downloaded(&self, bytes: u64) {
        self.downloaded.store(bytes, Ordering::Relaxed);
        self.bar.set_position(bytes);
    }

    pub fn finish_ok(&self) {
        self.bar.finish_with_message("done");
    }

    pub fn finish_err(&self, msg: &str) {
        self.bar
            .finish_with_message(format!("ERR: {}", truncate_str(msg, 40)));
    }
}

pub fn format_size(bytes: u64) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = KIB * 1024.0;
    const GIB: f64 = MIB * 1024.0;

    let b = bytes as f64;
    if b >= GIB {
        format!("{:.1}GiB", b / GIB)
    } else if b >= MIB {
        format!("{:.1}MiB", b / MIB)
    } else if b >= KIB {
        format!("{:.1}KiB", b / KIB)
    } else {
        format!("{bytes}B")
    }
}

fn format_speed(bytes_per_sec: f64) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = KIB * 1024.0;

    if bytes_per_sec >= MIB {
        format!("{:.1}MiB/s", bytes_per_sec / MIB)
    } else if bytes_per_sec >= KIB {
        format!("{:.1}KiB/s", bytes_per_sec / KIB)
    } else {
        format!("{:.0}B/s", bytes_per_sec)
    }
}

fn truncate_str(s: &str, max: usize) -> &str {
    if s.len() <= max { s } else { &s[..max] }
}
