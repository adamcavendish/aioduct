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

fn bench_coalescing_checkout(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();

    let mut group = c.benchmark_group("coalescing_checkout_scan");

    for pool_size in [10, 50, 200, 1000] {
        group.bench_with_input(
            criterion::BenchmarkId::new("connections", pool_size),
            &pool_size,
            |b, &size| {
                let pool = rt.block_on(async {
                    let p = aioduct::__bench::new_pool(size, Duration::from_secs(60));
                    for i in 0..size {
                        let mut conn = aioduct::__bench::make_h2_conn().await;
                        aioduct::__bench::set_sans(
                            &mut conn,
                            vec![
                                format!("a{i}.example.com"),
                                format!("b{i}.example.com"),
                                format!("c{i}.example.com"),
                                format!("d{i}.example.com"),
                                format!("e{i}.example.com"),
                            ],
                        );
                        aioduct::__bench::set_remote_addr(
                            &mut conn,
                            std::net::SocketAddr::from(([10, 0, 0, 1], 443)),
                        );
                        let key = aioduct::__bench::pool_key(
                            &format!("origin{i}.example.com:443"),
                        );
                        aioduct::__bench::checkin(&p, key, conn);
                    }
                    p
                });

                // Worst case: target SAN is in the last connection checked
                let target = format!("e{}.example.com", size - 1);
                let ip: std::net::IpAddr = [10, 0, 0, 1].into();
                b.iter(|| {
                    std::hint::black_box(
                        aioduct::__bench::checkout_coalesced(&pool, &target, Some(ip)),
                    );
                });
            },
        );
    }
    group.finish();
}

criterion_group! {
    name = pool_benches;
    config = Criterion::default().measurement_time(Duration::from_secs(5));
    targets =
        bench_h1_pool_vs_no_pool,
        bench_h2_pool_vs_no_pool,
        bench_coalescing_checkout,
}

criterion_main!(pool_benches);
