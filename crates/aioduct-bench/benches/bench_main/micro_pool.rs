use std::time::Duration;

use criterion::{Criterion, criterion_group};

use super::shared::*;

fn bench_checkout_checkin_roundtrip(c: &mut Criterion) {
    let mut group = c.benchmark_group("micro_pool/checkout_checkin");
    group.bench_function("roundtrip", |b| {
        let pool = RT.block_on(async {
            let p = aioduct::__bench::new_pool(1, Duration::from_secs(60));
            let conn = aioduct::__bench::make_h2_conn().await;
            let key = aioduct::__bench::pool_key("origin.example.com:443");
            aioduct::__bench::checkin(&p, key, conn);
            p
        });
        let key = aioduct::__bench::pool_key("origin.example.com:443");
        b.iter(|| {
            if let Some(conn) = aioduct::__bench::checkout(&pool, &key) {
                let key = aioduct::__bench::pool_key("origin.example.com:443");
                aioduct::__bench::checkin(&pool, key, conn);
            }
        });
    });
    group.finish();
}

fn bench_coalescing_checkout(c: &mut Criterion) {
    let mut group = c.benchmark_group("micro_pool/coalescing_scan");

    for pool_size in [10, 50, 200, 1000] {
        group.bench_with_input(
            criterion::BenchmarkId::new("connections", pool_size),
            &pool_size,
            |b, &size| {
                let pool = RT.block_on(async {
                    let p = aioduct::__bench::new_pool(size, Duration::from_secs(60));
                    for i in 0..size {
                        let mut conn = aioduct::__bench::make_h2_conn().await;
                        aioduct::__bench::set_sans(
                            &mut conn,
                            vec![
                                format!("a{i}.example.com"),
                                format!("b{i}.example.com"),
                                format!("c{i}.example.com"),
                                format!("d{i}.example.com"),
                                format!("e{i}.example.com"),
                            ],
                        );
                        aioduct::__bench::set_remote_addr(
                            &mut conn,
                            std::net::SocketAddr::from(([10, 0, 0, 1], 443)),
                        );
                        let key = aioduct::__bench::pool_key(&format!("origin{i}.example.com:443"));
                        aioduct::__bench::checkin(&p, key, conn);
                    }
                    p
                });

                let target = format!("e{}.example.com", size - 1);
                let addr = std::net::SocketAddr::from(([10, 0, 0, 1], 443));
                b.iter(|| {
                    std::hint::black_box(aioduct::__bench::checkout_coalesced(
                        &pool, &target, addr,
                    ));
                });
            },
        );
    }
    group.finish();
}

criterion_group! {
    name = benches;
    config = Criterion::default().measurement_time(Duration::from_secs(5));
    targets =
        bench_checkout_checkin_roundtrip,
        bench_coalescing_checkout,
}
