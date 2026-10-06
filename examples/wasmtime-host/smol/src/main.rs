#[path = "../../../smol/smol_threads.rs"]
mod smol_threads;

#[path = "../../../wasmtime-host-common.rs"]
mod common;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    smol_threads::configure();
    smol::block_on(common::run_with_host("smol", |origin| {
        Ok(aioduct::wasmtime::WasiHttpHost::builder()
            .transport(aioduct::SmolClient::builder().build()?)
            .policy(common::policy_for_origin(origin)?)
            .build()?)
    }))
}
