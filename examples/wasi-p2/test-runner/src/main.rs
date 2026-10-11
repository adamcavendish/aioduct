use std::error::Error;
use std::io;

use wasmtime::component::{Component, Linker, ResourceTable};
use wasmtime::{Config, Engine, Store};
use wasmtime_wasi::p2::bindings::Command;
use wasmtime_wasi::{WasiCtx, WasiCtxView, WasiView};
use wasmtime_wasi_http::WasiHttpCtx;
use wasmtime_wasi_http::p2::bindings::http::types::ErrorCode;
use wasmtime_wasi_http::p2::body::HyperOutgoingBody;
use wasmtime_wasi_http::p2::types::{HostFutureIncomingResponse, OutgoingRequestConfig};
use wasmtime_wasi_http::p2::{HttpResult, WasiHttpCtxView, WasiHttpHooks, WasiHttpView};

struct RejectHttpRequests;

impl WasiHttpHooks for RejectHttpRequests {
    fn send_request(
        &mut self,
        _request: hyper::Request<HyperOutgoingBody>,
        _config: OutgoingRequestConfig,
    ) -> HttpResult<HostFutureIncomingResponse> {
        Err(ErrorCode::HttpProtocolError.into())
    }
}

struct State {
    table: ResourceTable,
    wasi: WasiCtx,
    http: WasiHttpCtx,
    http_hooks: RejectHttpRequests,
}

impl WasiView for State {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.table,
        }
    }
}

impl WasiHttpView for State {
    fn http(&mut self) -> WasiHttpCtxView<'_> {
        WasiHttpCtxView {
            ctx: &mut self.http,
            table: &mut self.table,
            hooks: &mut self.http_hooks,
        }
    }
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<(), Box<dyn Error>> {
    let mut args = std::env::args_os();
    let _runner = args.next();
    let component_path = args.next().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "Cargo did not provide a WASI-P2 component path",
        )
    })?;
    let guest_args: Vec<String> = args.map(|arg| arg.to_string_lossy().into_owned()).collect();

    let mut wasi = WasiCtx::builder();
    wasi.arg(component_path.to_string_lossy());
    wasi.args(&guest_args);
    wasi.inherit_stdout();
    wasi.inherit_stderr();

    let mut config = Config::new();
    config.wasm_component_model(true);
    let engine = Engine::new(&config)?;
    let component = Component::from_file(&engine, component_path)?;

    let mut store = Store::new(
        &engine,
        State {
            table: ResourceTable::new(),
            wasi: wasi.build(),
            http: WasiHttpCtx::new(),
            http_hooks: RejectHttpRequests,
        },
    );
    let mut linker = Linker::new(&engine);
    wasmtime_wasi::p2::add_to_linker_async(&mut linker)?;
    wasmtime_wasi_http::p2::add_only_http_to_linker_async(&mut linker)?;

    let command = Command::instantiate_async(&mut store, &component, &linker).await?;
    command
        .wasi_cli_run()
        .call_run(&mut store)
        .await?
        .map_err(|()| io::Error::other("WASI-P2 guest returned failure"))?;

    Ok(())
}
