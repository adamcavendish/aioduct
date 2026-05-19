use std::time::Duration;

use criterion::{Criterion, criterion_group};

use super::shared::*;

const NO_POOL_MAX_ITERS: u64 = 200;

fn bench_h1_pool_vs_no_pool(c: &mut Criterion) {
    let url = h1_small_url();

    let no_pool = aioduct::HttpEngineSend::<
        aioduct::runtime::TokioRuntime,
        aioduct::runtime::tokio_rt::TcpConnector,
    >::builder()
    .no_connection_reuse()
    .build()
    .unwrap();

    let mut group = c.benchmark_group("e2e_pooling/h1");
    group.sample_size(10);
    group.warm_up_time(Duration::from_millis(500));
    group.measurement_time(Duration::from_secs(2));
    group.bench_function("with_pool", |b| {
        b.to_async(&*RT).iter(|| async {
            AIODUCT_H1
                .get(&url)
                .unwrap()
                .send()
                .await
                .unwrap()
                .bytes()
                .await
                .unwrap()
        });
    });
    group.bench_function("no_pool", |b| {
        b.to_async(&*RT).iter_custom(|iters| {
            let client = no_pool.clone();
            let u = url.clone();
            async move {
                let n = iters.min(NO_POOL_MAX_ITERS);
                let start = std::time::Instant::now();
                for _ in 0..n {
                    client
                        .get(&u)
                        .unwrap()
                        .send()
                        .await
                        .unwrap()
                        .bytes()
                        .await
                        .unwrap();
                }
                let elapsed = start.elapsed();
                elapsed.mul_f64(iters as f64 / n as f64)
            }
        });
    });
    group.finish();
}

fn bench_h2_pool_vs_no_pool(c: &mut Criterion) {
    let url = h2c_small_url();

    let no_pool = aioduct::HttpEngineSend::<
        aioduct::runtime::TokioRuntime,
        aioduct::runtime::tokio_rt::TcpConnector,
    >::builder()
    .http2_prior_knowledge()
    .http2(
        aioduct::Http2Config::new()
            .initial_stream_window_size(2 * 1024 * 1024)
            .initial_connection_window_size(4 * 1024 * 1024)
            .max_concurrent_reset_streams(1024),
    )
    .no_connection_reuse()
    .build()
    .unwrap();

    let mut group = c.benchmark_group("e2e_pooling/h2");
    group.sample_size(10);
    group.warm_up_time(Duration::from_millis(500));
    group.measurement_time(Duration::from_secs(2));
    group.bench_function("with_pool", |b| {
        b.to_async(&*RT).iter(|| async {
            AIODUCT_H2
                .get(&url)
                .unwrap()
                .send()
                .await
                .unwrap()
                .bytes()
                .await
                .unwrap()
        });
    });
    group.bench_function("no_pool", |b| {
        b.to_async(&*RT).iter_custom(|iters| {
            let client = no_pool.clone();
            let u = url.clone();
            async move {
                let n = iters.min(NO_POOL_MAX_ITERS);
                let start = std::time::Instant::now();
                for _ in 0..n {
                    client
                        .get(&u)
                        .unwrap()
                        .send()
                        .await
                        .unwrap()
                        .bytes()
                        .await
                        .unwrap();
                }
                let elapsed = start.elapsed();
                elapsed.mul_f64(iters as f64 / n as f64)
            }
        });
    });
    group.finish();
}

criterion_group! {
    name = benches;
    config = Criterion::default().measurement_time(Duration::from_secs(5));
    targets =
        bench_h1_pool_vs_no_pool,
        bench_h2_pool_vs_no_pool,
}
