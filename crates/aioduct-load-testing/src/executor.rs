//! Executor types that control VU scheduling.

use std::time::Duration;

/// Defines how virtual users are scheduled during a load test.
#[derive(Debug, Clone)]
pub enum Executor {
    /// Fixed VU count, each runs N iterations then stops.
    PerVuIterations {
        /// Number of concurrent virtual users.
        vus: usize,
        /// Number of iterations each VU performs.
        iterations: usize,
    },

    /// Fixed VU count, runs until duration expires.
    ConstantVus {
        /// Number of concurrent virtual users.
        vus: usize,
        /// How long to run the test.
        duration: Duration,
    },

    /// All VUs share a global iteration pool. Stops when all iterations consumed.
    SharedIterations {
        /// Number of concurrent virtual users.
        vus: usize,
        /// Total iterations shared across all VUs.
        iterations: usize,
    },

    /// VU count changes over stages.
    RampingVus {
        /// Each stage: (target VU count, ramp duration to reach it).
        stages: Vec<(usize, Duration)>,
    },

    /// Fixed request arrival rate (open model). Spawns VUs as needed up to max.
    ConstantArrivalRate {
        /// Target requests per second.
        rate: f64,
        /// How long to maintain this rate.
        duration: Duration,
        /// Maximum VUs to spawn.
        max_vus: usize,
    },
}

impl Executor {
    /// The number of VUs this executor will use (or the maximum for arrival-rate).
    pub fn vus(&self) -> usize {
        match self {
            Self::PerVuIterations { vus, .. } => *vus,
            Self::ConstantVus { vus, .. } => *vus,
            Self::SharedIterations { vus, .. } => *vus,
            Self::RampingVus { stages } => stages.iter().map(|(v, _)| *v).max().unwrap_or(0),
            Self::ConstantArrivalRate { max_vus, .. } => *max_vus,
        }
    }
}
