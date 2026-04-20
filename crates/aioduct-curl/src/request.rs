use aioduct::runtime::TokioRuntime;
use aioduct::{RequestBuilder, Response};
use http::{HeaderName, HeaderValue, Method};

use crate::cli::Cli;

pub async fn execute(
    cli: &Cli,
    client: &aioduct::Client<TokioRuntime>,
) -> Result<Response, aioduct::Error> {
    let method: Method = cli.effective_method().parse().map_err(|_| {
        aioduct::Error::InvalidUrl(format!("invalid method: {}", cli.effective_method()))
    })?;

    let mut req = client.request(method, &cli.url)?;
    req = apply_headers(cli, req);
    req = apply_auth(cli, req);
    req = apply_body(cli, req)?;

    if cli.verbose {
        eprint_request_info(cli, &req);
    }

    req.send().await
}

fn apply_headers<'a>(
    cli: &Cli,
    mut req: RequestBuilder<'a, TokioRuntime>,
) -> RequestBuilder<'a, TokioRuntime> {
    for h in &cli.headers {
        if let Some((name, value)) = h.split_once(':') {
            if let (Ok(n), Ok(v)) = (
                name.trim().parse::<HeaderName>(),
                value.trim().parse::<HeaderValue>(),
            ) {
                req = req.header(n, v);
            }
        }
    }

    if let Some(ref referer) = cli.referer {
        if let Ok(v) = referer.parse::<HeaderValue>() {
            req = req.header(http::header::REFERER, v);
        }
    }

    req
}

fn apply_auth<'a>(
    cli: &Cli,
    mut req: RequestBuilder<'a, TokioRuntime>,
) -> RequestBuilder<'a, TokioRuntime> {
    if let Some(ref user_str) = cli.user {
        let (user, pass) = match user_str.split_once(':') {
            Some((u, p)) => (u, Some(p)),
            None => (user_str.as_str(), None),
        };
        req = req.basic_auth(user, pass);
    }

    if let Some(ref token) = cli.oauth2_bearer {
        req = req.bearer_auth(token);
    }

    req
}

fn apply_body<'a>(
    cli: &Cli,
    mut req: RequestBuilder<'a, TokioRuntime>,
) -> Result<RequestBuilder<'a, TokioRuntime>, aioduct::Error> {
    if let Some(ref data) = cli.data {
        let body = if let Some(path) = data.strip_prefix('@') {
            std::fs::read(path).map_err(aioduct::Error::Io)?
        } else {
            data.as_bytes().to_vec()
        };
        req = req.body(body);
    } else if let Some(ref data) = cli.data_binary {
        let body = if let Some(path) = data.strip_prefix('@') {
            std::fs::read(path).map_err(aioduct::Error::Io)?
        } else {
            data.as_bytes().to_vec()
        };
        req = req.body(body);
    } else if !cli.form.is_empty() {
        let pairs: Vec<(&str, &str)> = cli.form.iter().filter_map(|f| f.split_once('=')).collect();
        req = req.form(&pairs);
    }

    Ok(req)
}

fn eprint_request_info(cli: &Cli, _req: &RequestBuilder<'_, TokioRuntime>) {
    let method = cli.effective_method();
    let url: http::Uri = match cli.url.parse() {
        Ok(u) => u,
        Err(_) => return,
    };
    let path = url.path_and_query().map(|pq| pq.as_str()).unwrap_or("/");
    let host = url.host().unwrap_or("");

    eprintln!("> {method} {path} HTTP/1.1");
    eprintln!("> Host: {host}");
    for h in &cli.headers {
        eprintln!("> {h}");
    }
    if let Some(ref ua) = cli.user_agent {
        eprintln!("> User-Agent: {ua}");
    }
    if cli.user.is_some() {
        eprintln!("> Authorization: Basic ***");
    }
    eprintln!(">");
}
