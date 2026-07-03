use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use aioduct_wasmtime::{ExactOriginPolicy, WasiHttpHost};
use http::HeaderValue;
use http::header::{AUTHORIZATION, FORWARDED};
use wasmtime::component::{Component, Linker, ResourceTable};
use wasmtime::{Config, Engine, Store};
use wasmtime_wasi::p2::bindings::Command as WasiCommand;
use wasmtime_wasi::p2::pipe::MemoryOutputPipe;
use wasmtime_wasi::{WasiCtx, WasiCtxView, WasiView};
use wasmtime_wasi_http::WasiHttpCtx;
use wasmtime_wasi_http::p2::{WasiHttpCtxView, WasiHttpView};

const SECRET_HEADER: &str = "Bearer host-owned-token";

struct HostState {
    table: ResourceTable,
    wasi: WasiCtx,
    http: WasiHttpCtx,
    hooks: WasiHttpHost,
}

impl WasiView for HostState {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.table,
        }
    }
}

impl WasiHttpView for HostState {
    fn http(&mut self) -> WasiHttpCtxView<'_> {
        WasiHttpCtxView {
            ctx: &mut self.http,
            table: &mut self.table,
            hooks: &mut self.hooks,
        }
    }
}

pub fn policy_for_origin(origin: &str) -> Result<ExactOriginPolicy, Box<dyn std::error::Error>> {
    Ok(ExactOriginPolicy::new(origin)?
        .forbid_sensitive_headers()
        .deny_headers([FORWARDED])
        .deny_header_prefixes(["x-forwarded-", "proxy-"])
        .inject_header(AUTHORIZATION, HeaderValue::from_static(SECRET_HEADER))
        .header_limit(16 * 1024)
        .body_limit(1024 * 1024)
        .deadline(Instant::now() + Duration::from_secs(10)))
}

pub async fn run_with_host(
    runtime_name: &'static str,
    build_host: impl FnOnce(&str) -> Result<WasiHttpHost, Box<dyn std::error::Error>>,
) -> Result<(), Box<dyn std::error::Error>> {
    let component = component_path()?;
    let server = DemoServer::start()?;
    let origin = server.origin();
    let hooks = build_host(&origin)?;

    let stdout = MemoryOutputPipe::new(16 * 1024);
    let stderr = MemoryOutputPipe::new(16 * 1024);
    let mut wasi = WasiCtx::builder();
    wasi.stdout(stdout.clone());
    wasi.stderr(stderr.clone());
    wasi.env("AIODUCT_WASI_DEMO_URL", origin.clone());

    let mut config = Config::new();
    config.wasm_component_model(true);
    config.wasm_backtrace_details(wasmtime::WasmBacktraceDetails::Enable);
    let engine = Engine::new(&config)?;
    let component = Component::from_file(&engine, component)?;
    let mut store = Store::new(
        &engine,
        HostState {
            table: ResourceTable::new(),
            wasi: wasi.build(),
            http: WasiHttpCtx::new(),
            hooks,
        },
    );

    let mut linker = Linker::new(&engine);
    wasmtime_wasi::p2::add_to_linker_async(&mut linker)?;
    wasmtime_wasi_http::p2::add_only_http_to_linker_async(&mut linker)?;
    let command = WasiCommand::instantiate_async(&mut store, &component, &linker).await?;
    command
        .wasi_cli_run()
        .call_run(&mut store)
        .await?
        .map_err(|()| std::io::Error::other("guest returned failure"))?;

    let requests = server.finish()?;
    let stdout = String::from_utf8_lossy(&stdout.contents()).into_owned();
    let stderr = String::from_utf8_lossy(&stderr.contents()).into_owned();
    validate_demo(&requests, &stdout, &stderr)?;
    print_report(runtime_name, &origin, &requests, &stdout);

    Ok(())
}

fn component_path() -> Result<PathBuf, Box<dyn std::error::Error>> {
    if let Some(path) = std::env::args_os().nth(1) {
        return Ok(PathBuf::from(path));
    }
    build_wasi_demo_component()
}

