#[path = "../../../wasmtime-host-common.rs"]
mod common;

use aioduct::wasmtime::{CompioHostTransport, WasiHttpHost};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    compio_runtime::Runtime::new()?.block_on(common::run_with_host("compio", |origin| {
        Ok(WasiHttpHost::builder()
            .transport(CompioHostTransport::from_builder_factory(
                aioduct::CompioClient::builder,
            )?)
            .policy(common::policy_for_origin(origin)?)
            .build()?)
    }))
}
