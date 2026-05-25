#[path = "bench_main/shared.rs"]
mod shared;

#[path = "bench_main/e2e_concurrent.rs"]
mod e2e_concurrent;
#[path = "bench_main/e2e_features.rs"]
mod e2e_features;
#[path = "bench_main/e2e_h1.rs"]
mod e2e_h1;
#[path = "bench_main/e2e_h2.rs"]
mod e2e_h2;
#[path = "bench_main/e2e_pooling.rs"]
mod e2e_pooling;
#[path = "bench_main/e2e_runtime.rs"]
mod e2e_runtime;
#[path = "bench_main/micro_body.rs"]
mod micro_body;
#[path = "bench_main/micro_cookie.rs"]
mod micro_cookie;
#[path = "bench_main/micro_pool.rs"]
mod micro_pool;

criterion::criterion_main!(
    e2e_h1::benches,
    e2e_h2::benches,
    e2e_concurrent::benches,
    e2e_features::benches,
    e2e_pooling::benches,
    e2e_runtime::benches,
    micro_pool::benches,
    micro_cookie::benches,
    micro_body::benches,
);
