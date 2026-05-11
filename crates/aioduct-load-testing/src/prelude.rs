//! Common re-exports for user convenience.

pub use crate::cancel::CancellationToken;
pub use crate::controller::LoadTest;
pub use crate::error::{Error, Result};
pub use crate::executor::Executor;
pub use crate::feeder::{Feeder, FeederFactory, JsonlFeeder};
pub use crate::metrics::{RequestRecord, Sample, Tags, TestSummary};
pub use crate::output::Output;
pub use crate::output::console::ConsoleOutput;
pub use crate::output::csv::CsvOutput;
pub use crate::output::jsonl::JsonlOutput;
pub use crate::scenario::Scenario;
pub use crate::vu::VuContext;

pub use aioduct::runtime::{ConnectorSend, RuntimePoll};
