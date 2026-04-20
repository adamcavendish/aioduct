use std::path::PathBuf;
use std::time::Duration;

use clap::Parser;

#[derive(Parser, Debug)]
#[command(
    name = "aioduct-curl",
    about = "Curl-inspired HTTP tool built on aioduct",
    version,
    after_help = "Examples:\n  \
        aioduct-curl https://httpbin.org/get\n  \
        aioduct-curl -X POST -d '{\"key\":\"val\"}' -H 'Content-Type: application/json' https://httpbin.org/post\n  \
        aioduct-curl -I https://example.com\n  \
        aioduct-curl -o output.html https://example.com\n  \
        aioduct-curl -u user:pass https://httpbin.org/basic-auth/user/pass\n  \
        aioduct-curl -L https://httpbin.org/redirect/3"
)]
pub struct Cli {
    /// URL to request
    #[arg(value_name = "URL")]
    pub url: String,

    /// HTTP method (default: GET, or POST if -d is used)
    #[arg(short = 'X', long = "request")]
    pub method: Option<String>,

    /// Request body data (sets method to POST if not specified)
    #[arg(short = 'd', long = "data")]
    pub data: Option<String>,

    /// Read request body from file (use @filename)
    #[arg(long = "data-binary")]
    pub data_binary: Option<String>,

    /// Send data as URL-encoded form
    #[arg(short = 'F', long = "form", action = clap::ArgAction::Append)]
    pub form: Vec<String>,

    /// Extra headers (repeatable)
    #[arg(short = 'H', long = "header", action = clap::ArgAction::Append)]
    pub headers: Vec<String>,

    /// User-Agent string
    #[arg(short = 'A', long = "user-agent")]
    pub user_agent: Option<String>,

    /// Referer URL
    #[arg(short = 'e', long = "referer")]
    pub referer: Option<String>,

    /// Basic auth (user:password)
    #[arg(short = 'u', long = "user")]
    pub user: Option<String>,

    /// Bearer token
    #[arg(long)]
    pub oauth2_bearer: Option<String>,

    /// Follow redirects
    #[arg(short = 'L', long = "location")]
    pub location: bool,

    /// Max redirects (default: 10)
    #[arg(long = "max-redirs", default_value_t = 10)]
    pub max_redirs: usize,

    /// Show response headers only (HEAD request)
    #[arg(short = 'I', long = "head")]
    pub head: bool,

    /// Include response headers in output
    #[arg(short = 'i', long = "include")]
    pub include: bool,

    /// Verbose output (show request and response headers)
    #[arg(short = 'v', long = "verbose")]
    pub verbose: bool,

    /// Silent mode
    #[arg(short = 's', long = "silent")]
    pub silent: bool,

    /// Show only errors (with -s)
    #[arg(short = 'S', long = "show-error")]
    pub show_error: bool,

    /// Write output to file
    #[arg(short = 'o', long = "output")]
    pub output: Option<PathBuf>,

    /// Write output to file named from URL
    #[arg(short = 'O', long = "remote-name")]
    pub remote_name: bool,

    /// Dump headers to file
    #[arg(short = 'D', long = "dump-header")]
    pub dump_header: Option<PathBuf>,

    /// Write just the HTTP status code to stdout
    #[arg(short = 'w', long = "write-out")]
    pub write_out: Option<String>,

    /// Connection timeout in seconds
    #[arg(long = "connect-timeout")]
    pub connect_timeout: Option<f64>,

    /// Max time for entire request in seconds
    #[arg(short = 'm', long = "max-time")]
    pub max_time: Option<f64>,

    /// Retry count
    #[arg(long)]
    pub retry: Option<u32>,

    /// Max retry delay in seconds
    #[arg(long = "retry-max-time", default_value_t = 60)]
    pub retry_max_time: u64,

    /// Proxy URL
    #[arg(short = 'x', long = "proxy")]
    pub proxy: Option<String>,

    /// Disable certificate verification
    #[arg(short = 'k', long = "insecure")]
    pub insecure: bool,

    /// Force HTTP/2
    #[arg(long = "http2")]
    pub http2: bool,

    /// Max download speed (bytes/sec, supports K/M/G suffix)
    #[arg(long = "limit-rate", value_parser = parse_rate)]
    pub limit_rate: Option<u64>,

    /// Disable decompression
    #[arg(long = "raw")]
    pub raw: bool,

    /// Compressed (Accept-Encoding: gzip, deflate, br)
    #[arg(long)]
    pub compressed: bool,
}

impl Cli {
    pub fn effective_method(&self) -> &str {
        if let Some(ref m) = self.method {
            m.as_str()
        } else if self.head {
            "HEAD"
        } else if self.data.is_some() || self.data_binary.is_some() || !self.form.is_empty() {
            "POST"
        } else {
            "GET"
        }
    }

    pub fn connect_timeout_duration(&self) -> Option<Duration> {
        self.connect_timeout.map(Duration::from_secs_f64)
    }

    pub fn max_time_duration(&self) -> Option<Duration> {
        self.max_time.map(Duration::from_secs_f64)
    }
}

fn parse_rate(s: &str) -> Result<u64, String> {
    let s = s.trim();
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
        .map_err(|e| format!("invalid rate '{s}': {e}"))?;
    Ok((num * multiplier as f64) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_rate_values() {
        assert_eq!(parse_rate("100K").unwrap(), 100 * 1024);
        assert_eq!(parse_rate("1M").unwrap(), 1024 * 1024);
        assert_eq!(parse_rate("1024").unwrap(), 1024);
    }
}
