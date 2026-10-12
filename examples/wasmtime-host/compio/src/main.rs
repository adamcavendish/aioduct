#[path = "../../../common/wasmtime-host/mod.rs"]
mod common;

use aioduct::wasmtime::{CompioHostTransport, WasiHttpHost};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let observer = common::host_observer("wasmtime-host-compio");
    compio_runtime::Runtime::new()?.block_on(async {
        common::run_with_host("compio", &observer, |origin| {
            Ok(WasiHttpHost::builder()
                .transport(CompioHostTransport::from_builder_factory(move || {
                    aioduct::CompioClient::builder().observer(observer)
                })?)
                .policy(common::policy_for_origin(origin)?)
                .build()?)
        })
        .await?;
        observer.complete();
        Ok(())
    })
}
