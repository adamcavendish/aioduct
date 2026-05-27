use std::io::IsTerminal;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use indicatif::{MultiProgress, ProgressBar, ProgressStyle};

pub struct DownloadResult {
    pub output: PathBuf,
    pub total_size: u64,
    pub error: Option<String>,
    pub checksum: Option<String>,
}

pub struct ProgressTracker {
    multi: MultiProgress,
    quiet: bool,
    is_tty: bool,
    use_tui: bool,
    start: Instant,
}

#[derive(Clone)]
pub struct ProgressHandle {
    bar: ProgressBar,
    downloaded: Arc<AtomicU64>,
    total: Arc<AtomicU64>,
}

impl ProgressTracker {
    pub fn new(quiet: bool, plain: bool) -> Self {
        let is_tty = std::io::stderr().is_terminal();
        let use_tui = is_tty && !plain;
        Self {
            multi: MultiProgress::new(),
            quiet,
            is_tty,
            use_tui,
            start: Instant::now(),
        }
    }

    pub fn add_download(&self, _url: &str, filename: &str) -> ProgressHandle {
        let bar = if self.quiet || self.use_tui {
            ProgressBar::hidden()
        } else if self.is_tty {
            let bar = self.multi.add(ProgressBar::new(0));
            bar.set_style(
                ProgressStyle::default_bar()
                    .template(
                        "{spinner:.green} [{bar:30.cyan/blue}] \
                         {bytes:.green}/{total_bytes:.green} \
                         ({percent:.bold.white}%) \
                         {binary_bytes_per_sec:.yellow} \
                         ETA {eta:.cyan} \
                         | {msg:.magenta}",
                    )
                    .unwrap()
                    .progress_chars("━╸─"),
            );
            bar.set_message(crate::util::truncate_str(filename, 30).to_string());
            bar.enable_steady_tick(std::time::Duration::from_millis(100));
            bar
        } else {
            // Non-TTY: use simple line-based output (like aria2 with --console-log-level=notice)
            let bar = self.multi.add(ProgressBar::new(0));
            bar.set_style(
                ProgressStyle::default_bar()
                    .template("[{elapsed_precise}] {bytes}/{total_bytes} ({percent}%) {binary_bytes_per_sec} | {msg}")
                    .unwrap(),
            );
            bar.set_message(crate::util::truncate_str(filename, 40).to_string());
            bar.enable_steady_tick(std::time::Duration::from_secs(1));
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

        if self.is_tty {
            self.print_summary_color(results, elapsed, ok_count, err_count, total_bytes);
        } else {
            self.print_summary_plain(results, elapsed, ok_count, err_count, total_bytes);
        }
    }

    fn print_summary_color(
        &self,
        results: &[DownloadResult],
        elapsed: std::time::Duration,
        ok_count: usize,
        err_count: usize,
        total_bytes: u64,
    ) {
        println!();
        println!("\x1b[1;35mDownload Results:\x1b[0m");
        println!(
            "\x1b[36m{:<6}\x1b[0m|\x1b[36m{:<5}\x1b[0m|\x1b[36m{:>12}\x1b[0m|path/URI",
            "gid", "stat", "avg speed"
        );
        println!("\x1b[90m======+=====+============+==============================\x1b[0m");

        for (i, r) in results.iter().enumerate() {
            let gid = format!("{:04x}", i);
            let (stat, stat_color) = if r.error.is_some() {
                ("ERR", "\x1b[1;31m")
            } else {
                ("OK", "\x1b[1;32m")
            };
            let speed = if elapsed.as_secs() > 0 && r.error.is_none() {
                crate::util::human_speed(r.total_size as f64 / elapsed.as_secs_f64())
            } else {
                "-".to_string()
            };
            let path = r.output.display().to_string();
            println!(
                " \x1b[36m{gid:>4}\x1b[0m|{stat_color} {stat:>3}\x1b[0m|\x1b[33m{speed:>11}\x1b[0m | {path}"
            );
            if let Some(ref err) = r.error {
                println!("      |     |            |  \x1b[31mError: {err}\x1b[0m");
            }
            if let Some(ref checksum) = r.checksum {
                println!("      |     |            |  \x1b[36mIntegrity: {checksum}\x1b[0m");
            }
        }

        println!();
        let status_color = if err_count > 0 {
            "\x1b[1;33m"
        } else {
            "\x1b[1;32m"
        };
        println!(
            "{status_color}Status: {ok_count} completed, {err_count} failed\x1b[0m | \
             Total: \x1b[1;36m{}\x1b[0m | Time: \x1b[1m{:.1}s\x1b[0m",
            format_size(total_bytes),
            elapsed.as_secs_f64(),
        );
    }

    fn print_summary_plain(
        &self,
        results: &[DownloadResult],
        elapsed: std::time::Duration,
        ok_count: usize,
        err_count: usize,
        total_bytes: u64,
    ) {
        println!();
        println!("Download Results:");
        println!("{:<6}|{:<5}|{:>12}|path/URI", "gid", "stat", "avg speed");
        println!("======+=====+============+==============================");

        for (i, r) in results.iter().enumerate() {
            let gid = format!("{:04x}", i);
            let stat = if r.error.is_some() { "ERR" } else { "OK" };
            let speed = if elapsed.as_secs() > 0 && r.error.is_none() {
                crate::util::human_speed(r.total_size as f64 / elapsed.as_secs_f64())
            } else {
                "-".to_string()
            };
            let path = r.output.display().to_string();
            println!(" {gid:>4}| {stat:>3}|{speed:>11} | {path}");
            if let Some(ref err) = r.error {
                println!("      |     |            |  Error: {err}");
            }
            if let Some(ref checksum) = r.checksum {
                println!("      |     |            |  Integrity: {checksum}");
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
    pub fn hidden() -> Self {
        Self {
            bar: ProgressBar::hidden(),
            downloaded: Arc::new(AtomicU64::new(0)),
            total: Arc::new(AtomicU64::new(0)),
        }
    }

    pub fn set_total(&self, total: u64) {
        self.total.store(total, Ordering::Relaxed);
        self.bar.set_length(total);
    }

    pub fn set_downloaded(&self, bytes: u64) {
        self.downloaded.store(bytes, Ordering::Relaxed);
        self.bar.set_position(bytes);
    }

    pub fn add_downloaded(&self, bytes: u64) {
        let prev = self.downloaded.fetch_add(bytes, Ordering::Relaxed);
        self.bar.set_position(prev + bytes);
    }

    pub fn finish_ok(&self) {
        self.bar.finish_with_message("✓ done");
    }

    pub fn finish_err(&self, msg: &str) {
        self.bar
            .finish_with_message(format!("✗ {}", crate::util::truncate_str(msg, 40)));
    }
}

pub fn format_size(bytes: u64) -> String {
    crate::util::human_bytes(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_size_bytes() {
        assert_eq!(format_size(0), "0 B");
        assert_eq!(format_size(512), "512 B");
        assert_eq!(format_size(1023), "1023 B");
    }

    #[test]
    fn format_size_kib() {
        assert_eq!(format_size(1024), "1 KiB");
        assert_eq!(format_size(1024 + 512), "1.5 KiB");
    }

    #[test]
    fn format_size_mib() {
        assert_eq!(format_size(1024 * 1024), "1 MiB");
        assert_eq!(format_size(5 * 1024 * 1024 + 512 * 1024), "5.5 MiB");
    }

    #[test]
    fn format_size_gib() {
        assert_eq!(format_size(1024 * 1024 * 1024), "1 GiB");
        assert_eq!(
            format_size(2 * 1024 * 1024 * 1024 + 512 * 1024 * 1024),
            "2.5 GiB"
        );
    }
}
