#![cfg(feature = "tokio")]

use std::convert::Infallible;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use aioduct::runtime::TokioRuntime;
use aioduct::runtime::tokio_rt::TcpConnector;
use aioduct::{
    Error, HttpCache, HttpEngineSend, MessageSignatureComponent, MessageSignatureConfig,
    MessageSignatureError,
};
use bytes::Bytes;
use http::header::{AUTHORIZATION, CACHE_CONTROL, HeaderName, HeaderValue, VARY};
use http_body_util::{BodyExt, Full};
use hyper::Response;

use aioduct_test_server::h1::h1_server_with;

fn basic_config() -> MessageSignatureConfig {
    MessageSignatureConfig::new("sig1")
        .unwrap()
        .component(MessageSignatureComponent::method())
        .component(MessageSignatureComponent::request_target())
}

fn sign_new(_: &[u8]) -> Result<Vec<u8>, MessageSignatureError> {
    Ok(b"new".to_vec())
}

fn fail_signing(_: &[u8]) -> Result<Vec<u8>, MessageSignatureError> {
    Err(MessageSignatureError::Signer("failed".to_owned()))
}

#[tokio::test]
async fn automatic_signing_adds_headers_after_middleware() {
    let bases = Arc::new(Mutex::new(Vec::new()));
    let signer_bases = bases.clone();
    let signer = move |base: &[u8]| -> Result<Vec<u8>, MessageSignatureError> {
        signer_bases
            .lock()
            .unwrap()
            .push(String::from_utf8(base.to_vec()).unwrap());
        Ok(b"signed".to_vec())
    };
    let config = MessageSignatureConfig::new("sig1")
        .unwrap()
        .component(MessageSignatureComponent::method())
        .component(MessageSignatureComponent::request_target())
        .component(MessageSignatureComponent::header(HeaderName::from_static(
            "x-final",
        )));

    let (addr, _counter) = h1_server_with(|req| async move {
        let signature_input = req
            .headers()
            .get("signature-input")
            .map(|v| v.to_str().unwrap().to_owned())
            .unwrap_or_default();
        let signature = req
            .headers()
            .get("signature")
            .map(|v| v.to_str().unwrap().to_owned())
            .unwrap_or_default();
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(format!(
            "signature-input={signature_input}\nsignature={signature}"
        )))))
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .middleware(
            |req: &mut http::Request<aioduct::body::RequestBodySend>, _uri: &http::Uri| {
                req.headers_mut()
                    .insert("x-final", http::HeaderValue::from_static("middleware"));
            },
        )
        .message_signature(config, signer)
        .build()
        .unwrap();

    let resp = client
        .get(&format!("http://{addr}/resource?x=1"))
        .unwrap()
        .send()
        .await
        .unwrap();
    let body = resp.text().await.unwrap();
    assert!(body.contains("signature-input=sig1="), "{body}");
    assert!(body.contains("signature=sig1=:c2lnbmVk:"), "{body}");

    let bases = bases.lock().unwrap();
    assert_eq!(bases.len(), 1);
    assert!(bases[0].contains(r#""@request-target": /resource?x=1"#));
    assert!(bases[0].contains(r#""x-final": middleware"#));
}

#[tokio::test]
async fn manual_signature_headers_are_preserved_without_automatic_signing() {
    let (addr, _counter) = h1_server_with(|req| async move {
        let signature_input = req
            .headers()
            .get("signature-input")
            .map(|v| v.to_str().unwrap().to_owned())
            .unwrap_or_default();
        let signature = req
            .headers()
            .get("signature")
            .map(|v| v.to_str().unwrap().to_owned())
            .unwrap_or_default();
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(format!(
            "{signature_input}\n{signature}"
        )))))
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .header_str("signature-input", r#"old=("@method")"#)
        .unwrap()
        .header_str("signature", "old=:b2xk:")
        .unwrap()
        .send()
        .await
        .unwrap();

    let body = resp.text().await.unwrap();
    assert!(body.contains(r#"old=("@method")"#), "{body}");
    assert!(body.contains("old=:b2xk:"), "{body}");
}

#[tokio::test]
async fn automatic_signing_merges_existing_signature_headers_by_label() {
    let (addr, _counter) = h1_server_with(|req| async move {
        let signature_input = req
            .headers()
            .get("signature-input")
            .map(|v| v.to_str().unwrap().to_owned())
            .unwrap_or_default();
        let signature = req
            .headers()
            .get("signature")
            .map(|v| v.to_str().unwrap().to_owned())
            .unwrap_or_default();
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(format!(
            "{signature_input}\n{signature}"
        )))))
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .message_signature(basic_config(), sign_new)
        .build()
        .unwrap();
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .header_str("signature-input", r#"old=("@method"), sig1=("@path")"#)
        .unwrap()
        .header_str("signature", "old=:b2xk:")
        .unwrap()
        .send()
        .await
        .unwrap();

    let body = resp.text().await.unwrap();
    assert!(body.contains(r#"old=("@method")"#), "{body}");
    assert!(body.contains("old=:b2xk:"), "{body}");
    assert!(body.contains("sig1="), "{body}");
    assert!(body.contains("sig1=:bmV3:"), "{body}");
    assert!(!body.contains("c3RhbGU"), "{body}");
}

#[tokio::test]
async fn signer_error_aborts_request() {
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .message_signature(basic_config(), fail_signing)
        .build()
        .unwrap();

    let err = client
        .get("http://127.0.0.1:9/")
        .unwrap()
        .send()
        .await
        .unwrap_err();
    assert!(matches!(
        err.into_error(),
        Error::MessageSignature(MessageSignatureError::Signer(message)) if message == "failed"
    ));
}

#[tokio::test]
async fn digest_retry_signature_covers_retry_authorization() {
    let bases = Arc::new(Mutex::new(Vec::new()));
    let signer_bases = bases.clone();
    let signer = move |base: &[u8]| -> Result<Vec<u8>, MessageSignatureError> {
        signer_bases
            .lock()
            .unwrap()
            .push(String::from_utf8(base.to_vec()).unwrap());
        Ok(b"signed".to_vec())
    };
    let config = basic_config().component(MessageSignatureComponent::header(AUTHORIZATION));
    let attempts = Arc::new(AtomicUsize::new(0));
    let server_attempts = attempts.clone();

    let (addr, _counter) = h1_server_with(move |_req| {
        let server_attempts = server_attempts.clone();
        async move {
            if server_attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                Ok::<_, Infallible>(
                    Response::builder()
                        .status(401)
                        .header(
                            "www-authenticate",
                            r#"Digest realm="sig-test", nonce="abc123", qop="auth""#,
                        )
                        .body(Full::new(Bytes::from("unauthorized")))
                        .unwrap(),
                )
            } else {
                Ok(Response::new(Full::new(Bytes::from("ok"))))
            }
        }
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .digest_auth("user", "pass")
        .message_signature(config, signer)
        .build()
        .unwrap();
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .header_str("authorization", "placeholder")
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    let bases = bases.lock().unwrap();
    assert_eq!(bases.len(), 2);
    assert!(bases[0].contains(r#""authorization": placeholder"#));
    assert!(
        bases[1].contains(r#""authorization": Digest "#),
        "{}",
        bases[1]
    );
}

#[tokio::test]
async fn automatic_content_digest_is_inserted_before_signing() {
    let bases = Arc::new(Mutex::new(Vec::new()));
    let signer_bases = bases.clone();
    let signer = move |base: &[u8]| -> Result<Vec<u8>, MessageSignatureError> {
        signer_bases
            .lock()
            .unwrap()
            .push(String::from_utf8(base.to_vec()).unwrap());
        Ok(b"signed".to_vec())
    };
    let content_digest = HeaderName::from_static("content-digest");
    let config = MessageSignatureConfig::new("sig1")
        .unwrap()
        .component(MessageSignatureComponent::method())
        .component(MessageSignatureComponent::header(content_digest.clone()));

    let (addr, _counter) = h1_server_with(|req| async move {
        let content_digest = req
            .headers()
            .get("content-digest")
            .map(|v| v.to_str().unwrap().to_owned())
            .unwrap_or_default();
        let body = req.into_body().collect().await.unwrap().to_bytes();
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(format!(
            "content-digest={content_digest}\nbody={}",
            String::from_utf8_lossy(&body)
        )))))
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .automatic_content_digest(true)
        .message_signature(config, signer)
        .build()
        .unwrap();
    let resp = client
        .post(&format!("http://{addr}/digest"))
        .unwrap()
        .body("hello")
        .send()
        .await
        .unwrap();

    let body = resp.text().await.unwrap();
    let expected = "sha-256=:LPJNul+wow4m6DsqxbninhsWHlwfp0JecwQzYpOLmCQ=:";
    assert!(
        body.contains(&format!("content-digest={expected}")),
        "{body}"
    );
    assert!(body.contains("body=hello"), "{body}");

    let bases = bases.lock().unwrap();
    assert_eq!(bases.len(), 1);
    assert!(
        bases[0].contains(&format!(r#""content-digest": {expected}"#)),
        "{}",
        bases[0]
    );
}

#[tokio::test]
async fn automatic_content_digest_preserves_manual_header() {
    let bases = Arc::new(Mutex::new(Vec::new()));
    let signer_bases = bases.clone();
    let signer = move |base: &[u8]| -> Result<Vec<u8>, MessageSignatureError> {
        signer_bases
            .lock()
            .unwrap()
            .push(String::from_utf8(base.to_vec()).unwrap());
        Ok(b"signed".to_vec())
    };
    let config =
        MessageSignatureConfig::new("sig1")
            .unwrap()
            .component(MessageSignatureComponent::header(HeaderName::from_static(
                "content-digest",
            )));

    let (addr, _counter) = h1_server_with(|req| async move {
        let content_digest = req
            .headers()
            .get("content-digest")
            .map(|v| v.to_str().unwrap().to_owned())
            .unwrap_or_default();
        let _ = req.into_body().collect().await.unwrap().to_bytes();
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(content_digest))))
    })
    .await;

    let manual = "sha-256=:bWFudWFs:";
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .automatic_content_digest(true)
        .message_signature(config, signer)
        .build()
        .unwrap();
    let resp = client
        .post(&format!("http://{addr}/digest"))
        .unwrap()
        .header_str("content-digest", manual)
        .unwrap()
        .body("hello")
        .send()
        .await
        .unwrap();

    assert_eq!(resp.text().await.unwrap(), manual);
    let bases = bases.lock().unwrap();
    assert_eq!(bases.len(), 1);
    assert!(
        bases[0].contains(&format!(r#""content-digest": {manual}"#)),
        "{}",
        bases[0]
    );
}

#[tokio::test]
async fn automatic_content_digest_participates_in_cache_vary() {
    let hits = Arc::new(AtomicUsize::new(0));
    let server_hits = hits.clone();
    let (addr, _counter) = h1_server_with(move |req| {
        let server_hits = server_hits.clone();
        async move {
            let hit = server_hits.fetch_add(1, Ordering::SeqCst) + 1;
            let content_digest = req
                .headers()
                .get("content-digest")
                .map(|v| v.to_str().unwrap().to_owned())
                .unwrap_or_else(|| "none".to_owned());
            let mut resp = Response::new(Full::new(Bytes::from(format!(
                "hit={hit}\ncontent-digest={content_digest}"
            ))));
            resp.headers_mut()
                .insert(CACHE_CONTROL, HeaderValue::from_static("max-age=60"));
            resp.headers_mut()
                .insert(VARY, HeaderValue::from_static("Content-Digest"));
            Ok::<_, Infallible>(resp)
        }
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .automatic_content_digest(true)
        .cache(HttpCache::new())
        .build()
        .unwrap();
    let url = format!("http://{addr}/vary-content-digest");

    let first = client
        .get(&url)
        .unwrap()
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(first.contains("hit=1"), "{first}");
    assert!(first.contains("content-digest=none"), "{first}");

    let second = client
        .get(&url)
        .unwrap()
        .body("hello")
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    let expected = "sha-256=:LPJNul+wow4m6DsqxbninhsWHlwfp0JecwQzYpOLmCQ=:";
    assert!(second.contains("hit=2"), "{second}");
    assert!(
        second.contains(&format!("content-digest={expected}")),
        "{second}"
    );
    assert_eq!(hits.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn automatic_content_digest_rejects_streaming_body_without_manual_digest() {
    let (addr, _counter) = h1_server_with(|req| async move {
        let _ = req.into_body().collect().await.unwrap().to_bytes();
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("unexpected"))))
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .automatic_content_digest(true)
        .build()
        .unwrap();
    let stream_body = Full::new(Bytes::from_static(b"hello"))
        .map_err(|never| match never {})
        .boxed_unsync();
    let err = client
        .post(&format!("http://{addr}/digest"))
        .unwrap()
        .body_stream(stream_body)
        .send()
        .await
        .unwrap_err();

    assert!(
        matches!(err.into_error(), Error::Unsupported(message) if message.contains("automatic Content-Digest"))
    );
}

#[tokio::test]
async fn automatic_content_digest_rejects_middleware_replaced_body_without_manual_digest() {
    let (addr, _counter) = h1_server_with(|req| async move {
        let _ = req.into_body().collect().await.unwrap().to_bytes();
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("unexpected"))))
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .automatic_content_digest(true)
        .middleware(
            |req: &mut http::Request<aioduct::body::RequestBodySend>, _uri: &http::Uri| {
                *req.body_mut() = Full::new(Bytes::from_static(b"changed"))
                    .map_err(|never| match never {})
                    .boxed_unsync();
            },
        )
        .build()
        .unwrap();
    let err = client
        .post(&format!("http://{addr}/digest"))
        .unwrap()
        .body("hello")
        .send()
        .await
        .unwrap_err();

    assert!(
        matches!(err.into_error(), Error::Unsupported(message) if message.contains("automatic Content-Digest"))
    );
}

#[tokio::test]
async fn forwarding_signature_covers_rewritten_upstream_request() {
    let bases = Arc::new(Mutex::new(Vec::new()));
    let signer_bases = bases.clone();
    let signer = move |base: &[u8]| -> Result<Vec<u8>, MessageSignatureError> {
        signer_bases
            .lock()
            .unwrap()
            .push(String::from_utf8(base.to_vec()).unwrap());
        Ok(b"forwarded".to_vec())
    };
    let config = MessageSignatureConfig::new("sig1")
        .unwrap()
        .component(MessageSignatureComponent::method())
        .component(MessageSignatureComponent::request_target())
        .component(MessageSignatureComponent::target_uri())
        .component(MessageSignatureComponent::header(HeaderName::from_static(
            "x-forward-final",
        )));
    let (addr, _counter) = h1_server_with(|req| async move {
        let signature = req
            .headers()
            .get("signature")
            .map(|v| v.to_str().unwrap().to_owned())
            .unwrap_or_default();
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(signature))))
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .message_signature(config, signer)
        .build()
        .unwrap();
    let incoming = http::Request::builder()
        .method("GET")
        .uri("/proxy/users?active=1")
        .body(Full::new(Bytes::new()))
        .unwrap();
    let resp = client
        .forward(incoming)
        .upstream(format!("http://{addr}/api"))
        .strip_prefix("/proxy")
        .on_request(|parts| {
            parts
                .headers
                .insert("x-forward-final", http::HeaderValue::from_static("hooked"));
        })
        .send()
        .await
        .unwrap();

    assert_eq!(resp.text().await.unwrap(), "sig1=:Zm9yd2FyZGVk:");
    let bases = bases.lock().unwrap();
    assert_eq!(bases.len(), 1);
    assert!(bases[0].contains(r#""@request-target": /api/users?active=1"#));
    assert!(
        bases[0].contains(&format!(
            r#""@target-uri": http://{addr}/api/users?active=1"#
        )),
        "{}",
        bases[0]
    );
    assert!(bases[0].contains(r#""x-forward-final": hooked"#));
}

#[tokio::test]
async fn forwarding_stale_retry_preserves_signed_headers() {
    let (addr, _counter) = aioduct_test_server::stale::h1_rst_on_reuse().await;
    let config = basic_config().component(MessageSignatureComponent::header(
        HeaderName::from_static("x-forward-final"),
    ));
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .message_signature(config, sign_new)
        .build()
        .unwrap();

    for path in ["/first", "/second"] {
        let incoming = http::Request::builder()
            .method("GET")
            .uri(path)
            .body(Full::new(Bytes::new()))
            .unwrap();
        let resp = client
            .forward(incoming)
            .upstream(format!("http://{addr}"))
            .on_request(|parts| {
                parts
                    .headers
                    .insert("x-forward-final", http::HeaderValue::from_static("hooked"));
            })
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), http::StatusCode::OK);
        assert_eq!(resp.text().await.unwrap(), "ok");
    }
}

#[tokio::test]
async fn stale_connection_replay_is_resigned() {
    let sign_count = Arc::new(AtomicUsize::new(0));
    let signer_count = sign_count.clone();
    let signer = move |_base: &[u8]| -> Result<Vec<u8>, MessageSignatureError> {
        let count = signer_count.fetch_add(1, Ordering::SeqCst) + 1;
        Ok(format!("signed-{count}").into_bytes())
    };
    let (addr, _counter) = aioduct_test_server::stale::h1_rst_on_reuse().await;
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .message_signature(basic_config(), signer)
        .build()
        .unwrap();
    let url = format!("http://{addr}/");

    let first = client.get(&url).unwrap().send().await.unwrap();
    assert_eq!(first.status(), http::StatusCode::OK);
    let _ = first.text().await.unwrap();

    let second = client.get(&url).unwrap().send().await.unwrap();
    assert_eq!(second.status(), http::StatusCode::OK);
    assert_eq!(sign_count.load(Ordering::SeqCst), 3);
}
