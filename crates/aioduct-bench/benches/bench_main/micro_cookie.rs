use std::time::Duration;

use criterion::{Criterion, criterion_group};
use http::header::{HeaderValue, SET_COOKIE};

fn bench_apply_to_request(c: &mut Criterion) {
    let mut group = c.benchmark_group("micro_cookie/apply_to_request");

    for count in [0, 10, 100] {
        group.bench_with_input(
            criterion::BenchmarkId::new("cookies", count),
            &count,
            |b, &n| {
                let jar = aioduct::CookieJar::new();
                let mut setup_headers = http::HeaderMap::new();
                for i in 0..n {
                    setup_headers.append(
                        SET_COOKIE,
                        HeaderValue::from_str(&format!(
                            "cookie{i}=value{i}; Path=/; Domain=example.com"
                        ))
                        .unwrap(),
                    );
                }
                jar.store_from_response("example.com", "/", &setup_headers);

                b.iter(|| {
                    let mut headers = http::HeaderMap::new();
                    jar.apply_to_request("example.com", true, "/", &mut headers);
                    std::hint::black_box(&headers);
                });
            },
        );
    }
    group.finish();
}

fn bench_store_from_response(c: &mut Criterion) {
    let mut group = c.benchmark_group("micro_cookie/store_from_response");
    group.bench_function("10_cookies", |b| {
        let jar = aioduct::CookieJar::new();
        let mut headers = http::HeaderMap::new();
        for i in 0..10 {
            headers.append(
                SET_COOKIE,
                HeaderValue::from_str(&format!(
                    "sess{i}=val{i}; Path=/app; Domain=example.com; Secure; HttpOnly; Max-Age=3600"
                ))
                .unwrap(),
            );
        }
        b.iter(|| {
            jar.store_from_response("example.com", "/", &headers);
        });
    });
    group.finish();
}

criterion_group! {
    name = benches;
    config = Criterion::default().measurement_time(Duration::from_secs(5));
    targets =
        bench_apply_to_request,
        bench_store_from_response,
}
