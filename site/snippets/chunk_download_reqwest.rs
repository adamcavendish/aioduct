// comparison: reqwest equivalent
// NOT compiled in CI (external crate)

// reqwest has no built-in parallel range-request downloader.
// To download a file in parallel chunks you'd need to:
// 1. Send a HEAD request to get Content-Length
// 2. Calculate byte ranges for N chunks
// 3. Spawn N tasks, each requesting a byte range
// 4. Reassemble the chunks in order
// 5. Handle partial failures and retries
//
// This is ~100 lines of manual orchestration code.

fn main() {
    println!("Parallel chunk download not available in reqwest");
}
