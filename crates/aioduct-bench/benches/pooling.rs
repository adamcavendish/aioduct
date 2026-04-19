use std::time::Duration;

use bytes::Bytes;
use criterion::{Criterion, criterion_group, criterion_main};
use tokio::runtime::Runtime;

use aioduct_bench::*;

fn bench_h1_pool_vs_no_pool(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let body = Bytes::from(JSON_BODY);
    let addr = rt.block_on(start_http1_server(body));
    let url = format!("http://{addr}/");

    let pooled = rt.block_on(async { aioduct::Client::<aioduct::runtime::TokioRuntime>::new() });
    let no_pool = rt.block_on(async {
        aioduct::Client::<aioduct::runtime::TokioRuntime>::builder()
            .no_connection_reuse()
            .build()
    });

    let mut group = c.benchmark_group("h1_connection_pool");
    group.bench_function("with_pool", |b| {
        b.to_async(&rt).iter(|| async {
            pooled
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
        b.to_async(&rt).iter(|| async {
            no_pool
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
    group.finish();
}

fn bench_h2_pool_vs_no_pool(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let body = Bytes::from(JSON_BODY);
    let addr = rt.block_on(start_h2c_server(body));
    let url = format!("http://{addr}/");

    let h2_config = aioduct::Http2Config::new()
        .initial_stream_window_size(2 * 1024 * 1024)
        .initial_connection_window_size(4 * 1024 * 1024)
        .max_concurrent_reset_streams(1024);
    let pooled = rt.block_on(async {
        aioduct::Client::<aioduct::runtime::TokioRuntime>::builder()
            .http2_prior_knowledge()
            .http2(h2_config.clone())
            .build()
    });
    let no_pool = rt.block_on(async {
        aioduct::Client::<aioduct::runtime::TokioRuntime>::builder()
            .http2_prior_knowledge()
            .http2(
                aioduct::Http2Config::new()
                    .initial_stream_window_size(2 * 1024 * 1024)
                    .initial_connection_window_size(4 * 1024 * 1024)
                    .max_concurrent_reset_streams(1024),
            )
            .no_connection_reuse()
            .build()
    });

    let mut group = c.benchmark_group("h2_connection_pool");
    group.bench_function("with_pool", |b| {
        b.to_async(&rt).iter(|| async {
            pooled
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
        b.to_async(&rt).iter(|| async {
            no_pool
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
    group.finish();
}

criterion_group! {
    name = pool_benches;
    config = Criterion::default().measurement_time(Duration::from_secs(5));
    targets =
        bench_h1_pool_vs_no_pool,
        bench_h2_pool_vs_no_pool,
}

criterion_main!(pool_benches);
