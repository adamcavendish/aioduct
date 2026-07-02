#[path = "../../../wasmtime-host-common.rs"]
mod common;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    smol::block_on(common::run_with_host("smol", |origin| {
        Ok(aioduct_wasmtime::WasiHttpHost::builder()
            .transport(aioduct::SmolClient::builder().build()?)
            .policy(common::policy_for_origin(origin)?)
            .build()?)
    }))
}
