use std::convert::Infallible;
use std::net::SocketAddr;

use bytes::Bytes;
use criterion::{Criterion, criterion_group, criterion_main};
use http_body_util::Full;
use hyper::server::conn::http1 as server_http1;
use hyper::service::service_fn;
use hyper::{Request, Response};
use tokio::net::TcpListener;
use tokio::runtime::Runtime;

async fn start_server() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let io = aioduct::runtime::tokio_rt::TokioIo::new(stream);
            tokio::spawn(async move {
                let _ = server_http1::Builder::new()
                    .serve_connection(
                        io,
                        service_fn(|_req: Request<hyper::body::Incoming>| async move {
                            Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(
                                r#"{"message":"hello","count":42}"#,
                            ))))
                        }),
                    )
                    .await;
            });
        }
    });

    addr
}

fn bench_single_get(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let (addr, aioduct_client) = rt.block_on(async {
        let addr = start_server().await;
        let client = aioduct::Client::<aioduct::runtime::TokioRuntime>::new();
        (addr, client)
    });
    let url = format!("http://{addr}/");
    let reqwest_client = reqwest::Client::new();

    let mut group = c.benchmark_group("single_get");

    group.bench_function("aioduct", |b| {
        b.to_async(&rt).iter(|| async {
            let resp = aioduct_client.get(&url).unwrap().send().await.unwrap();
            resp.bytes().await.unwrap()
        });
    });

    group.bench_function("reqwest", |b| {
        b.to_async(&rt).iter(|| async {
            let resp = reqwest_client.get(&url).send().await.unwrap();
            resp.bytes().await.unwrap()
        });
    });

    group.finish();
}

fn bench_single_get_text(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let (addr, aioduct_client) = rt.block_on(async {
        let addr = start_server().await;
        let client = aioduct::Client::<aioduct::runtime::TokioRuntime>::new();
        (addr, client)
    });
    let url = format!("http://{addr}/");
    let reqwest_client = reqwest::Client::new();

    let mut group = c.benchmark_group("single_get_text");

    group.bench_function("aioduct", |b| {
        b.to_async(&rt).iter(|| async {
            let resp = aioduct_client.get(&url).unwrap().send().await.unwrap();
            resp.text().await.unwrap()
        });
    });

    group.bench_function("reqwest", |b| {
        b.to_async(&rt).iter(|| async {
            let resp = reqwest_client.get(&url).send().await.unwrap();
            resp.text().await.unwrap()
        });
    });

    group.finish();
}

fn bench_json_parse(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let (addr, aioduct_client) = rt.block_on(async {
        let addr = start_server().await;
        let client = aioduct::Client::<aioduct::runtime::TokioRuntime>::new();
        (addr, client)
    });
    let url = format!("http://{addr}/");
    let reqwest_client = reqwest::Client::new();

    #[derive(serde::Deserialize)]
    struct Msg {
        #[allow(dead_code)]
        message: String,
        #[allow(dead_code)]
        count: u64,
    }

    let mut group = c.benchmark_group("json_parse");

    group.bench_function("aioduct", |b| {
        b.to_async(&rt).iter(|| async {
            let resp = aioduct_client.get(&url).unwrap().send().await.unwrap();
            resp.json::<Msg>().await.unwrap()
        });
    });

    group.bench_function("reqwest", |b| {
        b.to_async(&rt).iter(|| async {
            let resp = reqwest_client.get(&url).send().await.unwrap();
            resp.json::<Msg>().await.unwrap()
        });
    });

    group.finish();
}

fn bench_concurrent_requests(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let (addr, aioduct_client) = rt.block_on(async {
        let addr = start_server().await;
        let client = aioduct::Client::<aioduct::runtime::TokioRuntime>::new();
        (addr, client)
    });
    let url = format!("http://{addr}/");
    let reqwest_client = reqwest::Client::new();

    let mut group = c.benchmark_group("concurrent_10");
    group.sample_size(50);

    group.bench_function("aioduct", |b| {
        let url = url.clone();
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
        let url = url.clone();
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

fn bench_post_body(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let (addr, aioduct_client) = rt.block_on(async {
        let addr = start_server().await;
        let client = aioduct::Client::<aioduct::runtime::TokioRuntime>::new();
        (addr, client)
    });
    let url = format!("http://{addr}/");
    let payload = "x".repeat(4096);
    let reqwest_client = reqwest::Client::new();

    let mut group = c.benchmark_group("post_4k_body");

    group.bench_function("aioduct", |b| {
        let payload = payload.clone();
        b.to_async(&rt).iter(|| {
            let client = aioduct_client.clone();
            let url = url.clone();
            let body = payload.clone();
            async move {
                client
                    .post(&url)
                    .unwrap()
                    .body(body)
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
        let payload = payload.clone();
        b.to_async(&rt).iter(|| {
            let client = reqwest_client.clone();
            let url = url.clone();
            let body = payload.clone();
            async move {
                client
                    .post(&url)
                    .body(body)
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

criterion_group!(
    benches,
    bench_single_get,
    bench_single_get_text,
    bench_json_parse,
    bench_concurrent_requests,
    bench_post_body,
);
criterion_main!(benches);
