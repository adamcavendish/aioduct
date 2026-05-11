//! Sorted-vec histogram for percentile computation.

/// Computes percentile statistics from a collected set of f64 values.
pub struct Histogram {
    values: Vec<f64>,
    sorted: bool,
}

impl Histogram {
    pub fn new() -> Self {
        Self {
            values: Vec::new(),
            sorted: false,
        }
    }

    pub fn with_capacity(cap: usize) -> Self {
        Self {
            values: Vec::with_capacity(cap),
            sorted: false,
        }
    }

    pub fn add(&mut self, value: f64) {
        self.values.push(value);
        self.sorted = false;
    }

    pub fn count(&self) -> usize {
        self.values.len()
    }

    fn ensure_sorted(&mut self) {
        if !self.sorted {
            self.values
                .sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            self.sorted = true;
        }
    }

    pub fn min(&mut self) -> f64 {
        self.ensure_sorted();
        self.values.first().copied().unwrap_or(0.0)
    }

    pub fn max(&mut self) -> f64 {
        self.ensure_sorted();
        self.values.last().copied().unwrap_or(0.0)
    }

    pub fn avg(&self) -> f64 {
        if self.values.is_empty() {
            return 0.0;
        }
        self.values.iter().sum::<f64>() / self.values.len() as f64
    }

    /// Percentile (0.0 to 1.0). E.g. 0.95 for p95.
    pub fn percentile(&mut self, p: f64) -> f64 {
        self.ensure_sorted();
        if self.values.is_empty() {
            return 0.0;
        }
        let idx = ((self.values.len() as f64 - 1.0) * p).round() as usize;
        let idx = idx.min(self.values.len() - 1);
        self.values[idx]
    }

    pub fn median(&mut self) -> f64 {
        self.percentile(0.5)
    }

    pub fn p90(&mut self) -> f64 {
        self.percentile(0.90)
    }

    pub fn p95(&mut self) -> f64 {
        self.percentile(0.95)
    }

    pub fn p99(&mut self) -> f64 {
        self.percentile(0.99)
    }
}

impl Default for Histogram {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_percentiles() {
        let mut h = Histogram::new();
        for i in 1..=100 {
            h.add(i as f64);
        }
        assert_eq!(h.count(), 100);
        assert_eq!(h.min(), 1.0);
        assert_eq!(h.max(), 100.0);
        assert!((h.avg() - 50.5).abs() < 0.01);
        // median index = round(99 * 0.5) = 50 → values[50] = 51
        assert_eq!(h.median(), 51.0);
        assert_eq!(h.p90(), 90.0);
        assert_eq!(h.p95(), 95.0);
        assert_eq!(h.p99(), 99.0);
    }

    #[test]
    fn empty_histogram() {
        let mut h = Histogram::new();
        assert_eq!(h.min(), 0.0);
        assert_eq!(h.max(), 0.0);
        assert_eq!(h.avg(), 0.0);
        assert_eq!(h.percentile(0.5), 0.0);
    }
}
