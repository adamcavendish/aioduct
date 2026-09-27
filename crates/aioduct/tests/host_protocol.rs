#![cfg(all(feature = "tokio", feature = "rustls"))]

use std::convert::Infallible;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use aioduct::{
    MessageSignatureComponent, MessageSignatureConfig, MessageSignatureError, TokioClient,
};
use aioduct_test_server::tls::{make_client_config, tls_server_with};
use bytes::Bytes;
use http::{Request, Response, Version};
use http_body_util::Full;

#[tokio::test]
async fn negotiated_host_fields_survive_reuse_and_redirects() {
    for version in [Version::HTTP_11, Version::HTTP_2] {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let captured = seen.clone();
        let alpn: &[&[u8]] = if version == Version::HTTP_2 {
            &[b"h2"]
        } else {
            &[b"http/1.1"]
        };
        let (addr, cert, counter) = tls_server_with(alpn, move |req| {
            captured
                .lock()
                .unwrap()
                .push((req.uri().clone(), req.headers().clone()));
            async move {
                let response = if req.uri().path() == "/redirect" {
                    Response::builder()
                        .status(302)
                        .header("location", "/final?q=1")
                } else {
                    Response::builder()
                };
                Ok::<_, Infallible>(response.body(Full::new(Bytes::from_static(b"ok"))).unwrap())
            }
        })
        .await;
        let client = TokioClient::builder()
            .tls(aioduct::tls::RustlsConnector::new(make_client_config(
                &cert,
            )))
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap();
        for path in ["/first?q=1", "/second?q=1", "/redirect"] {
            let response = client
                .get(&format!("https://localhost:{}{path}", addr.port()))
                .unwrap()
                .send()
                .await
                .unwrap();
            assert_eq!(response.version(), version);
            assert_eq!(response.status(), 200);
            assert_eq!(response.text().await.unwrap(), "ok");
        }
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 4);
        assert_eq!(
            seen.last().unwrap().0.path_and_query().unwrap(),
            "/final?q=1"
        );
        let authority = format!("localhost:{}", addr.port());
        for (uri, headers) in seen.iter() {
            if version == Version::HTTP_2 {
                assert_eq!(uri.scheme_str(), Some("https"));
                assert_eq!(uri.authority().unwrap().as_str(), authority);
                assert!(
                    !headers.contains_key("host"),
                    "unexpected Host: {headers:?}"
                );
            } else {
                assert_eq!(headers["host"], authority);
            }
        }
        if version == Version::HTTP_2 {
            assert_eq!(
                counter.connections(),
                1,
                "requests should reuse the H2 connection"
            );
        }
    }
}

#[tokio::test]
async fn explicit_and_middleware_host_are_preserved() {
    for alpn in [b"h2".as_slice(), b"http/1.1".as_slice()] {
        let (addr, cert, _) = tls_server_with(&[alpn], |req| async move {
            Ok::<_, Infallible>(Response::new(Full::new(Bytes::copy_from_slice(
                req.headers()["host"].as_bytes(),
            ))))
        })
        .await;
        let client = TokioClient::builder()
            .tls(aioduct::tls::RustlsConnector::new(make_client_config(
                &cert,
            )))
            .timeout(Duration::from_secs(5))
            .middleware(
                |req: &mut Request<aioduct::body::RequestBodySend>, _: &http::Uri| {
                    if req.uri().path() == "/middleware" {
                        req.headers_mut()
                            .insert("host", "middleware.test".parse().unwrap());
                    }
                },
            )
            .build()
            .unwrap();
        for (path, expected) in [
            ("/explicit", "caller.test"),
            ("/middleware", "middleware.test"),
        ] {
            let response = client
                .get(&format!("https://localhost:{}{path}", addr.port()))
                .unwrap()
                .header_str("host", "caller.test")
                .unwrap()
                .send()
                .await
                .unwrap();
            assert_eq!(response.text().await.unwrap(), expected);
        }
    }
}

