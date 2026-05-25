use criterion::{Criterion, criterion_group};

use super::shared::*;

// ── Small body ───────────────────────────────────────────────────────────────

fn bench_runtime_get_small(c: &mut Criterion) {
    let url = h1_small_url();
    let mut group = c.benchmark_group("runtime/get_small");
    group.bench_function("tokio", |b| {
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
    group.bench_function("smol", |b| {
        let client = aioduct::SmolClient::new();
        b.iter(|| {
            smol::block_on(async {
                client
                    .get(&url)
                    .unwrap()
                    .send()
                    .await
                    .unwrap()
                    .bytes()
                    .await
                    .unwrap()
            })
        });
    });
    group.bench_function("compio", |b| {
        let client = aioduct::CompioClient::new();
        let rt = compio_runtime::Runtime::new().unwrap();
        b.iter(|| {
            rt.block_on(async {
                client
                    .get_local(&url)
                    .unwrap()
                    .send()
                    .await
                    .unwrap()
                    .bytes()
                    .await
                    .unwrap()
            });
        });
    });
    group.finish();
}

// ── 64K body ─────────────────────────────────────────────────────────────────

fn bench_runtime_get_64k(c: &mut Criterion) {
    let url = h1_64k_url();
    let mut group = c.benchmark_group("runtime/get_64k");
    group.bench_function("tokio", |b| {
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
    group.bench_function("smol", |b| {
        let client = aioduct::SmolClient::new();
        b.iter(|| {
            smol::block_on(async {
                client
                    .get(&url)
                    .unwrap()
                    .send()
                    .await
                    .unwrap()
                    .bytes()
                    .await
                    .unwrap()
            })
        });
    });
    group.bench_function("compio", |b| {
        let client = aioduct::CompioClient::new();
        let rt = compio_runtime::Runtime::new().unwrap();
        b.iter(|| {
            rt.block_on(async {
                client
                    .get_local(&url)
                    .unwrap()
                    .send()
                    .await
                    .unwrap()
                    .bytes()
                    .await
                    .unwrap()
            });
        });
    });
    group.finish();
}

// ── 1M body ──────────────────────────────────────────────────────────────────

fn bench_runtime_get_1m(c: &mut Criterion) {
    let url = h1_1m_url();
    let mut group = c.benchmark_group("runtime/get_1m");
    group.bench_function("tokio", |b| {
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
    group.bench_function("smol", |b| {
        let client = aioduct::SmolClient::new();
        b.iter(|| {
            smol::block_on(async {
                client
                    .get(&url)
                    .unwrap()
                    .send()
                    .await
                    .unwrap()
                    .bytes()
                    .await
                    .unwrap()
            })
        });
    });
    group.bench_function("compio", |b| {
        let client = aioduct::CompioClient::new();
        let rt = compio_runtime::Runtime::new().unwrap();
        b.iter(|| {
            rt.block_on(async {
                client
                    .get_local(&url)
                    .unwrap()
                    .send()
                    .await
                    .unwrap()
                    .bytes()
                    .await
                    .unwrap()
            });
        });
    });
    group.finish();
}

criterion_group!(
    benches,
    bench_runtime_get_small,
    bench_runtime_get_64k,
    bench_runtime_get_1m,
);
