//! General-purpose API load testing framework built on aioduct.
//!
//! # Core Concepts
//!
//! ## Virtual Users (VUs)
//!
//! A Virtual User is a concurrent actor that executes your scenario in a loop.
//! Each VU gets its own [`VuContext`] with an HTTP client, data feeder, and
//! metric recording. VUs run in parallel — 100 VUs means 100 concurrent
//! execution contexts.
//!
//! VUs are identified by a zero-based `vu_id`. Each VU tracks its own
//! `iteration` counter (how many times it has run the scenario).
//!
//! ## Scenarios
//!
//! A [`Scenario`] defines what each VU does on every iteration. Implement the
//! trait and return a pinned future:
//!
//! ```ignore
//! impl Scenario<TokioRuntime, TcpConnector> for GetUsers {
//!     fn run<'a>(
//!         &'a self,
//!         ctx: &'a mut VuContext<TokioRuntime, TcpConnector>,
//!     ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
//!         Box::pin(async move {
//!             let resp = ctx.client()
//!                 .get("https://api.example.com/users")?
//!                 .send().await?;
//!             ctx.set_status_code(resp.status().as_u16());
//!             Ok(())
//!         })
//!     }
//! }
//! ```
//!
//! ### Lifecycle hooks
//!
//! - **`on_start(ctx)`** — called once per VU before the first iteration.
//!   Use for login, token acquisition, or per-VU setup.
//! - **`run(ctx)`** — the main iteration body. Called repeatedly.
//! - **`on_stop(ctx)`** — called once when a VU finishes. Use for cleanup.
//!
//! ## Executors
//!
//! An [`Executor`] controls how many VUs run, for how long, and at what pace.
//!
//! | Executor | Model | Use case |
//! |----------|-------|----------|
//! | `PerVuIterations` | Each VU runs N iterations then stops | Functional test |
//! | `SharedIterations` | All VUs share a pool of N iterations | Fixed total requests |
//! | `ConstantVus` | Fixed VU count for a duration | Steady-state soak |
//! | `RampingVus` | VU count changes across stages | Ramp up, hold, ramp down |
//! | `ConstantArrivalRate` | Fixed request rate (open model) | Maintain N RPS |
//!
//! ### Closed vs open model
//!
//! `PerVuIterations`, `SharedIterations`, `ConstantVus`, and `RampingVus` use
//! a **closed model**: each VU waits for the previous iteration to finish
//! before starting the next. If the server slows down, request rate drops.
//!
//! `ConstantArrivalRate` uses an **open model**: iterations are scheduled at a
//! fixed rate regardless of whether previous ones finished. If the server slows
//! down, more VUs are spawned (up to `max_vus`) to maintain the target rate.
//!
//! ## Feeders
//!
//! A [`Feeder`](feeder::Feeder) provides data records to VUs for parameterized
//! testing. Load test data from files instead of hardcoding:
//!
//! ```ignore
//! let feeder = SharedJsonlFeederFactory::new("users.jsonl")?;
//! LoadTest::new().feeder(feeder).scenario(MyScenario).run().await;
//! ```
//!
//! Inside the scenario, call `ctx.feed::<T>()` to deserialize the next record.
//!
//! ### Feeder modes
//!
//! - **Per-VU**: Each VU gets its own cursor. Cycling wraps around; without
//!   cycling, returns `Err(FeederExhausted)`.
//! - **Shared queue**: All VUs draw from a single global cursor. Each record
//!   consumed exactly once.
//! - **Per-VU directory**: Each VU reads from its own JSONL file
//!   (round-robin assignment by `vu_id % file_count`).
//!
//! ## Metrics
//!
//! The framework records per-iteration data automatically: timestamp, vu,
//! iteration, status_code, latency_ms, success, error_msg.
//!
//! Record custom metrics via `ctx.record("name", value)` or
//! `ctx.record_with_tags("name", value, tags)`. The [`MetricsRegistry`]
//! aggregates samples into histograms and computes percentiles
//! (min, max, avg, med, p90, p95, p99) for the [`TestSummary`].
//!
//! ## Outputs
//!
//! Outputs receive metrics and produce reports. Multiple outputs fan out
//! simultaneously:
//!
//! - [`ConsoleOutput`](output::console::ConsoleOutput) — live progress bar +
//!   end-of-test summary table
//! - [`CsvOutput`](output::csv::CsvOutput) — one CSV row per request
//! - [`JsonlOutput`](output::jsonl::JsonlOutput) — one JSON line per request
//!
//! Implement [`Output`](output::Output) for custom backends.
//!
//! ## Cancellation
//!
//! The framework uses hierarchical [`CancellationToken`]s for cooperative
//! shutdown. Duration-based executors cancel the root when time expires.
//! VUs check `ctx.is_cancelled()` or await `ctx.cancelled()`. Cancelling a
//! parent automatically cancels all children.
//!
//! # Example
//!
//! ```ignore
//! use aioduct_load_testing::prelude::*;
//! use aioduct::runtime::TokioRuntime;
//! use aioduct::runtime::tokio_rt::TcpConnector;
//! use std::future::Future;
//! use std::pin::Pin;
//! use std::time::Duration;
//!
//! struct GetUsers;
//!
//! impl Scenario<TokioRuntime, TcpConnector> for GetUsers {
//!     fn run<'a>(
//!         &'a self,
//!         ctx: &'a mut VuContext<TokioRuntime, TcpConnector>,
//!     ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
//!         Box::pin(async move {
//!             let resp = ctx.client()
//!                 .get("https://api.example.com/users")?
//!                 .send().await?;
//!             ctx.set_status_code(resp.status().as_u16());
//!             Ok(())
//!         })
//!     }
//! }
//!
//! #[tokio::main]
//! async fn main() {
//!     let summary = LoadTest::<TokioRuntime, TcpConnector>::new()
//!         .scenario(GetUsers)
//!         .executor(Executor::ConstantVus {
//!             vus: 10,
//!             duration: Duration::from_secs(30),
//!         })
//!         .output(ConsoleOutput::new())
//!         .output(JsonlOutput::new("results.jsonl").unwrap())
//!         .run()
//!         .await;
//!
//!     println!("Total: {} requests, {:.0} RPS",
//!         summary.total_requests, summary.requests_per_second);
//! }
//! ```
//!
//! # Runtime support
//!
//! Enable one of `tokio`, `smol`, or `compio` features for native targets.
//! WASM/WASI targets get portable modules (cancel, metrics, feeders, outputs)
//! but not the full orchestrator.

// On native targets, require at least one runtime feature.
#[cfg(all(
    not(target_arch = "wasm32"),
    not(feature = "tokio"),
    not(feature = "smol"),
    not(feature = "compio"),
    not(doc)
))]
compile_error!(
    "aioduct-load-testing: on native targets, enable at least one runtime feature: tokio, smol, or compio"
);

// ── Portable modules (available on all targets) ──────────────────────────────
pub mod cancel;
pub mod error;
pub mod executor;
pub mod feeder;
pub mod metrics;
pub mod output;

// ── Native-only modules (require runtime + networking) ───────────────────────
#[cfg(not(target_arch = "wasm32"))]
pub mod controller;
#[cfg(not(target_arch = "wasm32"))]
pub mod scenario;
#[cfg(not(target_arch = "wasm32"))]
pub mod vu;

#[cfg(not(target_arch = "wasm32"))]
pub mod prelude;
