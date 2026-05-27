use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use aioduct::Response;
use async_trait::async_trait;
use http::StatusCode;

use super::cli::HttpArgs;
use super::output;
use super::verbose_tui::VerboseTui;

#[async_trait]
pub(crate) trait ResponseOutput {
    /// Consume the response body.
    async fn consume_body(&mut self, resp: Response) -> Result<(), aioduct::Error>;

    /// Finalize output on success: flush stdout, print write-out, wait for TUI, return exit code.
    async fn finish(&mut self) -> ExitCode;

    /// Abort on error: stop the TUI if active, no-op otherwise.
    async fn abort(&mut self);
}

pub(crate) struct TuiOutput {
    tui: VerboseTui,
    url: String,
    remote_name: bool,
    output_path: Option<PathBuf>,
    head: bool,
    include: bool,
    silent: bool,
    show_error: bool,
    write_out: Option<String>,
    version: http::Version,
    status: StatusCode,
    resp_headers: Vec<(String, String)>,
    is_binary: bool,
    body_buf: Vec<u8>,
    cancel_rx: tokio::sync::watch::Receiver<bool>,
}

impl TuiOutput {
    pub(crate) fn new(
        tui: VerboseTui,
        cli: &HttpArgs,
        version: http::Version,
        status: StatusCode,
        resp_headers: Vec<(String, String)>,
        is_binary: bool,
        cancel_rx: tokio::sync::watch::Receiver<bool>,
    ) -> Self {
        Self {
            tui,
            url: cli.url.clone(),
            remote_name: cli.remote_name,
            output_path: cli.output.clone(),
            head: cli.head,
            include: cli.include,
            silent: cli.silent,
            show_error: cli.show_error,
            write_out: cli.write_out.clone(),
            version,
            status,
            resp_headers,
            is_binary,
            body_buf: Vec::new(),
            cancel_rx,
        }
    }
}

#[async_trait]
impl ResponseOutput for TuiOutput {
    async fn consume_body(&mut self, resp: Response) -> Result<(), aioduct::Error> {
        if self.head {
            return Ok(());
        }

        let mut stream = resp.into_bytes_stream();
        loop {
            tokio::select! {
                biased;
                _ = self.cancel_rx.changed() => {
                    break;
                }
                chunk = stream.next() => {
                    match chunk {
                        Some(Ok(data)) => {
                            self.body_buf.extend_from_slice(&data);
                            if !self.is_binary {
                                let text = String::from_utf8_lossy(&data);
                                self.tui.send_body_chunk(text.into_owned());
                            }
                        }
                        Some(Err(e)) => {
                            return Err(e);
                        }
                        None => break,
                    }
                }
            }
        }

        if self.is_binary {
            self.tui.send_body_chunk(format!(
                "[binary data — {} bytes, {}]\n",
                self.body_buf.len(),
                self.resp_headers
                    .iter()
                    .find(|(k, _)| k.eq_ignore_ascii_case("content-type"))
                    .map(|(_, v)| v.as_str())
                    .unwrap_or("unknown type")
            ));
        }
        self.tui.send_body_done();

        // Write body to file for --remote-name / --output
        if self.remote_name {
            let filename = output::filename_from_url(&self.url);
            std::fs::write(Path::new(&filename), &self.body_buf).map_err(|e| {
                if !self.silent || self.show_error {
                    eprintln!("aioduct http: failed to write {filename}: {e}");
                }
                aioduct::Error::Io(e)
            })?;
            if !self.silent {
                eprintln!("Saved to {filename}");
            }
        } else if let Some(ref path) = self.output_path {
            std::fs::write(path, &self.body_buf).map_err(|e| {
                if !self.silent || self.show_error {
                    eprintln!("aioduct http: failed to write {}: {e}", path.display());
                }
                aioduct::Error::Io(e)
            })?;
        }

        Ok(())
    }

    async fn finish(&mut self) -> ExitCode {
        self.tui.wait().await;

        let write_body = !self.head && !self.remote_name && self.output_path.is_none();

        if self.head || self.include {
            output::write_headers_stdout(self.version, self.status, &self.resp_headers);
        }

        if write_body {
            write_stdout(&self.body_buf, self.silent, self.show_error);
        }

        if let Some(ref fmt) = self.write_out {
            output::print_write_out(fmt, self.status);
        }

        if self.status.is_client_error() || self.status.is_server_error() {
            ExitCode::from(22)
        } else {
            ExitCode::SUCCESS
        }
    }

    async fn abort(&mut self) {
        self.tui.stop().await;
    }
}

pub(crate) struct PlainOutput {
    dump_header: Option<PathBuf>,
    head: bool,
    include: bool,
    remote_name: bool,
    output_path: Option<PathBuf>,
    silent: bool,
    show_error: bool,
    write_out: Option<String>,
    url: String,
    version: http::Version,
    status: StatusCode,
    headers: Vec<(String, String)>,
}

impl PlainOutput {
    pub(crate) fn new(
        cli: &HttpArgs,
        version: http::Version,
        status: StatusCode,
        headers: Vec<(String, String)>,
    ) -> Self {
        Self {
            dump_header: cli.dump_header.clone(),
            head: cli.head,
            include: cli.include,
            remote_name: cli.remote_name,
            output_path: cli.output.clone(),
            silent: cli.silent,
            show_error: cli.show_error,
            write_out: cli.write_out.clone(),
            url: cli.url.clone(),
            version,
            status,
            headers,
        }
    }
}

#[async_trait]
impl ResponseOutput for PlainOutput {
    async fn consume_body(&mut self, resp: Response) -> Result<(), aioduct::Error> {
        if let Some(ref path) = self.dump_header {
            output::dump_headers_file(self.version, self.status, &self.headers, path)?;
        }

        if self.head {
            if self.include {
                output::write_headers_stdout(self.version, self.status, &self.headers);
            }
            return Ok(());
        }

        if self.include {
            output::write_headers_stdout(self.version, self.status, &self.headers);
        }

        if self.remote_name {
            let filename = output::filename_from_url(&self.url);
            output::stream_body_to_file(resp, Path::new(&filename)).await?;
            if !self.silent {
                eprintln!("Saved to {filename}");
            }
        } else if let Some(ref path) = self.output_path {
            output::stream_body_to_file(resp, path).await?;
        } else {
            output::stream_body_to_stdout(resp).await?;
        }

        Ok(())
    }

    async fn finish(&mut self) -> ExitCode {
        if let Some(ref fmt) = self.write_out {
            output::print_write_out(fmt, self.status);
        }

        if self.status.is_client_error() || self.status.is_server_error() {
            ExitCode::from(22)
        } else {
            ExitCode::SUCCESS
        }
    }

    async fn abort(&mut self) {
        // no-op: no TUI to clean up
    }
}

/// Synchronous stdout writer — keeps the non-Send lock guard off the async state machine.
fn write_stdout(body: &[u8], silent: bool, show_error: bool) {
    let mut out = std::io::stdout().lock();
    if let Err(e) = out.write_all(body) {
        if !silent || show_error {
            eprintln!("aioduct http: {e}");
        }
        return;
    }
    if std::io::stdout().is_terminal() && !body.ends_with(b"\n") {
        let _ = out.write_all(b"\n");
    }
}
