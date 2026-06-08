use std::path::PathBuf;
use std::time::Duration;

use aioduct::observer::RequestObserver;
use aioduct::{NoProxy, ProxyChain, ProxySettings, RedirectPolicy, RetryConfig, TokioClient};
use clap::Args;

use crate::common::parse_byte_size;
use crate::util::parse_proxy_url;

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

    /// Proxy URL (repeatable for multi-hop chaining)
    #[arg(short = 'x', long = "proxy", action = clap::ArgAction::Append)]
    pub proxy: Vec<String>,

    /// Proxy authentication (user:password)
    #[arg(long = "proxy-user")]
    pub proxy_user: Option<String>,

    /// Hosts to bypass proxy for (comma-separated)
    #[arg(long = "noproxy")]
    pub noproxy: Option<String>,

    /// Use proxy settings from environment variables (HTTP_PROXY, HTTPS_PROXY, NO_PROXY)
    #[arg(long = "system-proxy")]
    pub system_proxy: bool,

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

    pub fn to_client(
        &self,
        observer: Option<impl RequestObserver>,
    ) -> Result<TokioClient, aioduct::Error> {
        let mut builder = TokioClient::builder();

        if let Some(obs) = observer {
            builder = builder.request_observer(obs);
        }

        if let Some(ref ua) = self.user_agent {
            builder = builder.user_agent(ua);
        }

        if self.location {
            builder = builder.max_redirects(self.max_redirs);
        } else {
            builder = builder.redirect_policy(RedirectPolicy::none());
        }

        if let Some(timeout) = self.connect_timeout_duration() {
            builder = builder.connect_timeout(timeout);
        }

        if let Some(timeout) = self.max_time_duration() {
            builder = builder.timeout(timeout);
        }

        if let Some(count) = self.retry {
            builder = builder.retry(
                RetryConfig::default()
                    .max_retries(count)
                    .max_backoff(Duration::from_secs(self.retry_max_time)),
            );
        }

        if self.insecure {
            builder = builder.danger_accept_invalid_certs();
        }

        if let Some(rate) = self.limit_rate {
            builder = builder.max_download_speed(rate);
        }

        if self.raw {
            builder = builder.no_decompression();
        }

        if !self.proxy.is_empty() {
            let configs: Vec<_> = self
                .proxy
                .iter()
                .filter_map(|url| {
                    let mut cfg = parse_proxy_url(url)?;
                    if let Some(ref user) = self.proxy_user
                        && let Some((u, p)) = user.split_once(':')
                    {
                        cfg = cfg.basic_auth(u, p);
                    }
                    Some(cfg)
                })
                .collect();
            if configs.len() > 1 {
                builder = builder.proxy_chain(ProxyChain::new(configs));
            } else if let Some(cfg) = configs.into_iter().next() {
                if let Some(ref noproxy) = self.noproxy {
                    builder = builder
                        .proxy_settings(ProxySettings::all(cfg).no_proxy(NoProxy::new(noproxy)));
                } else {
                    builder = builder.proxy(cfg);
                }
            }
        } else if self.system_proxy {
            let mut settings = ProxySettings::from_env();
            if let Some(ref noproxy) = self.noproxy {
                settings = settings.no_proxy(NoProxy::new(noproxy));
            }
            builder = builder.proxy_settings(settings);
        }

        builder.build()
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

    // ── Proxy CLI flag tests ──

    #[test]
    fn proxy_single_flag() {
        let cli = parse(&["test", "-x", "http://proxy:8080", "http://example.com"]);
        assert_eq!(cli.proxy.len(), 1);
        assert_eq!(cli.proxy[0], "http://proxy:8080");
    }

    #[test]
    fn proxy_long_flag() {
        let cli = parse(&[
            "test",
            "--proxy",
            "socks5://proxy:1080",
            "http://example.com",
        ]);
        assert_eq!(cli.proxy.len(), 1);
        assert_eq!(cli.proxy[0], "socks5://proxy:1080");
    }

    #[test]
    fn proxy_repeated_flags() {
        let cli = parse(&[
            "test",
            "-x",
            "http://p1:8080",
            "-x",
            "socks5://p2:1080",
            "http://example.com",
        ]);
        assert_eq!(cli.proxy.len(), 2);
        assert_eq!(cli.proxy[0], "http://p1:8080");
        assert_eq!(cli.proxy[1], "socks5://p2:1080");
    }

    #[test]
    fn proxy_no_proxy_default() {
        let cli = parse(&["test", "http://example.com"]);
        assert!(cli.proxy.is_empty());
        assert!(cli.proxy_user.is_none());
        assert!(cli.noproxy.is_none());
        assert!(!cli.system_proxy);
    }

    #[test]
    fn proxy_user_flag() {
        let cli = parse(&[
            "test",
            "-x",
            "http://proxy:8080",
            "--proxy-user",
            "admin:secret",
            "http://example.com",
        ]);
        assert_eq!(cli.proxy_user.as_deref(), Some("admin:secret"));
    }

    #[test]
    fn noproxy_flag() {
        let cli = parse(&[
            "test",
            "--noproxy",
            "localhost,127.0.0.1,.internal",
            "http://example.com",
        ]);
        assert_eq!(
            cli.noproxy.as_deref(),
            Some("localhost,127.0.0.1,.internal")
        );
    }

    #[test]
    fn system_proxy_flag() {
        let cli = parse(&["test", "--system-proxy", "http://example.com"]);
        assert!(cli.system_proxy);
    }

    #[test]
    fn proxy_user_without_proxy_does_not_crash() {
        let cli = parse(&["test", "--proxy-user", "u:p", "http://example.com"]);
        assert!(cli.proxy.is_empty());
        assert_eq!(cli.proxy_user.as_deref(), Some("u:p"));
    }

    // ── to_client proxy tests ──

    use aioduct::observer::RequestObserver;

    struct NoopObserver;
    impl RequestObserver for NoopObserver {
        fn on_event(&self, _event: &aioduct::observer::RequestEvent) {}
        fn on_connection_event(&self, _event: &aioduct::observer::ConnectionEvent) {}
    }

    #[test]
    fn to_client_single_proxy() {
        let cli = parse(&["test", "-x", "http://proxy:8080", "http://example.com"]);
        let client = cli.to_client(Some(NoopObserver));
        assert!(client.is_ok());
    }

    #[test]
    fn to_client_proxy_chain_two_hops() {
        let cli = parse(&[
            "test",
            "-x",
            "http://p1:8080",
            "-x",
            "socks5://p2:1080",
            "http://example.com",
        ]);
        let client = cli.to_client(Some(NoopObserver));
        assert!(client.is_ok());
    }

    #[test]
    fn to_client_proxy_with_user() {
        let cli = parse(&[
            "test",
            "-x",
            "http://proxy:8080",
            "--proxy-user",
            "admin:secret",
            "http://example.com",
        ]);
        let client = cli.to_client(Some(NoopObserver));
        assert!(client.is_ok());
    }

    #[test]
    fn to_client_proxy_with_noproxy() {
        let cli = parse(&[
            "test",
            "-x",
            "http://proxy:8080",
            "--noproxy",
            "localhost,127.0.0.1",
            "http://example.com",
        ]);
        let client = cli.to_client(Some(NoopObserver));
        assert!(client.is_ok());
    }

    #[test]
    fn to_client_system_proxy() {
        let cli = parse(&["test", "--system-proxy", "http://example.com"]);
        let client = cli.to_client(Some(NoopObserver));
        assert!(client.is_ok());
    }

    #[test]
    fn to_client_system_proxy_with_noproxy() {
        let cli = parse(&[
            "test",
            "--system-proxy",
            "--noproxy",
            "localhost",
            "http://example.com",
        ]);
        let client = cli.to_client(Some(NoopObserver));
        assert!(client.is_ok());
    }

    #[test]
    fn to_client_explicit_proxy_overrides_system() {
        let cli = parse(&[
            "test",
            "-x",
            "http://explicit:8080",
            "--system-proxy",
            "http://example.com",
        ]);
        // -x takes priority over --system-proxy, so proxy should have 1 entry
        assert_eq!(cli.proxy.len(), 1);
        let client = cli.to_client(Some(NoopObserver));
        assert!(client.is_ok());
    }

    #[test]
    fn to_client_proxy_user_applied_to_all_chain_hops() {
        let cli = parse(&[
            "test",
            "-x",
            "http://p1:8080",
            "-x",
            "socks5://p2:1080",
            "--proxy-user",
            "user:pass",
            "http://example.com",
        ]);
        let client = cli.to_client(Some(NoopObserver));
        assert!(client.is_ok());
    }

    #[test]
    fn to_client_no_observer() {
        let cli = parse(&["test", "http://example.com"]);
        let client = cli.to_client(None::<NoopObserver>);
        assert!(client.is_ok());
    }
}
