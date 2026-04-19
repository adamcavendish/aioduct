use std::time::Duration;

use bytes::Bytes;
use criterion::{Criterion, criterion_group, criterion_main};
use tokio::runtime::Runtime;

use aioduct_bench::*;

fn bench_sse_consume(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let (addr, aioduct_client) = rt.block_on(async {
        let addr = start_sse_server(SSE_EVENT_COUNT).await;
        let client = aioduct::Client::<aioduct::runtime::TokioRuntime>::new();
        (addr, client)
    });
    let url = format!("http://{addr}/");

    let mut group = c.benchmark_group("sse_consume_100_events");
    group.sample_size(50);
    group.bench_function("aioduct", |b| {
        b.to_async(&rt).iter(|| async {
            let resp = aioduct_client.get(&url).unwrap().send().await.unwrap();
            let mut stream = resp.into_sse_stream();
            let mut count = 0;
            while let Some(Ok(_event)) = stream.next().await {
                count += 1;
            }
            assert_eq!(count, SSE_EVENT_COUNT);
        });
    });
    group.finish();
}

fn bench_multipart_small(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let (addr, aioduct_client) = rt.block_on(async {
        let addr = start_echo_server().await;
        let client = aioduct::Client::<aioduct::runtime::TokioRuntime>::new();
        (addr, client)
    });
    let url = format!("http://{addr}/");
    let reqwest_client = reqwest::Client::new();

    let mut group = c.benchmark_group("multipart_small");
    group.bench_function("aioduct", |b| {
        b.to_async(&rt).iter(|| async {
            let form = aioduct::Multipart::new()
                .text("field1", "value1")
                .text("field2", "value2");
            aioduct_client
                .post(&url)
                .unwrap()
                .multipart(form)
                .send()
                .await
                .unwrap()
                .bytes()
                .await
                .unwrap()
        });
    });
    group.bench_function("reqwest", |b| {
        b.to_async(&rt).iter(|| {
            let url = url.clone();
            let client = reqwest_client.clone();
            async move {
                let form = reqwest::multipart::Form::new()
                    .text("field1", "value1")
                    .text("field2", "value2");
                client
                    .post(&url)
                    .multipart(form)
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

fn bench_multipart_file_1m(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let (addr, aioduct_client) = rt.block_on(async {
        let addr = start_echo_server().await;
        let client = aioduct::Client::<aioduct::runtime::TokioRuntime>::new();
        (addr, client)
    });
    let url = format!("http://{addr}/");
    let file_data: Bytes = Bytes::from(vec![b'D'; BODY_1M]);
    let reqwest_client = reqwest::Client::new();

    let mut group = c.benchmark_group("multipart_file_1m");
    group.sample_size(30);
    group.bench_function("aioduct", |b| {
        b.to_async(&rt).iter(|| {
            let data = file_data.clone();
            async {
                let form = aioduct::Multipart::new()
                    .text("description", "large file")
                    .file("upload", "data.bin", "application/octet-stream", data);
                aioduct_client
                    .post(&url)
                    .unwrap()
                    .multipart(form)
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
            let data = file_data.clone();
            let url = url.clone();
            let client = reqwest_client.clone();
            async move {
                let part = reqwest::multipart::Part::bytes(data.to_vec())
                    .file_name("data.bin")
                    .mime_str("application/octet-stream")
                    .unwrap();
                let form = reqwest::multipart::Form::new()
                    .text("description", "large file")
                    .part("upload", part);
                client
                    .post(&url)
                    .multipart(form)
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

fn bench_upload_1m(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let (addr, aioduct_client) = rt.block_on(async {
        let addr = start_echo_server().await;
        let client = aioduct::Client::<aioduct::runtime::TokioRuntime>::new();
        (addr, client)
    });
    let url = format!("http://{addr}/");
    let payload = Bytes::from(vec![b'U'; BODY_1M]);
    let reqwest_client = reqwest::Client::new();

    let mut group = c.benchmark_group("upload_1m");
    group.sample_size(30);
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
    group.finish();
}

fn bench_chunk_download(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let total_size = BODY_1M;
    let (addr, aioduct_client) = rt.block_on(async {
        let addr = start_range_server(total_size).await;
        let client = aioduct::Client::<aioduct::runtime::TokioRuntime>::new();
        (addr, client)
    });
    let url = format!("http://{addr}/data");

    let mut group = c.benchmark_group("chunk_download_1m");
    group.sample_size(30);
    group.bench_function("1_chunk", |b| {
        b.to_async(&rt).iter(|| async {
            let result = aioduct_client
                .chunk_download(&url)
                .chunks(1)
                .download()
                .await
                .unwrap();
            assert_eq!(result.total_size as usize, total_size);
        });
    });
    group.bench_function("4_chunks", |b| {
        b.to_async(&rt).iter(|| async {
            let result = aioduct_client
                .chunk_download(&url)
                .chunks(4)
                .download()
                .await
                .unwrap();
            assert_eq!(result.total_size as usize, total_size);
        });
    });
    group.bench_function("8_chunks", |b| {
        b.to_async(&rt).iter(|| async {
            let result = aioduct_client
                .chunk_download(&url)
                .chunks(8)
                .download()
                .await
                .unwrap();
            assert_eq!(result.total_size as usize, total_size);
        });
    });
    group.bench_function("single_get_baseline", |b| {
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
    group.finish();
}

fn bench_body_stream(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let body = Bytes::from(vec![b'S'; BODY_64K]);
    let (addr, aioduct_client) = rt.block_on(async {
        let addr = start_http1_server(body).await;
        let client = aioduct::Client::<aioduct::runtime::TokioRuntime>::new();
        (addr, client)
    });
    let url = format!("http://{addr}/");

    let mut group = c.benchmark_group("body_stream_64k");
    group.sample_size(50);
    group.bench_function("bytes_collect", |b| {
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
    group.bench_function("frame_by_frame", |b| {
        b.to_async(&rt).iter(|| async {
            let resp = aioduct_client.get(&url).unwrap().send().await.unwrap();
            let mut stream = resp.into_bytes_stream();
            let mut total = 0usize;
            while let Some(Ok(chunk)) = stream.next().await {
                total += chunk.len();
            }
            assert_eq!(total, BODY_64K);
        });
    });
    group.finish();
}

criterion_group! {
    name = feature_benches;
    config = Criterion::default().measurement_time(Duration::from_secs(5));
    targets =
        bench_sse_consume,
        bench_multipart_small,
        bench_multipart_file_1m,
        bench_upload_1m,
        bench_chunk_download,
        bench_body_stream,
}

criterion_main!(feature_benches);
