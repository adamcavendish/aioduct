// comparison: reqwest equivalent
// NOT compiled in CI (external crate)

// reqwest is locked to tokio. If your project uses smol,
// async-std, compio, or any other runtime, you cannot use
// reqwest without also pulling in tokio as a dependency.
//
// aioduct supports tokio, smol, and compio — just swap
// the generic parameter. Same API, any runtime.

fn main() {
    println!("reqwest only supports tokio");
}