fn build_wasi_demo_component() -> Result<PathBuf, Box<dyn std::error::Error>> {
    let root = workspace_root();
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let output = std::process::Command::new(cargo)
        .current_dir(&root)
        .args([
            "build",
            "-p",
            "example-wasi-p2-demo",
            "--target",
            "wasm32-wasip2",
        ])
        .output()?;
    if !output.status.success() {
        return Err(std::io::Error::other(format!(
            "failed to build WASI demo component\nstatus: {}\nstdout:\n{}\nstderr:\n{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ))
        .into());
    }

    let target = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .map(|path| {
            if path.is_absolute() {
                path
            } else {
                root.join(path)
            }
        })
        .unwrap_or_else(|| root.join("target"));
    Ok(target
        .join("wasm32-wasip2")
        .join("debug")
        .join("example-wasi-p2-demo.wasm"))
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(3)
        .expect("example should live under workspace/examples/wasmtime-host")
        .to_path_buf()
}

struct DemoServer {
    addr: SocketAddr,
    stop: Arc<AtomicBool>,
    handle: thread::JoinHandle<std::io::Result<Vec<String>>>,
}

impl DemoServer {
    fn start() -> std::io::Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        listener.set_nonblocking(true)?;
        let addr = listener.local_addr()?;
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let handle = thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(20);
            let mut requests = Vec::new();
            while requests.len() < 3
                && !worker_stop.load(Ordering::Relaxed)
                && Instant::now() < deadline
            {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let request = read_http_request(&mut stream)?;
                        if request.is_empty() {
                            continue;
                        }
                        let response = response_for_request(&request);
                        stream.write_all(response.as_bytes())?;
                        requests.push(request);
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(error) => return Err(error),
                }
            }
            Ok(requests)
        });

        Ok(Self { addr, stop, handle })
    }

    fn origin(&self) -> String {
        format!("http://{}", self.addr)
    }

    fn finish(self) -> Result<Vec<String>, Box<dyn std::error::Error>> {
        self.stop.store(true, Ordering::Relaxed);
        Ok(self
            .handle
            .join()
            .map_err(|_| std::io::Error::other("demo server thread panicked"))??)
    }
}

fn read_http_request(stream: &mut TcpStream) -> std::io::Result<String> {
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    let mut bytes = Vec::new();
    let mut buf = [0_u8; 1024];
    loop {
        match stream.read(&mut buf) {
            Ok(0) => break,
            Ok(read) => bytes.extend_from_slice(&buf[..read]),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                break;
            }
            Err(error) => return Err(error),
        }
        if request_complete(&bytes) || bytes.len() > 16 * 1024 {
            break;
        }
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

fn request_complete(bytes: &[u8]) -> bool {
    let Some(header_end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") else {
        return false;
    };
    let headers = String::from_utf8_lossy(&bytes[..header_end]);
    let content_length = headers
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim())
        })
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(0);
    bytes.len() >= header_end + 4 + content_length
}

fn response_for_request(request: &str) -> String {
    let first_line = request.lines().next().unwrap_or_default();
    let (status, content_type, body) = if first_line.starts_with("GET /get ") {
        ("200 OK", "text/plain", "hello from host adapter\n")
    } else if first_line.starts_with("POST /post ") {
        ("200 OK", "application/json", "{\"ok\":true}\n")
    } else if first_line.starts_with("GET /status/404 ") {
        ("404 Not Found", "text/plain", "")
    } else {
        (
            "500 Internal Server Error",
            "text/plain",
            "unexpected path\n",
        )
    };
    format!(
        "HTTP/1.1 {status}\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    )
}

fn validate_demo(
    requests: &[String],
    stdout: &str,
    stderr: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    if requests.len() != 3 {
        return Err(std::io::Error::other(format!(
            "expected 3 guest requests, observed {}",
            requests.len()
        ))
        .into());
    }
    for request in requests {
        if !has_injected_secret(request) {
            return Err(std::io::Error::other(format!(
                "missing host-injected authorization on request line {:?}",
                request.lines().next().unwrap_or_default()
            ))
            .into());
        }
    }
    let post = requests
        .iter()
        .find(|request| request.starts_with("POST /post "))
        .ok_or_else(|| std::io::Error::other("guest did not send POST /post"))?;
    if !(post.contains("\"message\":\"hello from WASI\"")
        && post.contains("\"runtime\":\"wasm32-wasip2\""))
    {
        return Err(std::io::Error::other("guest POST body did not reach the host").into());
    }
    if !stderr.is_empty() {
        return Err(std::io::Error::other(format!("guest stderr was not empty:\n{stderr}")).into());
    }
    if !stdout.contains("Status: 200") || !stdout.contains("Expected error:") {
        return Err(std::io::Error::other(format!(
            "guest stdout did not include expected success and error paths:\n{stdout}"
        ))
        .into());
    }
    Ok(())
}

fn has_injected_secret(request: &str) -> bool {
    request.lines().any(|line| {
        let Some((name, value)) = line.split_once(':') else {
            return false;
        };
        name.eq_ignore_ascii_case("authorization") && value.trim() == SECRET_HEADER
    })
}

fn print_report(runtime_name: &str, origin: &str, requests: &[String], stdout: &str) {
    println!("aioduct-wasmtime {runtime_name} host demo");
    println!("origin: {origin}");
    println!("guest stdout:");
    println!("{stdout}");
    println!("host observations:");
    for request in requests {
        let first_line = request.lines().next().unwrap_or_default();
        let injected = if has_injected_secret(request) {
            "yes"
        } else {
            "no"
        };
        println!("  {first_line} | authorization injected: {injected}");
    }
    println!("host-owned secret header value was withheld from host output");
}
