use std::io::Write;
use std::path::Path;

use aioduct::Response;

use crate::cli::Cli;

pub async fn handle(cli: &Cli, resp: Response) -> Result<(), aioduct::Error> {
    if cli.verbose {
        eprint_response_info(&resp);
    }

    if let Some(ref path) = cli.dump_header {
        dump_headers(&resp, path)?;
    }

    if cli.head {
        print_headers(&resp);
        return Ok(());
    }

    if cli.include {
        print_headers(&resp);
    }

    let status = resp.status();

    if cli.remote_name {
        let filename = filename_from_url(&cli.url);
        write_body_to_file(resp, Path::new(&filename)).await?;
        if !cli.silent {
            eprintln!("Saved to {filename}");
        }
    } else if let Some(ref path) = cli.output {
        write_body_to_file(resp, path).await?;
    } else {
        let body = resp.bytes().await?;
        let stdout = std::io::stdout();
        let mut out = stdout.lock();
        out.write_all(&body).map_err(aioduct::Error::Io)?;
        if atty_stdout() && !body.ends_with(b"\n") {
            let _ = out.write_all(b"\n");
        }
    }

    if let Some(ref fmt) = cli.write_out {
        print_write_out(fmt, status);
    }

    Ok(())
}

fn print_headers(resp: &Response) {
    println!("HTTP/{:?} {}", resp.version(), resp.status());
    for (name, value) in resp.headers() {
        if let Ok(v) = value.to_str() {
            println!("{}: {}", name.as_str(), v);
        }
    }
    println!();
}

fn eprint_response_info(resp: &Response) {
    eprintln!("< HTTP/{:?} {}", resp.version(), resp.status());
    for (name, value) in resp.headers() {
        if let Ok(v) = value.to_str() {
            eprintln!("< {}: {}", name.as_str(), v);
        }
    }
    eprintln!("<");
}

fn dump_headers(resp: &Response, path: &Path) -> Result<(), aioduct::Error> {
    let mut out = std::fs::File::create(path).map_err(aioduct::Error::Io)?;
    writeln!(out, "HTTP/{:?} {}", resp.version(), resp.status()).map_err(aioduct::Error::Io)?;
    for (name, value) in resp.headers() {
        if let Ok(v) = value.to_str() {
            writeln!(out, "{}: {}", name.as_str(), v).map_err(aioduct::Error::Io)?;
        }
    }
    writeln!(out).map_err(aioduct::Error::Io)?;
    Ok(())
}

async fn write_body_to_file(resp: Response, path: &Path) -> Result<(), aioduct::Error> {
    let body = resp.bytes().await?;
    std::fs::write(path, &body).map_err(aioduct::Error::Io)?;
    Ok(())
}

fn print_write_out(fmt: &str, status: http::StatusCode) {
    let output = fmt
        .replace("%{http_code}", &status.as_u16().to_string())
        .replace("%{response_code}", &status.as_u16().to_string())
        .replace("\\n", "\n");
    print!("{output}");
}

fn filename_from_url(url: &str) -> String {
    let path = url
        .split('?')
        .next()
        .unwrap_or(url)
        .split('#')
        .next()
        .unwrap_or(url);

    let name = path.rsplit('/').next().unwrap_or("output");
    let name = name.trim();

    if name.is_empty() {
        "output".to_string()
    } else {
        name.to_string()
    }
}

fn atty_stdout() -> bool {
    unsafe { libc_isatty(1) != 0 }
}

unsafe extern "C" {
    #[link_name = "isatty"]
    fn libc_isatty(fd: i32) -> i32;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filename_extraction() {
        assert_eq!(
            filename_from_url("https://example.com/file.tar.gz"),
            "file.tar.gz"
        );
        assert_eq!(filename_from_url("https://example.com/"), "output");
        assert_eq!(filename_from_url("https://example.com/dl?v=1"), "dl");
    }

    #[test]
    fn write_out_format() {
        let mut buf = String::new();
        let formatted = "%{http_code}"
            .replace("%{http_code}", &200u16.to_string())
            .replace("\\n", "\n");
        buf.push_str(&formatted);
        assert_eq!(buf, "200");
    }
}
