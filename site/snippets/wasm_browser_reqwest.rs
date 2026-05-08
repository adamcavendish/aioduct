// comparison: reqwest equivalent
// NOT compiled in CI (external crate)

// reqwest has limited WASM support — it compiles to
// wasm32-unknown-unknown but has significant limitations:
// - No streaming responses
// - No connection pooling control
// - No timeout support on all platforms
// - Limited header access due to CORS
//
// aioduct's WasmClient is purpose-built for browser use,
// with full access to the web_sys::fetch API.

fn main() {
    println!("reqwest WASM support is limited");
}
