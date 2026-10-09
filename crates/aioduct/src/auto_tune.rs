//! Deterministic request transport selection.

/// Configuration for the opt-in automatic transport selection policy.
///
/// When enabled on a client builder, requests with a known body size at or
/// above the configured threshold (16 MiB by default) use HTTP/1.1. Requests whose
/// body size is unknown, or is below the threshold, keep the normal automatic
/// protocol selection behavior.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AutoTuneConfig {
    pub(crate) large_body_threshold: u64,
}

impl AutoTuneConfig {
    /// The default size at which known-length request bodies use HTTP/1.1.
    pub const DEFAULT_LARGE_BODY_THRESHOLD: u64 = 16 * 1024 * 1024;

    /// Create the default automatic transport selection configuration.
    pub const fn new() -> Self {
        Self {
            large_body_threshold: Self::DEFAULT_LARGE_BODY_THRESHOLD,
        }
    }

    /// Set the minimum known body size that selects HTTP/1.1.
    ///
    /// A threshold of zero selects HTTP/1.1 for every request whose exact body
    /// size is available. Unknown-length streaming bodies remain automatic.
    pub const fn large_body_threshold(mut self, threshold: u64) -> Self {
        self.large_body_threshold = threshold;
        self
    }
}

impl Default for AutoTuneConfig {
    fn default() -> Self {
        Self::new()
    }
}