#[tokio::test]
async fn signed_host_is_present_before_signing_and_on_the_wire() {
    let (addr, cert, _) = tls_server_with(&[b"h2"], |req| async move {
        assert!(
            req.headers()["signature-input"]
                .to_str()
                .unwrap()
                .contains("\"host\"")
        );
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::copy_from_slice(
            req.headers()["host"].as_bytes(),
        ))))
    })
    .await;
    let authority = format!("localhost:{}", addr.port());
    let expected = authority.clone();
    let config = MessageSignatureConfig::new("sig1")
        .unwrap()
        .component(MessageSignatureComponent::header(http::header::HOST));
    let client = TokioClient::builder()
        .tls(aioduct::tls::RustlsConnector::new(make_client_config(
            &cert,
        )))
        .timeout(Duration::from_secs(5))
        .message_signature(
            config,
            move |base: &[u8]| -> Result<Vec<u8>, MessageSignatureError> {
                assert!(
                    std::str::from_utf8(base)
                        .unwrap()
                        .contains(&format!("\"host\": {expected}"))
                );
                Ok(b"signature".to_vec())
            },
        )
        .build()
        .unwrap();
    let response = client
        .get(&format!("https://{authority}/"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(response.text().await.unwrap(), authority);
}

#[tokio::test]
async fn redirects_recompute_host_when_switching_protocols() {
    let h2_url = Arc::new(Mutex::new(String::new()));
    let target = h2_url.clone();
    let (h1_addr, _) = aioduct_test_server::h1::h1_server_with(move |req| {
        let target = target.lock().unwrap().clone();
        async move {
            assert!(req.headers().contains_key("host"));
            let response = if req.uri().path() == "/start" {
                Response::builder().status(302).header("location", target)
            } else {
                Response::builder()
            };
            Ok::<_, Infallible>(
                response
                    .body(Full::new(Bytes::from_static(b"done")))
                    .unwrap(),
            )
        }
    })
    .await;
    let (h2_addr, cert, _) = tls_server_with(&[b"h2"], move |req| async move {
        assert!(!req.headers().contains_key("host"));
        Ok::<_, Infallible>(
            Response::builder()
                .status(302)
                .header("location", format!("http://{h1_addr}/final"))
                .body(Full::new(Bytes::new()))
                .unwrap(),
        )
    })
    .await;
    *h2_url.lock().unwrap() = format!("https://localhost:{}/middle", h2_addr.port());
    let client = TokioClient::builder()
        .tls(aioduct::tls::RustlsConnector::new(make_client_config(
            &cert,
        )))
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let response = client
        .get(&format!("http://{h1_addr}/start"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(response.url().path(), "/final");
    assert_eq!(response.text().await.unwrap(), "done");
}

// Each client runs on its own executor; the TLS server runs on Tokio.
#[cfg(any(feature = "smol", feature = "compio"))]
macro_rules! runtime_host_test {
    ($name:ident, $client:ty, $build:ident, $get:ident, $run:expr) => {
        #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn $name() {
            let (addr, cert, counter) = tls_server_with(&[b"h2"], |req| async move {
                assert_eq!(req.uri().scheme_str(), Some("https"));
                assert!(req.uri().authority().is_some());
                assert!(!req.headers().contains_key("host"));
                Ok::<_, Infallible>(Response::new(Full::new(Bytes::from_static(b"ok"))))
            })
            .await;
            tokio::task::spawn_blocking(move || {
                ($run)(async move {
                    let client = <$client>::builder()
                        .tls(aioduct::tls::RustlsConnector::new(make_client_config(
                            &cert,
                        )))
                        .timeout(Duration::from_secs(5))
                        .$build()
                        .unwrap();
                    for _ in 0..2 {
                        let response = client
                            .$get(&format!("https://localhost:{}/", addr.port()))
                            .unwrap()
                            .send()
                            .await
                            .unwrap();
                        assert_eq!(response.version(), Version::HTTP_2);
                        assert_eq!(response.text().await.unwrap(), "ok");
                    }
                });
            })
            .await
            .unwrap();
            assert_eq!(counter.connections(), 1);
        }
    };
}

#[cfg(feature = "smol")]
runtime_host_test!(
    smol_omits_automatic_host_on_h2,
    aioduct::SmolClient,
    build,
    get,
    smol::block_on
);

#[cfg(feature = "compio")]
runtime_host_test!(
    compio_omits_automatic_host_on_h2,
    aioduct::CompioClient,
    build_local,
    get_local,
    |future| { compio_runtime::Runtime::new().unwrap().block_on(future) }
);

#[tokio::test]
async fn signing_authority_does_not_add_host() {
    let (addr, cert, _) = tls_server_with(&[b"h2"], |req| async move {
        assert!(!req.headers().contains_key("host"));
        assert!(
            req.headers()["signature-input"]
                .to_str()
                .unwrap()
                .contains("\"@authority\"")
        );
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from_static(b"ok"))))
    })
    .await;
    let config = MessageSignatureConfig::new("sig1")
        .unwrap()
        .component(MessageSignatureComponent::authority());
    let client = TokioClient::builder()
        .tls(aioduct::tls::RustlsConnector::new(make_client_config(
            &cert,
        )))
        .timeout(Duration::from_secs(5))
        .message_signature(
            config,
            |_: &[u8]| -> Result<Vec<u8>, MessageSignatureError> { Ok(b"signature".to_vec()) },
        )
        .build()
        .unwrap();
    let response = client
        .get(&format!("https://localhost:{}/", addr.port()))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(response.text().await.unwrap(), "ok");
}

#[cfg(feature = "http3")]
#[tokio::test]
async fn h3_omits_automatic_host() {
    let (addr, cert, counter) = aioduct_test_server::h3::h3_server_with(|req, _| {
        assert_eq!(req.uri().scheme_str(), Some("https"));
        assert!(req.uri().authority().is_some());
        assert!(!req.headers().contains_key("host"));
        (http::StatusCode::OK, Bytes::from_static(b"ok"))
    })
    .await;
    let client = TokioClient::builder()
        .tls(aioduct::tls::RustlsConnector::new(make_client_config(
            &cert,
        )))
        .http3(true)
        .unwrap()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    for _ in 0..2 {
        let response = client
            .get(&format!("https://localhost:{}/", addr.port()))
            .unwrap()
            .send()
            .await
            .unwrap();
        assert_eq!(response.version(), Version::HTTP_3);
        assert_eq!(response.text().await.unwrap(), "ok");
    }
    assert_eq!(counter.connections(), 1);
}
