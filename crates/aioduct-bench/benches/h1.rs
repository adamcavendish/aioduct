use std::time::Duration;

use bytes::Bytes;
use criterion::{Criterion, criterion_group, criterion_main};
use http_body_util::Full;
use tokio::runtime::Runtime;

use aioduct_bench::*;

fn bench_h1_get(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let body = Bytes::from(JSON_BODY);
    let (addr, aioduct_client) = rt.block_on(async {
        let addr = start_http1_server(body).await;
        let client = aioduct::Client::<aioduct::runtime::TokioRuntime>::new();
        (addr, client)
    });
    let url = format!("http://{addr}/");
    let reqwest_client = reqwest::Client::new();
    let hyper_util_client =
        hyper_util::client::legacy::Client::builder(hyper_util::rt::TokioExecutor::new())
            .build_http::<Full<Bytes>>();
    let isahc_client = isahc::HttpClient::new().unwrap();

    let mut group = c.benchmark_group("h1_get");
    group.bench_function("aioduct", |b| {
        b.to_async(&rt).iter(|| async {
            aioduct_client
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
    group.bench_function("reqwest", |b| {
        b.to_async(&rt).iter(|| async {
            reqwest_client
                .get(&url)
                .send()
                .await
                .unwrap()
                .bytes()
                .await
                .unwrap()
        });
    });
    group.bench_function("hyper_util", |b| {
        let url: http::Uri = url.parse().unwrap();
        b.to_async(&rt).iter(|| {
            let c = hyper_util_client.clone();
            let u = url.clone();
            async move {
                let resp = c.get(u).await.unwrap();
                http_body_util::BodyExt::collect(resp.into_body())
                    .await
                    .unwrap()
                    .to_bytes()
            }
        });
    });
    group.bench_function("isahc", |b| {
        b.to_async(&rt).iter(|| {
            let c = isahc_client.clone();
            let url = url.clone();
            async move {
                let mut resp = c.get_async(&url).await.unwrap();
                isahc::AsyncReadResponseExt::bytes(&mut resp).await.unwrap()
            }
        });
    });
    group.finish();
}

fn bench_h1_get_text(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let body = Bytes::from(JSON_BODY);
    let (addr, aioduct_client) = rt.block_on(async {
        let addr = start_http1_server(body).await;
        let client = aioduct::Client::<aioduct::runtime::TokioRuntime>::new();
        (addr, client)
    });
    let url = format!("http://{addr}/");
    let reqwest_client = reqwest::Client::new();

    let mut group = c.benchmark_group("h1_get_text");
    group.bench_function("aioduct", |b| {
        b.to_async(&rt).iter(|| async {
            aioduct_client
                .get(&url)
                .unwrap()
                .send()
                .await
                .unwrap()
                .text()
                .await
                .unwrap()
        });
    });
    group.bench_function("reqwest", |b| {
        b.to_async(&rt).iter(|| async {
            reqwest_client
                .get(&url)
                .send()
                .await
                .unwrap()
                .text()
                .await
                .unwrap()
        });
    });
    group.finish();
}

fn bench_h1_post_4k(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let (addr, aioduct_client) = rt.block_on(async {
        let addr = start_echo_server().await;
        let client = aioduct::Client::<aioduct::runtime::TokioRuntime>::new();
        (addr, client)
    });
    let url = format!("http://{addr}/");
    let payload = Bytes::from(vec![b'x'; 4096]);
    let reqwest_client = reqwest::Client::new();
    let isahc_client = isahc::HttpClient::new().unwrap();

    let mut group = c.benchmark_group("h1_post_4k");
    group.bench_function("aioduct", |b| {
        b.to_async(&rt).iter(|| {
            let p = payload.clone();
            async {
                aioduct_client
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
    group.bench_function("reqwest", |b| {
        b.to_async(&rt).iter(|| {
            let p = payload.clone();
            let url = url.clone();
            let client = reqwest_client.clone();
            async move {
                client
                    .post(&url)
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
    group.bench_function("isahc", |b| {
        b.to_async(&rt).iter(|| {
            let c = isahc_client.clone();
            let url = url.clone();
            let p = payload.clone();
            async move {
                let mut resp = c.post_async(&url, p.to_vec()).await.unwrap();
                isahc::AsyncReadResponseExt::bytes(&mut resp).await.unwrap()
            }
        });
    });
    group.finish();
}

fn bench_h1_download_64k(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let body = Bytes::from(vec![b'x'; BODY_64K]);
    let (addr, aioduct_client) = rt.block_on(async {
        let addr = start_http1_server(body).await;
        let client = aioduct::Client::<aioduct::runtime::TokioRuntime>::new();
        (addr, client)
    });
    let url = format!("http://{addr}/");
    let reqwest_client = reqwest::Client::new();
    let hyper_util_client =
        hyper_util::client::legacy::Client::builder(hyper_util::rt::TokioExecutor::new())
            .build_http::<Full<Bytes>>();

    let mut group = c.benchmark_group("h1_download_64k");
    group.sample_size(50);
    group.bench_function("aioduct", |b| {
        b.to_async(&rt).iter(|| async {
            aioduct_client
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
    group.bench_function("reqwest", |b| {
        b.to_async(&rt).iter(|| async {
            reqwest_client
                .get(&url)
                .send()
                .await
                .unwrap()
                .bytes()
                .await
                .unwrap()
        });
    });
    group.bench_function("hyper_util", |b| {
        let url: http::Uri = url.parse().unwrap();
        b.to_async(&rt).iter(|| {
            let c = hyper_util_client.clone();
            let u = url.clone();
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

fn bench_h1_download_1m(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let body = Bytes::from(vec![b'x'; BODY_1M]);
    let (addr, aioduct_client) = rt.block_on(async {
        let addr = start_http1_server(body).await;
        let client = aioduct::Client::<aioduct::runtime::TokioRuntime>::new();
        (addr, client)
    });
    let url = format!("http://{addr}/");
    let reqwest_client = reqwest::Client::new();

    let mut group = c.benchmark_group("h1_download_1m");
    group.sample_size(30);
    group.bench_function("aioduct", |b| {
        b.to_async(&rt).iter(|| async {
            aioduct_client
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
    group.bench_function("reqwest", |b| {
        b.to_async(&rt).iter(|| async {
            reqwest_client
                .get(&url)
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

fn bench_h1_concurrent_10(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let body = Bytes::from(JSON_BODY);
    let (addr, aioduct_client) = rt.block_on(async {
        let addr = start_http1_server(body).await;
        let client = aioduct::Client::<aioduct::runtime::TokioRuntime>::new();
        (addr, client)
    });
    let url = format!("http://{addr}/");
    let reqwest_client = reqwest::Client::new();

    let mut group = c.benchmark_group("h1_concurrent_10");
    group.sample_size(50);
    group.bench_function("aioduct", |b| {
        b.to_async(&rt).iter(|| {
            let client = aioduct_client.clone();
            let url = url.clone();
            async move {
                let futs: Vec<_> = (0..10)
                    .map(|_| {
                        let c = client.clone();
                        let u = url.clone();
                        tokio::spawn(async move {
                            c.get(&u)
                                .unwrap()
                                .send()
                                .await
                                .unwrap()
                                .bytes()
                                .await
                                .unwrap()
                        })
                    })
                    .collect();
                for f in futs {
                    f.await.unwrap();
                }
            }
        });
    });
    group.bench_function("reqwest", |b| {
        b.to_async(&rt).iter(|| {
            let client = reqwest_client.clone();
            let url = url.clone();
            async move {
                let futs: Vec<_> = (0..10)
                    .map(|_| {
                        let c = client.clone();
                        let u = url.clone();
                        tokio::spawn(async move {
                            c.get(&u).send().await.unwrap().bytes().await.unwrap()
                        })
                    })
                    .collect();
                for f in futs {
                    f.await.unwrap();
                }
            }
        });
    });
    group.finish();
}

fn bench_h1_concurrent_50(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let body = Bytes::from(JSON_BODY);
    let (addr, aioduct_client) = rt.block_on(async {
        let addr = start_http1_server(body).await;
        let client = aioduct::Client::<aioduct::runtime::TokioRuntime>::builder()
            .pool_max_idle_per_host(100)
            .build();
        (addr, client)
    });
    let url = format!("http://{addr}/");
    let reqwest_client = reqwest::Client::new();

    let mut group = c.benchmark_group("h1_concurrent_50");
    group.sample_size(30);
    group.bench_function("aioduct", |b| {
        b.to_async(&rt).iter(|| {
            let client = aioduct_client.clone();
            let url = url.clone();
            async move {
                let futs: Vec<_> = (0..50)
                    .map(|_| {
                        let c = client.clone();
                        let u = url.clone();
                        tokio::spawn(async move {
                            c.get(&u)
                                .unwrap()
                                .send()
                                .await
                                .unwrap()
                                .bytes()
                                .await
                                .unwrap()
                        })
                    })
                    .collect();
                for f in futs {
                    f.await.unwrap();
                }
            }
        });
    });
    group.bench_function("reqwest", |b| {
        b.to_async(&rt).iter(|| {
            let client = reqwest_client.clone();
            let url = url.clone();
            async move {
                let futs: Vec<_> = (0..50)
                    .map(|_| {
                        let c = client.clone();
                        let u = url.clone();
                        tokio::spawn(async move {
                            c.get(&u).send().await.unwrap().bytes().await.unwrap()
                        })
                    })
                    .collect();
                for f in futs {
                    f.await.unwrap();
                }
            }
        });
    });
    group.finish();
}

criterion_group! {
    name = h1_benches;
    config = Criterion::default().measurement_time(Duration::from_secs(5));
    targets =
        bench_h1_get,
        bench_h1_get_text,
        bench_h1_post_4k,
        bench_h1_download_64k,
        bench_h1_download_1m,
        bench_h1_concurrent_10,
        bench_h1_concurrent_50,
}

criterion_main!(h1_benches);
