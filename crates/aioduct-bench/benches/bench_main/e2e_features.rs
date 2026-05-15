use std::time::Duration;

use bytes::Bytes;
use criterion::{Criterion, Throughput, criterion_group};

use aioduct_bench::*;

use super::shared::*;

fn bench_sse_consume(c: &mut Criterion) {
    let url = sse_url();
    let mut group = c.benchmark_group("e2e_features/sse_100");
    group.sample_size(50);
    group.bench_function("aioduct", |b| {
        b.to_async(&*RT).iter(|| async {
            let resp = AIODUCT_H1.get(&url).unwrap().send().await.unwrap();
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
    let url = h1_echo_url();
    let mut group = c.benchmark_group("e2e_features/multipart_small");
    group.bench_function("aioduct", |b| {
        b.to_async(&*RT).iter(|| async {
            let form = aioduct::Multipart::new()
                .text("field1", "value1")
                .text("field2", "value2");
            AIODUCT_H1
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
        b.to_async(&*RT).iter(|| {
            let url = url.clone();
            async move {
                let form = reqwest::multipart::Form::new()
                    .text("field1", "value1")
                    .text("field2", "value2");
                REQWEST
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
    let url = h1_echo_url();
    let file_data = Bytes::from(vec![b'D'; BODY_1M]);
    let mut group = c.benchmark_group("e2e_features/multipart_file_1m");
    group.throughput(Throughput::Bytes(BODY_1M as u64));
    group.sample_size(30);
    group.bench_function("aioduct", |b| {
        b.to_async(&*RT).iter(|| {
            let data = file_data.clone();
            async {
                let form = aioduct::Multipart::new()
                    .text("description", "large file")
                    .file("upload", "data.bin", "application/octet-stream", data);
                AIODUCT_H1
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
        b.to_async(&*RT).iter(|| {
            let data = file_data.clone();
            let url = url.clone();
            async move {
                let part = reqwest::multipart::Part::bytes(data.to_vec())
                    .file_name("data.bin")
                    .mime_str("application/octet-stream")
                    .unwrap();
                let form = reqwest::multipart::Form::new()
                    .text("description", "large file")
                    .part("upload", part);
                REQWEST
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
    let url = h1_echo_url();
    let payload = Bytes::from(vec![b'U'; BODY_1M]);
    let mut group = c.benchmark_group("e2e_features/upload_1m");
    group.throughput(Throughput::Bytes(BODY_1M as u64));
    group.sample_size(30);
    group.bench_function("aioduct", |b| {
        b.to_async(&*RT).iter(|| {
            let p = payload.clone();
            async {
                AIODUCT_H1
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
        b.to_async(&*RT).iter(|| {
            let p = payload.clone();
            let url = url.clone();
            async move {
                REQWEST
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
    let url = range_url();
    let mut group = c.benchmark_group("e2e_features/chunk_download_1m");
    group.throughput(Throughput::Bytes(BODY_1M as u64));
    group.sample_size(30);
    for chunks in [1, 4, 8] {
        group.bench_with_input(
            criterion::BenchmarkId::new("chunks", chunks),
            &chunks,
            |b, &n| {
                b.to_async(&*RT).iter(|| async {
                    let result = AIODUCT_H1
                        .chunk_download(&url)
                        .chunks(n)
                        .download()
                        .await
                        .unwrap();
                    assert_eq!(result.total_size as usize, BODY_1M);
                });
            },
        );
    }
    group.bench_function("single_get_baseline", |b| {
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
    group.finish();
}

fn bench_body_stream(c: &mut Criterion) {
    let url = h1_64k_url();
    let mut group = c.benchmark_group("e2e_features/body_stream_64k");
    group.throughput(Throughput::Bytes(BODY_64K as u64));
    group.sample_size(50);
    group.bench_function("bytes_collect", |b| {
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
    group.bench_function("frame_by_frame", |b| {
        b.to_async(&*RT).iter(|| async {
            let resp = AIODUCT_H1.get(&url).unwrap().send().await.unwrap();
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

fn bench_json_parse(c: &mut Criterion) {
    let url = h1_small_url();

    #[derive(serde::Deserialize)]
    struct Msg {
        #[allow(dead_code)]
        message: String,
        #[allow(dead_code)]
        count: u64,
    }

    let mut group = c.benchmark_group("e2e_features/json_parse");
    group.bench_function("aioduct", |b| {
        b.to_async(&*RT).iter(|| async {
            AIODUCT_H1
                .get(&url)
                .unwrap()
                .send()
                .await
                .unwrap()
                .json::<Msg>()
                .await
                .unwrap()
        });
    });
    group.bench_function("reqwest", |b| {
        b.to_async(&*RT).iter(|| async {
            REQWEST
                .get(&url)
                .send()
                .await
                .unwrap()
                .json::<Msg>()
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
        bench_sse_consume,
        bench_multipart_small,
        bench_multipart_file_1m,
        bench_upload_1m,
        bench_chunk_download,
        bench_body_stream,
        bench_json_parse,
}
