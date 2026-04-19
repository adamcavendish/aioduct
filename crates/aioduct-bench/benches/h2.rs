use std::time::Duration;

use bytes::Bytes;
use criterion::{Criterion, criterion_group, criterion_main};
use http_body_util::Full;
use tokio::runtime::Runtime;

use aioduct_bench::*;

fn make_aioduct_h2_client(rt: &Runtime) -> aioduct::Client<aioduct::runtime::TokioRuntime> {
    rt.block_on(async {
        aioduct::Client::<aioduct::runtime::TokioRuntime>::builder()
            .http2_prior_knowledge()
            .http2(
                aioduct::Http2Config::new()
                    .initial_stream_window_size(2 * 1024 * 1024)
                    .initial_connection_window_size(4 * 1024 * 1024)
                    .max_concurrent_reset_streams(1024),
            )
            .build()
    })
}

fn make_hyper_util_h2_client(
) -> hyper_util::client::legacy::Client<hyper_util::client::legacy::connect::HttpConnector, Full<Bytes>>
{
    hyper_util::client::legacy::Client::builder(hyper_util::rt::TokioExecutor::new())
        .http2_only(true)
        .pool_idle_timeout(Duration::from_secs(90))
        .http2_initial_stream_window_size(2 * 1024 * 1024)
        .http2_initial_connection_window_size(4 * 1024 * 1024)
        .http2_max_concurrent_reset_streams(1024)
        .build_http::<Full<Bytes>>()
}

fn bench_h2_get(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let body = Bytes::from(JSON_BODY);
    let addr = rt.block_on(start_h2c_server(body));
    let url = format!("http://{addr}/");

    let aioduct_client = make_aioduct_h2_client(&rt);
    let hyper_util_client = make_hyper_util_h2_client();

    let mut group = c.benchmark_group("h2_get");
    group.bench_function("aioduct", |b| {
        b.to_async(&rt).iter(|| async {
            aioduct_client.get(&url).unwrap().send().await.unwrap().bytes().await.unwrap()
        });
    });
    group.bench_function("hyper_util", |b| {
        let url: http::Uri = url.parse().unwrap();
        b.to_async(&rt).iter(|| {
            let c = hyper_util_client.clone();
            let u = url.clone();
            async move {
                let resp = c.get(u).await.unwrap();
                http_body_util::BodyExt::collect(resp.into_body()).await.unwrap().to_bytes()
            }
        });
    });
    group.finish();
}

fn bench_h2_download_64k(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let body = Bytes::from(vec![b'x'; BODY_64K]);
    let addr = rt.block_on(start_h2c_server(body));
    let url = format!("http://{addr}/");

    let aioduct_client = make_aioduct_h2_client(&rt);
    let hyper_util_client = make_hyper_util_h2_client();

    let mut group = c.benchmark_group("h2_download_64k");
    group.sample_size(50);
    group.bench_function("aioduct", |b| {
        b.to_async(&rt).iter(|| async {
            aioduct_client.get(&url).unwrap().send().await.unwrap().bytes().await.unwrap()
        });
    });
    group.bench_function("hyper_util", |b| {
        let url: http::Uri = url.parse().unwrap();
        b.to_async(&rt).iter(|| {
            let c = hyper_util_client.clone();
            let u = url.clone();
            async move {
                let resp = c.get(u).await.unwrap();
                http_body_util::BodyExt::collect(resp.into_body()).await.unwrap().to_bytes()
            }
        });
    });
    group.finish();
}

fn bench_h2_download_1m(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let body = Bytes::from(vec![b'x'; BODY_1M]);
    let addr = rt.block_on(start_h2c_server(body));
    let url = format!("http://{addr}/");

    let aioduct_client = make_aioduct_h2_client(&rt);

    let mut group = c.benchmark_group("h2_download_1m");
    group.sample_size(30);
    group.bench_function("aioduct", |b| {
        b.to_async(&rt).iter(|| async {
            aioduct_client.get(&url).unwrap().send().await.unwrap().bytes().await.unwrap()
        });
    });
    group.finish();
}

fn bench_h2_concurrent_10(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let body = Bytes::from(JSON_BODY);
    let addr = rt.block_on(start_h2c_server(body));
    let url = format!("http://{addr}/");

    let aioduct_client = make_aioduct_h2_client(&rt);

    let mut group = c.benchmark_group("h2_concurrent_10");
    group.sample_size(50);
    group.bench_function("aioduct", |b| {
        b.to_async(&rt).iter(|| {
            let client = aioduct_client.clone();
            let url = url.clone();
            async move {
                let futs: Vec<_> = (0..10).map(|_| {
                    let c = client.clone();
                    let u = url.clone();
                    tokio::spawn(async move { c.get(&u).unwrap().send().await.unwrap().bytes().await.unwrap() })
                }).collect();
                for f in futs { f.await.unwrap(); }
            }
        });
    });
    group.finish();
}

fn bench_h2_post_4k(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let addr = rt.block_on(start_h2c_echo_server());
    let url = format!("http://{addr}/");

    let aioduct_client = make_aioduct_h2_client(&rt);
    let payload = Bytes::from(vec![b'x'; 4096]);

    let mut group = c.benchmark_group("h2_post_4k");
    group.bench_function("aioduct", |b| {
        b.to_async(&rt).iter(|| {
            let p = payload.clone();
            async {
                aioduct_client.post(&url).unwrap().body(p).send().await.unwrap().bytes().await.unwrap()
            }
        });
    });
    group.finish();
}

criterion_group! {
    name = h2_benches;
    config = Criterion::default().measurement_time(Duration::from_secs(5));
    targets =
        bench_h2_get,
        bench_h2_download_64k,
        bench_h2_download_1m,
        bench_h2_concurrent_10,
        bench_h2_post_4k,
}

criterion_main!(h2_benches);
