// comparison: reqwest equivalent
// NOT compiled in CI (external crate)

// reqwest does not support HTTP/3.
// There is no way to do this with reqwest today.
//
// For HTTP/3, you'd need to use h3 + quinn directly,
// which requires hundreds of lines of manual setup.
fn main() {
    println!("HTTP/3 is not available in reqwest");
}
