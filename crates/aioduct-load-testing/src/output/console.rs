//! Console output — live progress bar and end-of-test summary table.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use indicatif::{ProgressBar, ProgressStyle};

use crate::metrics::{RequestRecord, Sample, TestSummary};
use crate::output::Output;

/// Console output with live progress bar and end-of-test summary.
pub struct ConsoleOutput {
    state: Arc<ConsoleState>,
}

struct ConsoleState {
    progress: ProgressBar,
    start: Instant,
    success: AtomicU64,
    fail: AtomicU64,
    total: AtomicU64,
}

impl ConsoleOutput {
    /// Create a new console output. If `total_requests` is known, shows a progress bar;
    /// otherwise shows a spinner.
    pub fn new(total_requests: Option<u64>) -> Self {
        let progress = if let Some(total) = total_requests {
            let pb = ProgressBar::new(total);
            pb.set_style(
                ProgressStyle::with_template(
                    "{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {pos}/{len} ({per_sec}) {msg}",
                )
                .unwrap()
                .progress_chars("=>-"),
            );
            pb
        } else {
            let pb = ProgressBar::new_spinner();
            pb.set_style(
                ProgressStyle::with_template(
                    "{spinner:.green} [{elapsed_precise}] {pos} requests ({per_sec}) {msg}",
                )
                .unwrap(),
            );
            pb
        };

        Self {
            state: Arc::new(ConsoleState {
                progress,
                start: Instant::now(),
                success: AtomicU64::new(0),
                fail: AtomicU64::new(0),
                total: AtomicU64::new(0),
            }),
        }
    }
}

impl Output for ConsoleOutput {
    fn record(&self, _sample: &Sample) {}

    fn request_done(&self, record: &RequestRecord) {
        self.state.total.fetch_add(1, Ordering::Relaxed);
        if record.success == 1 {
            self.state.success.fetch_add(1, Ordering::Relaxed);
        } else {
            self.state.fail.fetch_add(1, Ordering::Relaxed);
        }
        self.state.progress.inc(1);

        let ok = self.state.success.load(Ordering::Relaxed);
        let fail = self.state.fail.load(Ordering::Relaxed);
        self.state
            .progress
            .set_message(format!("ok={ok} fail={fail}"));
    }

    fn summary(&self, summary: &TestSummary) {
        self.state.progress.finish_and_clear();
        let elapsed = self.state.start.elapsed();

        println!();
        println!("═══════════════════════════════════════════════════");
        println!("  Load Test Summary");
        println!("═══════════════════════════════════════════════════");
        println!("  Duration:    {:.1}s", elapsed.as_secs_f64());
        println!("  Total:       {}", summary.total_requests);
        println!("  Successful:  {}", summary.successful_requests);
        println!("  Failed:      {}", summary.failed_requests);
        println!("  RPS:         {:.1}", summary.requests_per_second);
        println!("───────────────────────────────────────────────────");

        if !summary.metrics.is_empty() {
            println!(
                "  {:20} {:>8} {:>8} {:>8} {:>8} {:>8}",
                "Metric", "avg", "med", "p90", "p95", "p99"
            );
            println!("  {}", "─".repeat(68));
            for m in &summary.metrics {
                println!(
                    "  {:20} {:>8.1} {:>8.1} {:>8.1} {:>8.1} {:>8.1}",
                    m.name, m.avg, m.med, m.p90, m.p95, m.p99
                );
            }
        }
        println!("═══════════════════════════════════════════════════");
        println!();
    }

    fn flush(&self) {
        self.state.progress.tick();
    }
}
