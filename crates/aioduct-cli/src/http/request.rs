use aioduct::runtime::TokioRuntime;
use aioduct::runtime::tokio_rt::TcpConnector;
use aioduct::{RequestBuilderSend, Response};
use http::{HeaderName, HeaderValue, Method};

use super::cli::HttpArgs;

pub async fn execute(
    cli: &HttpArgs,
    client: &aioduct::TokioClient,
) -> Result<Response, aioduct::Error> {
    let method: Method = cli.effective_method().parse().map_err(|_| {
        aioduct::Error::InvalidUrl(format!("invalid method: {}", cli.effective_method()))
    })?;

    let mut req = client.request(method, &cli.url)?;
    req = apply_headers(cli, req);
    req = apply_auth(cli, req);
    req = apply_body(cli, req)?;

    Ok(req.send().await?)
}

fn apply_headers<'a>(
    cli: &HttpArgs,
    mut req: RequestBuilderSend<'a, TokioRuntime, TcpConnector>,
) -> RequestBuilderSend<'a, TokioRuntime, TcpConnector> {
    if cli.compressed {
        req = req.header(
            http::header::ACCEPT_ENCODING,
            HeaderValue::from_static("gzip, deflate, br, zstd"),
        );
    }

    for h in &cli.headers {
        if let Some((name, value)) = h.split_once(':')
            && let (Ok(n), Ok(v)) = (
                name.trim().parse::<HeaderName>(),
                value.trim().parse::<HeaderValue>(),
            )
        {
            req = req.header(n, v);
        }
    }

    if let Some(ref referer) = cli.referer
        && let Ok(v) = referer.parse::<HeaderValue>()
    {
        req = req.header(http::header::REFERER, v);
    }

    req
}

fn apply_auth<'a>(
    cli: &HttpArgs,
    mut req: RequestBuilderSend<'a, TokioRuntime, TcpConnector>,
) -> RequestBuilderSend<'a, TokioRuntime, TcpConnector> {
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
    cli: &HttpArgs,
    mut req: RequestBuilderSend<'a, TokioRuntime, TcpConnector>,
) -> Result<RequestBuilderSend<'a, TokioRuntime, TcpConnector>, aioduct::Error> {
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
