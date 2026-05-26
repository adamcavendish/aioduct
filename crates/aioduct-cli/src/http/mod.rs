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
            let value = v.to_str().unwrap_or("<binary>").to_owned();
            let display_value = redact_header_value(&value, k.as_str());
            (k.as_str().to_owned(), display_value.to_owned())
        })
        .collect();

    let is_binary = is_binary_content_type(&resp_headers);

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
                                if !is_binary {
                                    let text = String::from_utf8_lossy(&data);
                                    tui.send_body_chunk(text.into_owned());
                                }
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
            if is_binary {
                tui.send_body_chunk(format!(
                    "[binary data — {} bytes, {}]\n",
                    body_buf.len(),
                    resp_headers
                        .iter()
                        .find(|(k, _)| k.eq_ignore_ascii_case("content-type"))
                        .map(|(_, v)| v.as_str())
                        .unwrap_or("unknown type")
                ));
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

fn redact_header_value<'a>(value: &'a str, name: &str) -> &'a str {
    let lower = name.to_lowercase();
    if lower == "authorization"
        || lower == "proxy-authorization"
        || lower == "cookie"
        || lower == "set-cookie"
    {
        "***"
    } else {
        value
    }
}

fn is_binary_content_type(headers: &[(String, String)]) -> bool {
    let ct = headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("content-type"))
        .map(|(_, v)| v.as_str())
        .unwrap_or("");

    let ct_lower = ct.to_lowercase();

    if ct_lower.starts_with("text/") || ct_lower == "application/json" {
        return false;
    }

    if ct_lower.starts_with("image/")
        || ct_lower.starts_with("audio/")
        || ct_lower.starts_with("video/")
        || ct_lower.starts_with("font/")
    {
        return true;
    }

    matches!(
        ct_lower.as_str(),
        "application/octet-stream"
            | "application/pdf"
            | "application/zip"
            | "application/gzip"
            | "application/x-tar"
            | "application/x-gtar"
            | "application/x-compressed"
            | "application/x-bzip2"
            | "application/x-xz"
            | "application/zstd"
            | "application/protobuf"
            | "application/x-protobuf"
            | "application/msgpack"
            | "application/x-msgpack"
            | "application/cbor"
            | "application/wasm"
    )
}
