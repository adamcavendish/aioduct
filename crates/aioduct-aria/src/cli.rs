use std::path::PathBuf;
use std::time::Duration;

use clap::Parser;

#[derive(Parser, Debug)]
#[command(
    name = "aioduct-aria",
    about = "Aria2-inspired parallel download tool built on aioduct",
    version,
    after_help = "Examples:\n  \
        aioduct-aria https://example.com/file.iso\n  \
        aioduct-aria -s 8 -o output.bin https://example.com/large.dat\n  \
        aioduct-aria -j 3 -i urls.txt\n  \
        aioduct-aria -c https://example.com/file.iso   # resume download"
)]
pub struct Cli {
    /// Download URIs
    #[arg(value_name = "URI")]
    pub uris: Vec<String>,

    /// Output directory
    #[arg(short = 'd', long, default_value = ".")]
    pub dir: PathBuf,

    /// Output filename (only valid for single URI)
    #[arg(short = 'o', long)]
    pub out: Option<String>,

    /// Number of parallel connections per download
    #[arg(short = 's', long, default_value_t = 4)]
    pub split: usize,

    /// Maximum connections per server
    #[arg(short = 'x', long, default_value_t = 4)]
    pub max_connection_per_server: usize,

    /// Maximum concurrent downloads
    #[arg(short = 'j', long, default_value_t = 5)]
    pub max_concurrent_downloads: usize,

    /// Minimum split size (e.g. 1M, 20M)
    #[arg(short = 'k', long, default_value = "1M", value_parser = parse_size)]
    pub min_split_size: u64,

    /// Continue/resume a partially downloaded file
    #[arg(short = 'c', long = "continue")]
    pub continue_download: bool,

    /// Timeout in seconds
    #[arg(short = 't', long, default_value_t = 60)]
    pub timeout: u64,

    /// Connection timeout in seconds
    #[arg(long, default_value_t = 30)]
    pub connect_timeout: u64,

    /// Max retry attempts (0 = no retry)
    #[arg(short = 'm', long, default_value_t = 5)]
    pub max_tries: u32,

    /// Seconds to wait between retries
    #[arg(long, default_value_t = 1)]
    pub retry_wait: u64,

    /// Overall download speed limit (e.g. 1M, 500K)
    #[arg(long, value_parser = parse_size)]
    pub max_overall_download_limit: Option<u64>,

    /// Per-download speed limit
    #[arg(long, value_parser = parse_size)]
    pub max_download_limit: Option<u64>,

    /// Additional HTTP headers (repeatable)
    #[arg(long = "header", short = 'H', action = clap::ArgAction::Append)]
    pub headers: Vec<String>,

    /// Referer URI
    #[arg(long)]
    pub referer: Option<String>,

    /// User agent string
    #[arg(short = 'U', long)]
    pub user_agent: Option<String>,

    /// HTTP basic auth username
    #[arg(long)]
    pub http_user: Option<String>,

    /// HTTP basic auth password
    #[arg(long)]
    pub http_passwd: Option<String>,

    /// Proxy for all protocols (http://host:port or socks5://host:port)
    #[arg(long)]
    pub all_proxy: Option<String>,

    /// Suppress console output
    #[arg(short = 'q', long)]
    pub quiet: bool,

    /// Log file path (- for stdout)
    #[arg(short = 'l', long)]
    pub log: Option<PathBuf>,

    /// Log level
    #[arg(long, default_value = "warn")]
    pub log_level: String,

    /// Read URIs from file (one per line)
    #[arg(short = 'i', long)]
    pub input_file: Option<PathBuf>,

    /// File allocation method
    #[arg(long, default_value = "prealloc", value_parser = ["none", "prealloc", "falloc"])]
    pub file_allocation: String,

    /// Show sizes in human-readable format
    #[arg(long, default_value_t = true)]
    pub human_readable: bool,

    /// Disable certificate verification
    #[arg(long)]
    pub check_certificate_false: bool,

    /// Dry run — resolve URIs and show file info without downloading
    #[arg(long)]
    pub dry_run: bool,

    /// Checksum verification (TYPE=DIGEST, e.g. sha-256=abcdef...)
    #[arg(long)]
    pub checksum: Option<String>,

    /// Auto file renaming if file already exists
    #[arg(long, default_value_t = true)]
    pub auto_file_renaming: bool,

    /// Allow overwriting existing files
    #[arg(long)]
    pub allow_overwrite: bool,
}

impl Cli {
    pub fn timeout_duration(&self) -> Duration {
        Duration::from_secs(self.timeout)
    }

    pub fn connect_timeout_duration(&self) -> Duration {
        Duration::from_secs(self.connect_timeout)
    }

    pub fn retry_wait_duration(&self) -> Duration {
        Duration::from_secs(self.retry_wait)
    }

    pub fn all_uris(&self) -> Result<Vec<String>, std::io::Error> {
        let mut uris = self.uris.clone();
        if let Some(ref path) = self.input_file {
            let content = std::fs::read_to_string(path)?;
            for line in content.lines() {
                let line = line.trim();
                if !line.is_empty() && !line.starts_with('#') {
                    uris.push(line.to_string());
                }
            }
        }
        Ok(uris)
    }
}

fn parse_size(s: &str) -> Result<u64, String> {
    let s = s.trim();
    if s.is_empty() {
        return Err("empty size".to_string());
    }

    let (num_str, multiplier) = if s.ends_with('K') || s.ends_with('k') {
        (&s[..s.len() - 1], 1024u64)
    } else if s.ends_with('M') || s.ends_with('m') {
        (&s[..s.len() - 1], 1024 * 1024)
    } else if s.ends_with('G') || s.ends_with('g') {
        (&s[..s.len() - 1], 1024 * 1024 * 1024)
    } else {
        (s, 1u64)
    };

    let num: f64 = num_str
        .parse()
        .map_err(|e| format!("invalid size '{s}': {e}"))?;
    Ok((num * multiplier as f64) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_size() {
        assert_eq!(parse_size("1M").unwrap(), 1024 * 1024);
        assert_eq!(parse_size("20M").unwrap(), 20 * 1024 * 1024);
        assert_eq!(parse_size("500K").unwrap(), 500 * 1024);
        assert_eq!(parse_size("1G").unwrap(), 1024 * 1024 * 1024);
        assert_eq!(parse_size("1024").unwrap(), 1024);
        assert_eq!(parse_size("1.5M").unwrap(), (1.5 * 1024.0 * 1024.0) as u64);
    }
}
