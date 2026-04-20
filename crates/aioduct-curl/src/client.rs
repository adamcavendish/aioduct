use std::time::Duration;

use aioduct::runtime::TokioRuntime;
use aioduct::{Client, RetryConfig};

use crate::cli::Cli;

pub fn build_client(cli: &Cli) -> Client<TokioRuntime> {
    let mut builder = Client::<TokioRuntime>::builder();

    if let Some(ref ua) = cli.user_agent {
        builder = builder.user_agent(ua);
    }

    if cli.location {
        builder = builder.max_redirects(cli.max_redirs);
    } else {
        builder = builder.redirect_policy(aioduct::RedirectPolicy::none());
    }

    if let Some(timeout) = cli.connect_timeout_duration() {
        builder = builder.connect_timeout(timeout);
    }

    if let Some(timeout) = cli.max_time_duration() {
        builder = builder.timeout(timeout);
    }

    if let Some(count) = cli.retry {
        builder = builder.retry(
            RetryConfig::default()
                .max_retries(count)
                .max_backoff(Duration::from_secs(cli.retry_max_time)),
        );
    }

    if cli.insecure {
        builder = builder.danger_accept_invalid_certs();
    }

    if cli.http2 {
        builder = builder.http2_prior_knowledge();
    }

    if let Some(rate) = cli.limit_rate {
        builder = builder.max_download_speed(rate);
    }

    if cli.raw {
        builder = builder.no_decompression();
    }

    if let Some(ref proxy_url) = cli.proxy {
        if let Ok(proxy) = aioduct::ProxyConfig::http(proxy_url)
            .or_else(|_| aioduct::ProxyConfig::socks5(proxy_url))
        {
            builder = builder.proxy(proxy);
        }
    }

    builder.build()
}
