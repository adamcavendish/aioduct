#[path = "../../../wasmtime-host-common.rs"]
mod common;

use aioduct_wasmtime::WasiHttpHost;

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    common::run_with_host("tokio", |origin| {
        Ok(WasiHttpHost::builder()
            .transport(aioduct::TokioClient::builder().build()?)
            .policy(common::policy_for_origin(origin)?)
            .build()?)
    })
    .await
}
