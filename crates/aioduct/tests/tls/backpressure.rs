use std::convert::Infallible;
use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::task::{Context, Poll};
use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::Response;

use aioduct::HttpEngineSend;
use aioduct::runtime::tokio_rt::{TcpConnector, TokioIo};
use aioduct::runtime::{ConnectorSend, SocketConfig, TokioRuntime};

fn expected_ocr_multipart(boundary: &str, file: &[u8]) -> Bytes {
    let mut body = Vec::new();
    body.extend_from_slice(
        format!("--{boundary}\r\nContent-Disposition: form-data; name=\"model\"\r\n\r\nocr\r\n")
            .as_bytes(),
    );
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; \
             filename=\"ocr-page.pdf\"\r\nContent-Type: application/octet-stream\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(file);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    Bytes::from(body)
}

#[tokio::test]
async fn https_h1_multipart_upload_preserves_exact_body() {
    aioduct_test_server::tls::install_crypto_provider();

    let file: Vec<u8> = (0..2 * 1024 * 1024)
        .map(|i| ((i * 31 + i / 251) % 256) as u8)
        .collect();
    let file = Bytes::from(file);
    let boundary = "AioductUploadBoundary";
    let expected_body = expected_ocr_multipart(boundary, &file);

    let (addr, cert_der, _counter) =
        aioduct_test_server::tls::tls_server_with(&[b"http/1.1"], move |req| {
            let expected_body = expected_body.clone();
            async move {
                assert_eq!(req.method(), http::Method::POST);
                let content_type = req
                    .headers()
                    .get(http::header::CONTENT_TYPE)
                    .and_then(|value| value.to_str().ok())
                    .unwrap_or_default()
                    .to_owned();
                assert_eq!(
                    content_type,
                    format!("multipart/form-data; boundary=\"{boundary}\"")
                );

                let body = req.into_body().collect().await.unwrap().to_bytes();
                assert_eq!(body, expected_body);

                Ok::<_, Infallible>(Response::new(Full::new(Bytes::from_static(b"upload ok"))))
            }
        })
        .await;

    let client_config = aioduct_test_server::tls::make_client_config(&cert_der);
    let connector = aioduct::tls::RustlsConnector::new(client_config);
    let client: HttpEngineSend<TokioRuntime, TcpConnector> = HttpEngineSend::builder()
        .tls(connector)
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap();

    let multipart = aioduct::Multipart::new()
        .with_boundary(boundary)
        .unwrap()
        .text("model", "ocr")
        .part(
            aioduct::Part::bytes("file", file)
                .file_name("ocr-page.pdf")
                .mime_str("application/octet-stream"),
        );

    let resp = client
        .post(&format!(
            "https://localhost:{}/api/v2/ocr/jobs",
            addr.port()
        ))
        .unwrap()
        .multipart(multipart)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), http::StatusCode::OK);
    assert_eq!(resp.text().await.unwrap(), "upload ok");
}

#[derive(Clone)]
struct DeferredWriteErrorConnector {
    fail_next_write: Arc<AtomicBool>,
    connections: Arc<AtomicUsize>,
}

impl DeferredWriteErrorConnector {
    fn new() -> Self {
        Self {
            fail_next_write: Arc::new(AtomicBool::new(false)),
            connections: Arc::new(AtomicUsize::new(0)),
        }
    }

    fn fail_next_write(&self) {
        self.fail_next_write.store(true, Ordering::SeqCst);
    }

    fn connections(&self) -> usize {
        self.connections.load(Ordering::SeqCst)
    }
}

struct DeferredWriteErrorStream {
    inner: TokioIo<tokio::net::TcpStream>,
    fail_next_write: Arc<AtomicBool>,
}

impl hyper::rt::Read for DeferredWriteErrorStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: hyper::rt::ReadBufCursor<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

impl hyper::rt::Write for DeferredWriteErrorStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        if self.fail_next_write.swap(false, Ordering::SeqCst) {
            return Poll::Ready(Err(io::Error::other("injected deferred TLS write failure")));
        }
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

impl SocketConfig for DeferredWriteErrorStream {
    fn set_keepalive(
        &self,
        time: Duration,
        interval: Option<Duration>,
        retries: Option<u32>,
    ) -> io::Result<()> {
        self.inner.set_keepalive(time, interval, retries)
    }

    fn set_fast_open(&self) -> io::Result<()> {
        self.inner.set_fast_open()
    }
}

impl ConnectorSend for DeferredWriteErrorConnector {
    type Stream = DeferredWriteErrorStream;

    fn connect(&self, addr: SocketAddr) -> impl Future<Output = io::Result<Self::Stream>> + Send {
        let fail_next_write = Arc::clone(&self.fail_next_write);
        let connections = Arc::clone(&self.connections);
        async move {
            let inner = <TcpConnector as ConnectorSend>::connect(&TcpConnector, addr).await?;
            connections.fetch_add(1, Ordering::SeqCst);
            Ok(DeferredWriteErrorStream {
                inner,
                fail_next_write,
            })
        }
    }
}

#[tokio::test]
async fn https_deferred_write_error_evicts_pooled_connection() {
    aioduct_test_server::tls::install_crypto_provider();
    let (addr, cert_der, _counter) =
        aioduct_test_server::tls::tls_server_with(&[b"http/1.1"], |req| async move {
            let _ = req.into_body().collect().await;
            Ok::<_, Infallible>(Response::new(Full::new(Bytes::from_static(b"ok"))))
        })
        .await;

    let connector = DeferredWriteErrorConnector::new();
    let client_config = aioduct_test_server::tls::make_client_config(&cert_der);
    let tls = aioduct::tls::RustlsConnector::new(client_config);
    let client =
        HttpEngineSend::<TokioRuntime, DeferredWriteErrorConnector>::builder_with_connector(
            connector.clone(),
        )
        .tls(tls)
        .pool_idle_timeout(Duration::from_secs(60))
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let url = format!("https://localhost:{}/", addr.port());

    let response = client.get(&url).unwrap().send().await.unwrap();
    assert_eq!(response.text().await.unwrap(), "ok");
    assert_eq!(connector.connections(), 1);
    assert_eq!(client.pool_stats().idle_pool_entries, 1);

    connector.fail_next_write();
    let result = client
        .post(&url)
        .unwrap()
        .body(vec![b'x'; 64 * 1024])
        .send()
        .await;
    assert!(
        result.is_err(),
        "the injected TLS write failure must surface"
    );
    assert_eq!(connector.connections(), 1, "the failed POST must not retry");

    for _ in 0..100 {
        let stats = client.pool_stats();
        if stats.idle_pool_entries == 0 && stats.checked_out_pool_handles == 0 {
            break;
        }
        tokio::task::yield_now().await;
    }
    let failed_stats = client.pool_stats();
    assert_eq!(
        failed_stats.checkout_hits, 1,
        "the failed POST must use the pool"
    );
    assert_eq!(failed_stats.idle_pool_entries, 0);
    assert_eq!(failed_stats.checked_out_pool_handles, 0);

    let response = client.get(&url).unwrap().send().await.unwrap();
    assert_eq!(response.text().await.unwrap(), "ok");
    assert_eq!(connector.connections(), 2);
    assert_eq!(client.pool_stats().checkout_misses, 2);
}
