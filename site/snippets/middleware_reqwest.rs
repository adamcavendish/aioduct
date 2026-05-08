// comparison: reqwest equivalent
// NOT compiled in CI (external crate)

// reqwest has no native middleware system.
// Use reqwest-middleware crate (separate ecosystem):
//
// use reqwest_middleware::{ClientBuilder, ClientWithMiddleware};
// use reqwest_retry::{RetryTransientMiddleware, policies::ExponentialBackoff};
//
// let retry_policy = ExponentialBackoff::builder().build_with_max_retries(3);
// let client = ClientBuilder::new(reqwest::Client::new())
//     .with(RetryTransientMiddleware::new_with_policy(retry_policy))
//     .build();
//
// Each middleware type requires a separate crate:
// - reqwest-retry for retries
// - reqwest-tracing for tracing
// - Custom middleware: implement reqwest_middleware::Middleware trait

fn main() {
    println!("reqwest requires reqwest-middleware ecosystem");
}
