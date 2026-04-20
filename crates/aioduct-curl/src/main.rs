mod cli;
mod client;
mod output;
mod request;

use std::process::ExitCode;

use clap::Parser;

use cli::Cli;

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();

    let http_client = client::build_client(&cli);

    let resp = match request::execute(&cli, &http_client).await {
        Ok(r) => r,
        Err(e) => {
            if !cli.silent || cli.show_error {
                eprintln!("aioduct-curl: {e}");
            }
            return exit_code_for_error(&e);
        }
    };

    let status = resp.status();

    if let Err(e) = output::handle(&cli, resp).await {
        if !cli.silent || cli.show_error {
            eprintln!("aioduct-curl: {e}");
        }
        return ExitCode::from(23);
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
