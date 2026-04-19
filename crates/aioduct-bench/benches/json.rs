use std::time::Duration;

use bytes::Bytes;
use criterion::{Criterion, criterion_group, criterion_main};
use tokio::runtime::Runtime;

use aioduct_bench::*;

fn bench_json_parse(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let body = Bytes::from(JSON_BODY);
    let (addr, aioduct_client) = rt.block_on(async {
        let addr = start_http1_server(body).await;
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
            aioduct_client.get(&url).unwrap().send().await.unwrap().json::<Msg>().await.unwrap()
        });
    });
    group.bench_function("reqwest", |b| {
        b.to_async(&rt).iter(|| async {
            reqwest_client.get(&url).send().await.unwrap().json::<Msg>().await.unwrap()
        });
    });
    group.finish();
}

criterion_group! {
    name = json_benches;
    config = Criterion::default().measurement_time(Duration::from_secs(5));
    targets = bench_json_parse,
}

criterion_main!(json_benches);
