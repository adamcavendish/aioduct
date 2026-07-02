use std::path::{Path, PathBuf};
use std::time::Duration;

use aioduct_wasmtime::{ExactOriginPolicy, WasiHttpHost};
use http::HeaderValue;
use http::header::AUTHORIZATION;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use wasmtime::component::{Component, Linker, ResourceTable};
use wasmtime::{Config, Engine, Store};
use wasmtime_wasi::p2::bindings::Command as WasiCommand;
use wasmtime_wasi::p2::pipe::MemoryOutputPipe;
use wasmtime_wasi::{WasiCtx, WasiCtxView, WasiView};
use wasmtime_wasi_http::WasiHttpCtx;
use wasmtime_wasi_http::p2::{WasiHttpCtxView, WasiHttpView};

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

#[tokio::test(flavor = "multi_thread")]
async fn guest_wasi_client_is_serviced_by_host_adapter() -> Result<(), Box<dyn std::error::Error>> {
    let component = build_wasi_demo_component()?;
    let (addr, server) = demo_server().await?;
    let origin = format!("http://{addr}");
    let hooks = test_host(
        ExactOriginPolicy::new(&origin)?
            .forbid_sensitive_headers()
            .inject_header(AUTHORIZATION, HeaderValue::from_static("Bearer e2e-secret"))
            .header_limit(16 * 1024)
            .body_limit(1024 * 1024),
    )?;

    let stdout = MemoryOutputPipe::new(16 * 1024);
    let stderr = MemoryOutputPipe::new(16 * 1024);
    let mut wasi = WasiCtx::builder();
    wasi.stdout(stdout.clone());
    wasi.stderr(stderr.clone());
    wasi.env("AIODUCT_WASI_DEMO_URL", origin);

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

    let requests = tokio::time::timeout(Duration::from_secs(5), server).await??;
    assert_eq!(requests.len(), 3);
    for request in &requests {
        assert!(request.contains("authorization: Bearer e2e-secret"));
    }
    let post = requests
        .iter()
        .find(|request| request.starts_with("POST /post "))
        .expect("guest should send POST /post");
    assert!(
        post.contains("\"message\":\"hello from WASI\"")
            && post.contains("\"runtime\":\"wasm32-wasip2\""),
        "server should receive the guest JSON body:\n{post}"
    );

    let stdout = String::from_utf8_lossy(&stdout.contents()).into_owned();
    let stderr = String::from_utf8_lossy(&stderr.contents()).into_owned();
    assert!(stderr.is_empty(), "guest stderr:\n{stderr}");
    assert!(stdout.contains("Status: 200"), "guest stdout:\n{stdout}");
    assert!(
        stdout.contains("Expected error:"),
        "guest should exercise error_for_status:\n{stdout}"
    );

    Ok(())
}

fn test_host(policy: ExactOriginPolicy) -> Result<WasiHttpHost, Box<dyn std::error::Error>> {
    let builder = WasiHttpHost::builder().policy(policy);
    #[cfg(feature = "tokio")]
    {
        Ok(builder.build()?)
    }
    #[cfg(all(not(feature = "tokio"), feature = "smol"))]
    {
        Ok(builder
            .transport(aioduct::SmolClient::builder().build()?)
            .build()?)
    }
    #[cfg(all(not(feature = "tokio"), not(feature = "smol"), feature = "compio"))]
    {
        Ok(builder
            .transport(aioduct_wasmtime::CompioHostTransport::new()?)
            .build()?)
    }
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
        .nth(2)
        .expect("crate should live under workspace/crates")
        .to_path_buf()
}

async fn demo_server()
-> std::io::Result<(std::net::SocketAddr, tokio::task::JoinHandle<Vec<String>>)> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let handle = tokio::spawn(async move {
        let mut requests = Vec::new();
        for _ in 0..3 {
            let Ok((mut stream, _)) = listener.accept().await else {
                break;
            };
            let request = read_http_request(&mut stream).await.unwrap_or_default();
            let response = response_for_request(&request);
            let _ = stream.write_all(response.as_bytes()).await;
            requests.push(request);
        }
        requests
    });
    Ok((addr, handle))
}

async fn read_http_request(stream: &mut tokio::net::TcpStream) -> std::io::Result<String> {
    let mut bytes = Vec::new();
    let mut buf = [0_u8; 1024];
    loop {
        let read = stream.read(&mut buf).await?;
        if read == 0 {
            break;
        }
        bytes.extend_from_slice(&buf[..read]);
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
        .find_map(|line| line.strip_prefix("content-length: "))
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
