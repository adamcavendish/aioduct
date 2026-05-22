// features: wasm,json
// runtime: wasm
use aioduct::WasmClient;

// This same library works in the browser via WebAssembly!
#[allow(dead_code)]
async fn fetch_in_browser() -> Result<String, aioduct::Error> {
    let client = WasmClient::new();
    let resp = client.get("https://httpbin.org/get")?.send().await?;
    resp.text().await
}

fn main() {}
