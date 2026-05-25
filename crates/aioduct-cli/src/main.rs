#![allow(dead_code)]

mod common;
mod download;
mod http;

use std::process::ExitCode;

use clap::{CommandFactory, Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "aioduct",
    about = "Unified HTTP toolkit — parallel downloads and HTTP requests",
    version
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Parallel segmented download
    Download(download::DownloadArgs),
    /// HTTP request tool (curl-style)
    Http(http::HttpArgs),
    /// Print version information
    Version,
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Some(Command::Download(args)) => download::run(args).await,
        Some(Command::Http(args)) => http::run(args).await,
        Some(Command::Version) => {
            print_version();
            ExitCode::SUCCESS
        }
        None => {
            Cli::command().print_help().ok();
            println!();
            ExitCode::from(2)
        }
    }
}

fn print_version() {
    println!("aioduct {}", env!("CARGO_PKG_VERSION"));
}
