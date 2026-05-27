mod cli;
mod observer;
mod output;
mod request;
mod response_output;
mod verbose_plain;
mod verbose_tui;

pub use cli::HttpArgs;

use std::io::IsTerminal;
use std::process::ExitCode;

use crate::util::{is_binary_content_type, redact_header_value};
use response_output::{PlainOutput, ResponseOutput, TuiOutput};

pub async fn run(cli: HttpArgs) -> ExitCode {
    let (cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
    let (obs, tui_handle) = setup_observer(&cli, cancel_tx);

    let http_client = match cli.to_client(obs) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("aioduct http: {e}");
            return ExitCode::FAILURE;
        }
    };

    let resp = match request::execute(&cli, &http_client).await {
        Ok(r) => r,
        Err(e) => {
            if let Some(mut tui) = tui_handle {
                tui.send_fatal_error(e.to_string());
                tui.wait().await;
            } else if !cli.silent || cli.show_error {
                eprintln!("aioduct http: {e}");
            }
            return exit_code_for_error(&e);
        }
    };

    let info = ResponseInfo::from_response(&resp);

    // Dump headers to file
    if let Some(ref path) = cli.dump_header
        && let Err(e) = output::dump_headers_file(info.version, info.status, &info.headers, path)
    {
        if let Some(mut tui) = tui_handle {
            tui.send_fatal_error(e.to_string());
            tui.wait().await;
        } else if !cli.silent || cli.show_error {
            eprintln!("aioduct http: {e}");
        }
        return ExitCode::from(23);
    }

    // Create unified output adapter
    let mut output: Box<dyn ResponseOutput> = if let Some(tui) = tui_handle {
        tui.send_response_headers(info.headers.clone());
        Box::new(TuiOutput::new(
            tui,
            &cli,
            info.version,
            info.status,
            info.headers,
            info.is_binary,
            cancel_rx,
        ))
    } else {
        Box::new(PlainOutput::new(
            &cli,
            info.version,
            info.status,
            info.headers,
        ))
    };

    // Consume body
    if !cli.head
        && let Err(e) = output.consume_body(resp).await
    {
        if !cli.silent || cli.show_error {
            eprintln!("aioduct http: {e}");
        }
        output.abort().await;
        return ExitCode::from(23);
    }

    output.finish().await
}

struct ResponseInfo {
    status: http::StatusCode,
    version: http::Version,
    headers: Vec<(String, String)>,
    is_binary: bool,
}

impl ResponseInfo {
    fn from_response(resp: &aioduct::Response) -> Self {
        let headers: Vec<(String, String)> = resp
            .headers()
            .iter()
            .map(|(k, v)| {
                let value = v.to_str().unwrap_or("<binary>").to_owned();
                let display_value = redact_header_value(&value, k.as_str());
                (k.as_str().to_owned(), display_value.to_owned())
            })
            .collect();
        let is_binary = is_binary_content_type(&headers);
        Self {
            status: resp.status(),
            version: resp.version(),
            headers,
            is_binary,
        }
    }
}

fn setup_observer(
    cli: &HttpArgs,
    cancel_tx: tokio::sync::watch::Sender<bool>,
) -> (
    Option<observer::CliObserver>,
    Option<verbose_tui::VerboseTui>,
) {
    if !cli.verbose && !cli.verbose_plain {
        return (None, None);
    }
    let (obs, rx) = observer::CliObserver::new();
    let handle = if cli.verbose_plain || !std::io::stdout().is_terminal() {
        tokio::spawn(verbose_plain::run(rx));
        None
    } else {
        Some(verbose_tui::VerboseTui::start(rx, cancel_tx))
    };
    (Some(obs), handle)
}

fn exit_code_for_error(e: &aioduct::Error) -> ExitCode {
    match e {
        aioduct::Error::Timeout => ExitCode::from(28),
        aioduct::Error::Io(_) => ExitCode::from(7),
        aioduct::Error::Tls(_) => ExitCode::from(60),
        aioduct::Error::InvalidUrl(_) => ExitCode::from(3),
        _ => ExitCode::from(1),
    }
}
