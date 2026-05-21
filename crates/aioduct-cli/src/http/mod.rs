mod cli;
mod client;
mod observer;
mod output;
mod request;
mod verbose_plain;
mod verbose_tui;

pub use cli::HttpArgs;

use std::io::IsTerminal;
use std::process::ExitCode;

pub async fn run(cli: HttpArgs) -> ExitCode {
    let use_verbose = cli.verbose || cli.verbose_plain;

    let (obs, tui_handle) = if use_verbose {
        let (obs, rx) = observer::CliObserver::new();
        let handle = if cli.verbose_plain || !std::io::stdout().is_terminal() {
            tokio::spawn(verbose_plain::run(rx));
            None
        } else {
            Some(verbose_tui::VerboseTui::start(rx))
        };
        (Some(obs), handle)
    } else {
        (None, None)
    };

    let http_client = match client::build_client(&cli, obs) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("aioduct http: {e}");
            return ExitCode::FAILURE;
        }
    };

    let resp = match request::execute(&cli, &http_client).await {
        Ok(r) => r,
        Err(e) => {
            if !cli.silent || cli.show_error {
                eprintln!("aioduct http: {e}");
            }
            if let Some(tui) = tui_handle {
                tui.stop().await;
            }
            return exit_code_for_error(&e);
        }
    };

    let status = resp.status();

    if let Some(ref tui) = tui_handle {
        let headers: Vec<(String, String)> = resp
            .headers()
            .iter()
            .map(|(k, v)| {
                (
                    k.as_str().to_owned(),
                    v.to_str().unwrap_or("<binary>").to_owned(),
                )
            })
            .collect();
        tui.send_response_headers(headers);
    }

    if let Err(e) = output::handle(&cli, resp, tui_handle.is_some()).await {
        if !cli.silent || cli.show_error {
            eprintln!("aioduct http: {e}");
        }
        if let Some(tui) = tui_handle {
            tui.stop().await;
        }
        return ExitCode::from(23);
    }

    if let Some(tui) = tui_handle {
        tui.wait().await;
    }

    if status.is_client_error() || status.is_server_error() {
        ExitCode::from(22)
    } else {
        ExitCode::SUCCESS
    }
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
