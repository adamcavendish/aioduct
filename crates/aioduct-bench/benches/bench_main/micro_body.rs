use std::task::Context;
use std::time::Duration;

use criterion::{Criterion, criterion_group};
use http_body::Body;

use super::shared::*;

const BODY_SIZE: usize = 64 * 1024;

fn drain_body(body: &mut std::pin::Pin<Box<aioduct::body::RequestBoxBody>>, cx: &mut Context<'_>) {
    loop {
        match body.as_mut().poll_frame(cx) {
            std::task::Poll::Ready(Some(Ok(_))) => continue,
            std::task::Poll::Ready(None) => break,
            _ => break,
        }
    }
}

fn bench_body_transform_layers(c: &mut Criterion) {
    let mut group = c.benchmark_group("micro_body/poll_frame");

    group.bench_function("layers/0", |b| {
        b.iter(|| {
            let body = aioduct::__bench::make_full_body(BODY_SIZE);
            let mut pinned = Box::pin(body);
            let mut cx = Context::from_waker(std::task::Waker::noop());
            drain_body(&mut pinned, &mut cx);
        });
    });

    group.bench_function("layers/1_read_timeout", |b| {
        b.to_async(&*RT).iter(|| async {
            let body = aioduct::__bench::make_full_body(BODY_SIZE);
            let body = aioduct::__bench::wrap_read_timeout_body(body, Duration::from_secs(10));
            let mut pinned = Box::pin(body);
            let mut cx = Context::from_waker(std::task::Waker::noop());
            drain_body(&mut pinned, &mut cx);
        });
    });

    group.bench_function("layers/2_read_timeout_bandwidth", |b| {
        b.to_async(&*RT).iter(|| async {
            let body = aioduct::__bench::make_full_body(BODY_SIZE);
            let body = aioduct::__bench::wrap_read_timeout_body(body, Duration::from_secs(10));
            let limiter = aioduct::BandwidthLimiter::new(u64::MAX);
            let body = aioduct::__bench::wrap_bandwidth_body(body, limiter);
            let mut pinned = Box::pin(body);
            let mut cx = Context::from_waker(std::task::Waker::noop());
            drain_body(&mut pinned, &mut cx);
        });
    });

    group.finish();
}

criterion_group! {
    name = benches;
    config = Criterion::default().measurement_time(Duration::from_secs(5));
    targets =
        bench_body_transform_layers,
}
