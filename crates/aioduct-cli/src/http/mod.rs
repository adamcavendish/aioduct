mod cli;
mod client;
mod observer;
mod output;
mod request;
mod verbose_plain;
mod verbose_tui;

pub use cli::HttpArgs;

use std::io::{IsTerminal, Write};
use std::path::Path;
use std::process::ExitCode;

pub async fn run(cli: HttpArgs) -> ExitCode {
    let use_verbose = cli.verbose || cli.verbose_plain;

    let (cancel_tx, mut cancel_rx) = tokio::sync::watch::channel(false);

    let (obs, tui_handle) = if use_verbose {
        let (obs, rx) = observer::CliObserver::new();
        let handle = if cli.verbose_plain || !std::io::stdout().is_terminal() {
            tokio::spawn(verbose_plain::run(rx));
            None
        } else {
            Some(verbose_tui::VerboseTui::start(rx, cancel_tx))
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
    let resp_version = resp.version();

    let resp_headers: Vec<(String, String)> = resp
        .headers()
        .iter()
        .map(|(k, v)| {
            (
                k.as_str().to_owned(),
                v.to_str().unwrap_or("<binary>").to_owned(),
            )
        })
        .collect();

    if let Some(ref tui) = tui_handle {
        tui.send_response_headers(resp_headers.clone());
    }

    if let Some(tui) = tui_handle {
        // --- TUI output path ---

        if let Some(ref path) = cli.dump_header
            && let Err(e) = output::dump_headers_file(resp_version, status, &resp_headers, path)
        {
            if !cli.silent || cli.show_error {
                eprintln!("aioduct http: {e}");
            }
            tui.stop().await;
            return ExitCode::from(23);
        }

        let mut body_buf: Vec<u8> = Vec::new();

        if !cli.head {
            let mut stream = resp.into_bytes_stream();
            loop {
                tokio::select! {
                    biased;
                    _ = cancel_rx.changed() => {
                        break;
                    }
                    chunk = stream.next() => {
                        match chunk {
                            Some(Ok(data)) => {
                                body_buf.extend_from_slice(&data);
                                let text = String::from_utf8_lossy(&data);
                                tui.send_body_chunk(text.into_owned());
                            }
                            Some(Err(e)) => {
                                if !cli.silent || cli.show_error {
                                    eprintln!("aioduct http: {e}");
                                }
                                tui.stop().await;
                                return ExitCode::from(23);
                            }
                            None => break,
                        }
                    }
                }
            }
            tui.send_body_done();
        }

        // Handle --remote-name / --output
        if cli.remote_name {
            let filename = output::filename_from_url(&cli.url);
            if let Err(e) = std::fs::write(Path::new(&filename), &body_buf) {
                if !cli.silent || cli.show_error {
                    eprintln!("aioduct http: failed to write {filename}: {e}");
                }
                tui.stop().await;
                return ExitCode::from(23);
            }
            if !cli.silent {
                eprintln!("Saved to {filename}");
            }
        } else if let Some(ref path) = cli.output
            && let Err(e) = std::fs::write(path, &body_buf)
        {
            if !cli.silent || cli.show_error {
                eprintln!("aioduct http: failed to write {}: {e}", path.display());
            }
            tui.stop().await;
            return ExitCode::from(23);
        }

        // Wait for user to press 'q'
        tui.wait().await;

        // Flush to stdout after TUI exits
        let write_body = !cli.head && !cli.remote_name && cli.output.is_none();

        if cli.head || cli.include {
            output::write_headers_stdout(resp_version, status, &resp_headers);
        }

        if write_body {
            let stdout = std::io::stdout();
            let mut out = stdout.lock();
            if let Err(e) = out.write_all(&body_buf) {
                if !cli.silent || cli.show_error {
                    eprintln!("aioduct http: {e}");
                }
                return ExitCode::from(23);
            }
            if std::io::stdout().is_terminal() && !body_buf.ends_with(b"\n") {
                let _ = out.write_all(b"\n");
            }
        }

        if let Some(ref fmt) = cli.write_out {
            output::print_write_out(fmt, status);
        }
    } else {
        // --- Non-TUI output path ---
        if let Err(e) = output::handle(&cli, resp, false).await {
            if !cli.silent || cli.show_error {
                eprintln!("aioduct http: {e}");
            }
            return ExitCode::from(23);
        }
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
