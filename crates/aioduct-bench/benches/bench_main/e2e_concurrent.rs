use std::time::Duration;

use criterion::{Criterion, criterion_group};

use super::shared::*;

fn bench_h1_concurrent_10(c: &mut Criterion) {
    let url = h1_small_url();
    let mut group = c.benchmark_group("e2e_concurrent/h1_x10");
    group.sample_size(50);
    group.bench_function("aioduct", |b| {
        b.to_async(&*RT).iter(|| {
            let client = AIODUCT_H1.clone();
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
        b.to_async(&*RT).iter(|| {
            let client = REQWEST.clone();
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
    let url = h1_small_url();
    let mut group = c.benchmark_group("e2e_concurrent/h1_x50");
    group.sample_size(30);
    group.bench_function("aioduct", |b| {
        b.to_async(&*RT).iter(|| {
            let client = AIODUCT_H1_LARGE_POOL.clone();
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
        b.to_async(&*RT).iter(|| {
            let client = REQWEST.clone();
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

fn bench_h2_concurrent_10(c: &mut Criterion) {
    let url = h2c_small_url();
    let mut group = c.benchmark_group("e2e_concurrent/h2_x10");
    group.sample_size(50);
    group.bench_function("aioduct", |b| {
        b.to_async(&*RT).iter(|| {
            let client = AIODUCT_H2.clone();
            let url = url.clone();
            async move {
                let futs: Vec<_> = (0..10)
                    .map(|_| {
                        let c = client.clone();
                        let u = url.clone();
                        tokio::spawn(async move {
                            c.get(&u)
                                .unwrap()
                                .h2c_prior_knowledge()
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
    group.finish();
}

fn bench_h2_concurrent_50(c: &mut Criterion) {
    let url = h2c_small_url();
    let mut group = c.benchmark_group("e2e_concurrent/h2_x50");
    group.sample_size(30);
    group.bench_function("aioduct", |b| {
        b.to_async(&*RT).iter(|| {
            let client = AIODUCT_H2.clone();
            let url = url.clone();
            async move {
                let futs: Vec<_> = (0..50)
                    .map(|_| {
                        let c = client.clone();
                        let u = url.clone();
                        tokio::spawn(async move {
                            c.get(&u)
                                .unwrap()
                                .h2c_prior_knowledge()
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
    group.finish();
}

criterion_group! {
    name = benches;
    config = Criterion::default().measurement_time(Duration::from_secs(5));
    targets =
        bench_h1_concurrent_10,
        bench_h1_concurrent_50,
        bench_h2_concurrent_10,
        bench_h2_concurrent_50,
}
