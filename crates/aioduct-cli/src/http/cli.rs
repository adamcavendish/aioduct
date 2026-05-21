use std::path::PathBuf;
use std::time::Duration;

use clap::Args;

use crate::common::parse_byte_size;

#[derive(Args, Debug)]
#[command(
    about = "Curl-inspired HTTP tool built on aioduct",
    after_help = "Examples:\n  \
        aioduct http https://httpbin.org/get\n  \
        aioduct http -X POST -d '{\"key\":\"val\"}' -H 'Content-Type: application/json' https://httpbin.org/post\n  \
        aioduct http -I https://example.com\n  \
        aioduct http -o output.html https://example.com\n  \
        aioduct http -u user:pass https://httpbin.org/basic-auth/user/pass\n  \
        aioduct http -L https://httpbin.org/redirect/3"
)]
pub struct HttpArgs {
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
    #[arg(long = "limit-rate", value_parser = parse_byte_size)]
    pub limit_rate: Option<u64>,

    /// Disable decompression
    #[arg(long = "raw")]
    pub raw: bool,

    /// Compressed (Accept-Encoding: gzip, deflate, br)
    #[arg(long)]
    pub compressed: bool,

    /// Verbose plain text output (no TUI, colored stderr log)
    #[arg(long = "verbose-plain")]
    pub verbose_plain: bool,
}

impl HttpArgs {
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

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[derive(Parser)]
    struct TestCli {
        #[command(flatten)]
        inner: HttpArgs,
    }

    fn parse(args: &[&str]) -> HttpArgs {
        TestCli::parse_from(args).inner
    }

    #[test]
    fn effective_method_default_is_get() {
        let cli = parse(&["test", "http://example.com"]);
        assert_eq!(cli.effective_method(), "GET");
    }

    #[test]
    fn effective_method_explicit_overrides() {
        let cli = parse(&["test", "-X", "PUT", "http://example.com"]);
        assert_eq!(cli.effective_method(), "PUT");
    }

    #[test]
    fn effective_method_head_flag() {
        let cli = parse(&["test", "-I", "http://example.com"]);
        assert_eq!(cli.effective_method(), "HEAD");
    }

    #[test]
    fn effective_method_data_implies_post() {
        let cli = parse(&["test", "-d", "payload", "http://example.com"]);
        assert_eq!(cli.effective_method(), "POST");
    }

    #[test]
    fn effective_method_explicit_plus_data() {
        let cli = parse(&["test", "-X", "PUT", "-d", "payload", "http://example.com"]);
        assert_eq!(cli.effective_method(), "PUT");
    }

    #[test]
    fn effective_method_form_implies_post() {
        let cli = parse(&["test", "-F", "file=@data.txt", "http://example.com"]);
        assert_eq!(cli.effective_method(), "POST");
    }

    #[test]
    fn effective_method_data_binary_implies_post() {
        let cli = parse(&["test", "--data-binary", "@file.bin", "http://example.com"]);
        assert_eq!(cli.effective_method(), "POST");
    }
}
