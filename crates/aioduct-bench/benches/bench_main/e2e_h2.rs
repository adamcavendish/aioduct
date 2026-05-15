use std::time::Duration;

use bytes::Bytes;
use criterion::{Criterion, Throughput, criterion_group};

use aioduct_bench::*;

use super::shared::*;

fn bench_h2_get(c: &mut Criterion) {
    let url = h2c_small_url();
    let mut group = c.benchmark_group("e2e_h2/get_small");
    group.bench_function("aioduct", |b| {
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
    group.bench_function("hyper_util", |b| {
        let uri: http::Uri = url.parse().unwrap();
        b.to_async(&*RT).iter(|| {
            let c = HYPER_UTIL_H2.clone();
            let u = uri.clone();
            async move {
                let resp = c.get(u).await.unwrap();
                http_body_util::BodyExt::collect(resp.into_body())
                    .await
                    .unwrap()
                    .to_bytes()
            }
        });
    });
    group.finish();
}

fn bench_h2_post_4k(c: &mut Criterion) {
    let url = h2c_echo_url();
    let payload = Bytes::from(vec![b'x'; 4096]);
    let mut group = c.benchmark_group("e2e_h2/post_4k");
    group.bench_function("aioduct", |b| {
        b.to_async(&*RT).iter(|| {
            let p = payload.clone();
            async {
                AIODUCT_H2
                    .post(&url)
                    .unwrap()
                    .body(p)
                    .send()
                    .await
                    .unwrap()
                    .bytes()
                    .await
                    .unwrap()
            }
        });
    });
    group.finish();
}

fn bench_h2_download_64k(c: &mut Criterion) {
    let url = h2c_64k_url();
    let mut group = c.benchmark_group("e2e_h2/download_64k");
    group.throughput(Throughput::Bytes(BODY_64K as u64));
    group.sample_size(50);
    group.bench_function("aioduct", |b| {
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
    group.bench_function("hyper_util", |b| {
        let uri: http::Uri = url.parse().unwrap();
        b.to_async(&*RT).iter(|| {
            let c = HYPER_UTIL_H2.clone();
            let u = uri.clone();
            async move {
                let resp = c.get(u).await.unwrap();
                http_body_util::BodyExt::collect(resp.into_body())
                    .await
                    .unwrap()
                    .to_bytes()
            }
        });
    });
    group.finish();
}

fn bench_h2_download_1m(c: &mut Criterion) {
    let url = h2c_1m_url();
    let mut group = c.benchmark_group("e2e_h2/download_1m");
    group.throughput(Throughput::Bytes(BODY_1M as u64));
    group.sample_size(30);
    group.bench_function("aioduct", |b| {
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
    group.finish();
}

criterion_group! {
    name = benches;
    config = Criterion::default().measurement_time(Duration::from_secs(5));
    targets =
        bench_h2_get,
        bench_h2_post_4k,
        bench_h2_download_64k,
        bench_h2_download_1m,
}
