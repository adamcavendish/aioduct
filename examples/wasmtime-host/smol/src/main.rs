#[path = "../../../smol/smol_threads.rs"]
mod smol_threads;

#[path = "../../../common/wasmtime-host/mod.rs"]
mod common;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    smol_threads::configure();
    let observer = common::host_observer("wasmtime-host-smol");
    smol::block_on(async {
        common::run_with_host("smol", &observer, |origin| {
            Ok(aioduct::wasmtime::WasiHttpHost::builder()
                .transport(aioduct::SmolClient::builder().observer(observer).build()?)
                .policy(common::policy_for_origin(origin)?)
                .build()?)
        })
        .await?;
        observer.complete();
        Ok(())
    })
}
