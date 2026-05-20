use std::collections::VecDeque;
use std::time::{Duration, Instant};

pub struct SpeedMonitor {
    samples: VecDeque<(Instant, u64)>,
    window: Duration,
    total_bytes: u64,
}

impl SpeedMonitor {
    pub fn new(window: Duration) -> Self {
        Self {
            samples: VecDeque::new(),
            window,
            total_bytes: 0,
        }
    }

    pub fn record(&mut self, bytes: u64) {
        let now = Instant::now();
        self.samples.push_back((now, bytes));
        self.total_bytes += bytes;
        self.prune(now);
    }

    pub fn speed_bytes_per_sec(&mut self) -> f64 {
        let now = Instant::now();
        self.prune(now);

        if self.samples.is_empty() {
            return 0.0;
        }

        let window_bytes: u64 = self.samples.iter().map(|(_, b)| *b).sum();
        let elapsed = if let Some((first_time, _)) = self.samples.front() {
            now.duration_since(*first_time).as_secs_f64()
        } else {
            0.0
        };

        if elapsed < 0.001 {
            return 0.0;
        }

        window_bytes as f64 / elapsed
    }

    pub fn total_bytes(&self) -> u64 {
        self.total_bytes
    }

    fn prune(&mut self, now: Instant) {
        while let Some((t, _)) = self.samples.front() {
            if now.duration_since(*t) > self.window {
                self.samples.pop_front();
            } else {
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn speed_monitor_zero_when_empty() {
        let mut m = SpeedMonitor::new(Duration::from_secs(5));
        assert_eq!(m.speed_bytes_per_sec(), 0.0);
    }
}
