// comparison: reqwest equivalent
// NOT compiled in CI (external crate)

// reqwest does not support DNS-over-HTTPS.
// You'd need to bring in hickory-resolver manually,
// configure it for DoH, then implement the Resolve trait
// and wire it into reqwest — ~50 lines of glue code.

fn main() {
    println!("DoH is not available in reqwest");
}
