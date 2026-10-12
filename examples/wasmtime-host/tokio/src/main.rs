#[path = "../../../common/wasmtime-host/mod.rs"]
mod common;

use aioduct::wasmtime::WasiHttpHost;

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let observer = common::host_observer("wasmtime-host-tokio");
    common::run_with_host("tokio", &observer, |origin| {
        Ok(WasiHttpHost::builder()
            .transport(aioduct::TokioClient::builder().observer(observer).build()?)
            .policy(common::policy_for_origin(origin)?)
            .build()?)
    })
    .await?;
    observer.complete();
    Ok(())
}
