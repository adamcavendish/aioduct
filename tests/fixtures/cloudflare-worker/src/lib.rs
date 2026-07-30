use std::time::Duration;

use aioduct::{Error as AioductError, WasmClient};
use worker::{Context, Env, Fetch, Request, Response, Result, Url, event};

const STATUS_URL: &str = "http://127.0.0.1:9877/status/204";
const DELAY_URL: &str = "http://127.0.0.1:9877/delay/500";

#[event(fetch)]
async fn fetch(request: Request, _env: Env, _context: Context) -> Result<Response> {
    match request.path().as_str() {
        "/control" => control().await,
        "/aioduct" => aioduct_status(None).await,
        "/aioduct-timed-fast" => aioduct_status(Some(Duration::from_secs(5))).await,
        "/aioduct-timeout" => aioduct_timeout().await,
        "/" => Response::ok("aioduct Cloudflare Worker fixture"),
        _ => Response::error("not found", 404),
    }
}

async fn control() -> Result<Response> {
    let url = Url::parse(STATUS_URL)
        .map_err(|error| worker::Error::RustError(format!("control URL: {error}")))?;
    let response = Fetch::Url(url).send().await?;
    Response::ok(response.status_code().to_string())
}

async fn aioduct_status(timeout: Option<Duration>) -> Result<Response> {
    let client = match timeout {
        Some(duration) => WasmClient::builder()
            .timeout(duration)
            .build()
            .map_err(|error| worker::Error::RustError(format!("client build: {error}")))?,
        None => WasmClient::new(),
    };

    let request = match client.get(STATUS_URL) {
        Ok(request) => request,
        Err(error) => return Response::error(format!("request build: {error}"), 500),
    };

    match request.send().await {
        Ok(response) => Response::ok(response.status().as_u16().to_string()),
        Err(error) => Response::error(format!("upstream fetch: {error}"), 500),
    }
}

async fn aioduct_timeout() -> Result<Response> {
    let client = WasmClient::builder()
        .timeout(Duration::from_millis(50))
        .build()
        .map_err(|error| worker::Error::RustError(format!("client build: {error}")))?;
    let request = match client.get(DELAY_URL) {
        Ok(request) => request,
        Err(error) => return Response::error(format!("request build: {error}"), 500),
    };

    match request.send().await {
        Err(AioductError::Timeout) => Response::ok("timeout"),
        Err(error) => Response::error(format!("unexpected fetch error: {error}"), 500),
        Ok(response) => Response::error(
            format!("expected timeout, got {}", response.status().as_u16()),
            500,
        ),
    }
}
