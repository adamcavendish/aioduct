#![cfg(all(feature = "tokio", feature = "rustls"))]

use std::convert::Infallible;
use std::time::Duration;

use aioduct::{HttpCache, TokioClient};
use aioduct_test_server::tls::{make_client_config, tls_server_with};
use bytes::Bytes;
use http_body_util::Full;

#[tokio::test]
async fn shared_cache_does_not_mix_automatic_host_variants() {
    for first_h1 in [true, false] {
        let (addr, cert, counter) = tls_server_with(&[b"h2", b"http/1.1"], |req| async move {
            let body = if req.headers().contains_key("host") {
                "present"
            } else {
                "absent"
            };
            Ok::<_, Infallible>(
                http::Response::builder()
                    .header("cache-control", "max-age=3600")
                    .header("vary", "Accept-Encoding")
                    .header("vary", "HoSt")
                    .body(Full::new(Bytes::from_static(body.as_bytes())))
                    .unwrap(),
            )
        })
        .await;
        let cache = HttpCache::new();
        let mut h1_tls = aioduct::tls::RustlsConnector::new(make_client_config(&cert));
        h1_tls.config_mut().alpn_protocols = vec![b"http/1.1".to_vec()];
        let h1 = TokioClient::builder()
            .tls(h1_tls)
            .cache(cache.clone())
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap();
        let h2 = TokioClient::builder()
            .tls(aioduct::tls::RustlsConnector::new(make_client_config(
                &cert,
            )))
            .cache(cache)
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap();
        let url = format!("https://localhost:{}/", addr.port());
        for use_h1 in [first_h1, !first_h1, first_h1, !first_h1] {
            let client = if use_h1 { &h1 } else { &h2 };
            let response = client.get(&url).unwrap().send().await.unwrap();
            assert_eq!(
                response.text().await.unwrap(),
                if use_h1 { "present" } else { "absent" }
            );
        }
        assert_eq!(
            counter.requests(),
            4,
            "unresolved Host variants must reach the server"
        );
    }
}

#[tokio::test]
async fn explicit_host_variants_still_hit_and_remain_separate() {
    let (addr, cert, counter) = tls_server_with(&[b"h2"], |req| async move {
        Ok::<_, Infallible>(
            http::Response::builder()
                .header("cache-control", "max-age=3600")
                .header("vary", "Host")
                .body(Full::new(Bytes::copy_from_slice(
                    req.headers()["host"].as_bytes(),
                )))
                .unwrap(),
        )
    })
    .await;
    let client = TokioClient::builder()
        .tls(aioduct::tls::RustlsConnector::new(make_client_config(
            &cert,
        )))
        .cache(HttpCache::new())
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let url = format!("https://localhost:{}/", addr.port());
    for host in ["one.test", "two.test", "one.test", "two.test"] {
        let response = client
            .get(&url)
            .unwrap()
            .header_str("host", host)
            .unwrap()
            .send()
            .await
            .unwrap();
        assert_eq!(response.text().await.unwrap(), host);
    }
    assert_eq!(counter.requests(), 2);
}

#[cfg(feature = "compio")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn local_client_does_not_reuse_send_clients_host_variant() {
    let (addr, cert, _) = tls_server_with(&[b"h2", b"http/1.1"], |req| async move {
        let body = if req.headers().contains_key("host") {
            "present"
        } else {
            "absent"
        };
        Ok::<_, Infallible>(
            http::Response::builder()
                .header("cache-control", "max-age=3600")
                .header("vary", "Host")
                .body(Full::new(Bytes::from_static(body.as_bytes())))
                .unwrap(),
        )
    })
    .await;
    let cache = HttpCache::new();
    let mut h1_tls = aioduct::tls::RustlsConnector::new(make_client_config(&cert));
    h1_tls.config_mut().alpn_protocols = vec![b"http/1.1".to_vec()];
    let h1 = TokioClient::builder()
        .tls(h1_tls)
        .cache(cache.clone())
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let url = format!("https://localhost:{}/", addr.port());
    assert_eq!(
        h1.get(&url)
            .unwrap()
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap(),
        "present"
    );
    let local_url = url.clone();
    tokio::task::spawn_blocking(move || {
        compio_runtime::Runtime::new()
            .unwrap()
            .block_on(async move {
                let h2 = aioduct::CompioClient::builder()
                    .tls(aioduct::tls::RustlsConnector::new(make_client_config(
                        &cert,
                    )))
                    .cache(cache)
                    .timeout(Duration::from_secs(5))
                    .build_local()
                    .unwrap();
                for _ in 0..2 {
                    assert_eq!(
                        h2.get_local(&local_url)
                            .unwrap()
                            .send()
                            .await
                            .unwrap()
                            .text()
                            .await
                            .unwrap(),
                        "absent"
                    );
                }
            });
    })
    .await
    .unwrap();
    assert_eq!(
        h1.get(&url)
            .unwrap()
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap(),
        "present"
    );
}
