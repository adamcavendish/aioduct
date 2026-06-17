#![cfg(feature = "tokio")]

use std::convert::Infallible;
#[cfg(any(
    feature = "gzip",
    feature = "deflate",
    feature = "brotli",
    feature = "zstd"
))]
use std::sync::atomic::{AtomicU32, Ordering};
#[cfg(any(
    feature = "gzip",
    feature = "deflate",
    feature = "brotli",
    feature = "zstd"
))]
use std::sync::{Arc, Mutex};
#[cfg(any(
    feature = "gzip",
    feature = "deflate",
    feature = "brotli",
    feature = "zstd"
))]
use std::time::Duration;

use bytes::Bytes;
use http_body_util::Full;
#[cfg(any(
    feature = "gzip",
    feature = "deflate",
    feature = "brotli",
    feature = "zstd"
))]
use hyper::Request;
use hyper::Response;

use aioduct::HttpEngineSend;
use aioduct::runtime::TokioRuntime;
use aioduct::runtime::tokio_rt::TcpConnector;

use aioduct_test_server::h1::h1_server_with;
#[cfg(any(
    feature = "gzip",
    feature = "deflate",
    feature = "brotli",
    feature = "zstd"
))]
use aioduct_test_server::raw::raw_streaming_server;

#[cfg(feature = "gzip")]
#[tokio::test]
async fn test_gzip_decompression() {
    use flate2::Compression;
    use flate2::write::GzEncoder;
    use std::io::Write;

    let handler = |_req: Request<hyper::body::Incoming>| async {
        let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
        encoder.write_all(b"hello compressed world").unwrap();
        let compressed = encoder.finish().unwrap();

        let resp = Response::builder()
            .header("content-encoding", "gzip")
            .body(Full::new(Bytes::from(compressed)))
            .unwrap();
        Ok::<_, Infallible>(resp)
    };
    let (addr, _counter) = h1_server_with(handler).await;
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert!(!resp.headers().contains_key("content-encoding"));
    let text = resp.text().await.unwrap();
    assert_eq!(text, "hello compressed world");
}
#[cfg(feature = "gzip")]
#[tokio::test]
async fn test_gzip_accept_encoding_header() {
    let handler = |req: Request<hyper::body::Incoming>| async move {
        let accept = req
            .headers()
            .get("accept-encoding")
            .map(|v| v.to_str().unwrap().to_string())
            .unwrap_or_default();
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(accept))))
    };
    let (addr, _counter) = h1_server_with(handler).await;
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();
    let text = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();

    assert!(text.contains("gzip"));
}
#[cfg(feature = "gzip")]
#[tokio::test]
async fn test_no_decompression_passthrough() {
    use flate2::Compression;
    use flate2::write::GzEncoder;
    use std::io::Write;

    let handler = |_req: Request<hyper::body::Incoming>| async {
        let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
        encoder.write_all(b"raw gzip data").unwrap();
        let compressed = encoder.finish().unwrap();

        let resp = Response::builder()
            .header("content-encoding", "gzip")
            .body(Full::new(Bytes::from(compressed)))
            .unwrap();
        Ok::<_, Infallible>(resp)
    };
    let (addr, _counter) = h1_server_with(handler).await;
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .no_decompression()
        .build()
        .unwrap();
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert!(resp.headers().contains_key("content-encoding"));
    let bytes = resp.bytes().await.unwrap();
    // Should be raw gzip, not decompressed
    assert_ne!(bytes.as_ref(), b"raw gzip data");
}
#[cfg(feature = "deflate")]
#[tokio::test]
async fn test_deflate_decompression() {
    use flate2::Compression;
    use flate2::write::ZlibEncoder;
    use std::io::Write;

    let handler = |_req: Request<hyper::body::Incoming>| async {
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::fast());
        encoder.write_all(b"deflate test payload").unwrap();
        let compressed = encoder.finish().unwrap();

        let resp = Response::builder()
            .header("content-encoding", "deflate")
            .body(Full::new(Bytes::from(compressed)))
            .unwrap();
        Ok::<_, Infallible>(resp)
    };
    let (addr, _counter) = h1_server_with(handler).await;
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();
    let text = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();

    assert_eq!(text, "deflate test payload");
}
#[tokio::test]
async fn test_get_no_content_headers() {
    let (addr, _counter) = h1_server_with(|req| async move {
        assert_eq!(req.method(), "GET");
        assert!(
            req.headers().get("content-length").is_none(),
            "GET should not have content-length"
        );
        assert!(
            req.headers().get("content-type").is_none(),
            "GET should not have content-type"
        );
        assert!(
            req.headers().get("transfer-encoding").is_none(),
            "GET should not have transfer-encoding"
        );
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("ok"))))
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), http::StatusCode::OK);
}
#[cfg(feature = "gzip")]
#[tokio::test]
async fn test_gzip_empty_body_head_request() {
    let (addr, _counter) = h1_server_with(|req| async move {
        assert_eq!(req.method(), "HEAD");
        Ok::<_, Infallible>(
            Response::builder()
                .header("content-encoding", "gzip")
                .body(Full::new(Bytes::new()))
                .unwrap(),
        )
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();
    let resp = client
        .head(&format!("http://{addr}/gzip"))
        .unwrap()
        .send()
        .await
        .unwrap();

    let body = resp.text().await.unwrap();
    assert_eq!(body, "");
}
#[cfg(feature = "gzip")]
#[tokio::test]
async fn test_custom_accept_encoding_preserved() {
    let (addr, _counter) = h1_server_with(|req| async move {
        let accept_encoding = req
            .headers()
            .get("accept-encoding")
            .map(|v| v.to_str().unwrap().to_owned())
            .unwrap_or_default();
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(accept_encoding))))
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .header(
            http::header::ACCEPT_ENCODING,
            http::header::HeaderValue::from_static("identity"),
        )
        .send()
        .await
        .unwrap();

    let body = resp.text().await.unwrap();
    assert_eq!(body, "identity");
}

#[cfg(feature = "gzip")]
mod gzip_tests {
    use super::*;
    use flate2::Compression;
    use flate2::write::GzEncoder;
    use std::io::Write;

    fn gzip_compress(input: &[u8]) -> Vec<u8> {
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(input).unwrap();
        encoder.finish().unwrap()
    }

    #[tokio::test]
    async fn gzip_large_body() {
        let content: String = (0..10_000).map(|i| format!("test {i}")).collect();
        let compressed = gzip_compress(content.as_bytes());

        let (addr, _counter) = h1_server_with(move |_req| {
            let compressed = compressed.clone();
            async move {
                Ok::<_, Infallible>(
                    Response::builder()
                        .header("content-encoding", "gzip")
                        .header("content-length", compressed.len().to_string())
                        .body(Full::new(Bytes::from(compressed)))
                        .unwrap(),
                )
            }
        })
        .await;

        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();
        let resp = client
            .get(&format!("http://{addr}/"))
            .unwrap()
            .send()
            .await
            .unwrap();

        let text = resp.text().await.unwrap();
        assert_eq!(text, content);
    }

    #[tokio::test]
    async fn gzip_empty_body_head_request() {
        let (addr, _counter) = h1_server_with(|req| async move {
            assert_eq!(req.method(), "HEAD");
            Ok::<_, Infallible>(
                Response::builder()
                    .header("content-encoding", "gzip")
                    .body(Full::new(Bytes::new()))
                    .unwrap(),
            )
        })
        .await;

        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();
        let resp = client
            .head(&format!("http://{addr}/"))
            .unwrap()
            .send()
            .await
            .unwrap();

        let body = resp.text().await.unwrap();
        assert_eq!(body, "");
    }

    #[tokio::test]
    async fn gzip_accept_encoding_sent_automatically() {
        let (addr, _counter) = h1_server_with(|req| async move {
            let ae = req
                .headers()
                .get("accept-encoding")
                .map(|v| v.to_str().unwrap().to_owned())
                .unwrap_or_default();
            assert!(
                ae.contains("gzip"),
                "accept-encoding should contain gzip, got: {ae}"
            );
            Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("ok"))))
        })
        .await;

        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();
        let resp = client
            .get(&format!("http://{addr}/"))
            .unwrap()
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), http::StatusCode::OK);
    }

    #[tokio::test]
    async fn gzip_accept_encoding_not_changed_if_set() {
        let (addr, _counter) = h1_server_with(|req| async move {
            let ae = req
                .headers()
                .get("accept-encoding")
                .map(|v| v.to_str().unwrap().to_owned())
                .unwrap_or_default();
            assert_eq!(ae, "identity");
            Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("ok"))))
        })
        .await;

        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();
        let resp = client
            .get(&format!("http://{addr}/"))
            .unwrap()
            .header(
                http::header::ACCEPT_ENCODING,
                http::HeaderValue::from_static("identity"),
            )
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), http::StatusCode::OK);
    }

    #[tokio::test]
    async fn gzip_connection_reuse_after_decompression() {
        let compressed = gzip_compress(b"hello compressed");

        let request_count = Arc::new(AtomicU32::new(0));
        let count_clone = request_count.clone();

        let (addr, _counter) = h1_server_with(move |_req| {
            let compressed = compressed.clone();
            let count = count_clone.clone();
            async move {
                count.fetch_add(1, Ordering::SeqCst);
                Ok::<_, Infallible>(
                    Response::builder()
                        .header("content-encoding", "gzip")
                        .body(Full::new(Bytes::from(compressed)))
                        .unwrap(),
                )
            }
        })
        .await;

        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();

        for _ in 0..3 {
            let resp = client
                .get(&format!("http://{addr}/"))
                .unwrap()
                .send()
                .await
                .unwrap();
            let text = resp.text().await.unwrap();
            assert_eq!(text, "hello compressed");
        }

        assert_eq!(request_count.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn no_decompression_passthrough() {
        let compressed = gzip_compress(b"raw gzip data");

        let (addr, _counter) = h1_server_with(move |_req| {
            let compressed = compressed.clone();
            async move {
                Ok::<_, Infallible>(
                    Response::builder()
                        .header("content-encoding", "gzip")
                        .body(Full::new(Bytes::from(compressed)))
                        .unwrap(),
                )
            }
        })
        .await;

        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
            .no_decompression()
            .build()
            .unwrap();

        let resp = client
            .get(&format!("http://{addr}/"))
            .unwrap()
            .send()
            .await
            .unwrap();

        assert!(resp.headers().contains_key("content-encoding"));
        let bytes = resp.bytes().await.unwrap();
        assert_ne!(&bytes[..], b"raw gzip data");
    }
}

#[cfg(feature = "deflate")]
mod deflate_tests {
    use super::*;
    use flate2::Compression;
    use flate2::write::ZlibEncoder;
    use std::io::Write;

    #[tokio::test]
    async fn deflate_decompression() {
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::fast());
        encoder.write_all(b"deflate test payload").unwrap();
        let compressed = encoder.finish().unwrap();

        let (addr, _counter) = h1_server_with(move |_req| {
            let compressed = compressed.clone();
            async move {
                Ok::<_, Infallible>(
                    Response::builder()
                        .header("content-encoding", "deflate")
                        .body(Full::new(Bytes::from(compressed)))
                        .unwrap(),
                )
            }
        })
        .await;

        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();
        let text = client
            .get(&format!("http://{addr}/"))
            .unwrap()
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();

        assert_eq!(text, "deflate test payload");
    }

    #[tokio::test]
    async fn deflate_accept_encoding_sent() {
        let (addr, _counter) = h1_server_with(|req| async move {
            let ae = req
                .headers()
                .get("accept-encoding")
                .map(|v| v.to_str().unwrap().to_owned())
                .unwrap_or_default();
            assert!(
                ae.contains("deflate"),
                "accept-encoding should contain deflate, got: {ae}"
            );
            Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("ok"))))
        })
        .await;

        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();
        client
            .get(&format!("http://{addr}/"))
            .unwrap()
            .send()
            .await
            .unwrap();
    }
}

#[cfg(feature = "brotli")]
mod brotli_tests {
    use super::*;
    use std::io::Write;

    fn brotli_compress(input: &[u8]) -> Vec<u8> {
        let mut compressed = Vec::new();
        let mut encoder = brotli::CompressorWriter::new(&mut compressed, 4096, 6, 22);
        encoder.write_all(input).unwrap();
        drop(encoder);
        compressed
    }

    #[tokio::test]
    async fn brotli_decompression() {
        let compressed = brotli_compress(b"brotli test payload");

        let (addr, _counter) = h1_server_with(move |_req| {
            let compressed = compressed.clone();
            async move {
                Ok::<_, Infallible>(
                    Response::builder()
                        .header("content-encoding", "br")
                        .body(Full::new(Bytes::from(compressed)))
                        .unwrap(),
                )
            }
        })
        .await;

        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();
        let text = client
            .get(&format!("http://{addr}/"))
            .unwrap()
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();

        assert_eq!(text, "brotli test payload");
    }

    #[tokio::test]
    async fn brotli_large_body() {
        let content: String = (0..5_000).map(|i| format!("brotli {i} ")).collect();
        let compressed = brotli_compress(content.as_bytes());

        let (addr, _counter) = h1_server_with(move |_req| {
            let compressed = compressed.clone();
            async move {
                Ok::<_, Infallible>(
                    Response::builder()
                        .header("content-encoding", "br")
                        .body(Full::new(Bytes::from(compressed)))
                        .unwrap(),
                )
            }
        })
        .await;

        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();
        let text = client
            .get(&format!("http://{addr}/"))
            .unwrap()
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();

        assert_eq!(text, content);
    }

    #[tokio::test]
    async fn brotli_accept_encoding_sent() {
        let (addr, _counter) = h1_server_with(|req| async move {
            let ae = req
                .headers()
                .get("accept-encoding")
                .map(|v| v.to_str().unwrap().to_owned())
                .unwrap_or_default();
            assert!(
                ae.contains("br"),
                "accept-encoding should contain br, got: {ae}"
            );
            Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("ok"))))
        })
        .await;

        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();
        client
            .get(&format!("http://{addr}/"))
            .unwrap()
            .send()
            .await
            .unwrap();
    }
}

#[cfg(feature = "zstd")]
mod zstd_tests {
    use super::*;

    #[tokio::test]
    async fn zstd_decompression() {
        let compressed = zstd::encode_all(b"zstd test payload" as &[u8], 3).unwrap();

        let (addr, _counter) = h1_server_with(move |_req| {
            let compressed = compressed.clone();
            async move {
                Ok::<_, Infallible>(
                    Response::builder()
                        .header("content-encoding", "zstd")
                        .body(Full::new(Bytes::from(compressed)))
                        .unwrap(),
                )
            }
        })
        .await;

        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();
        let text = client
            .get(&format!("http://{addr}/"))
            .unwrap()
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();

        assert_eq!(text, "zstd test payload");
    }

    #[tokio::test]
    async fn zstd_large_body() {
        let content: String = (0..5_000).map(|i| format!("zstd {i} ")).collect();
        let compressed = zstd::encode_all(content.as_bytes(), 3).unwrap();

        let (addr, _counter) = h1_server_with(move |_req| {
            let compressed = compressed.clone();
            async move {
                Ok::<_, Infallible>(
                    Response::builder()
                        .header("content-encoding", "zstd")
                        .body(Full::new(Bytes::from(compressed)))
                        .unwrap(),
                )
            }
        })
        .await;

        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();
        let text = client
            .get(&format!("http://{addr}/"))
            .unwrap()
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();

        assert_eq!(text, content);
    }

    #[tokio::test]
    async fn zstd_accept_encoding_sent() {
        let (addr, _counter) = h1_server_with(|req| async move {
            let ae = req
                .headers()
                .get("accept-encoding")
                .map(|v| v.to_str().unwrap().to_owned())
                .unwrap_or_default();
            assert!(
                ae.contains("zstd"),
                "accept-encoding should contain zstd, got: {ae}"
            );
            Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("ok"))))
        })
        .await;

        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();
        client
            .get(&format!("http://{addr}/"))
            .unwrap()
            .send()
            .await
            .unwrap();
    }
}

#[cfg(feature = "gzip")]
mod fragmented_gzip {
    use super::*;
    use flate2::Compression;
    use flate2::write::GzEncoder;
    use std::io::Write;
    use tokio::io::AsyncWriteExt;

    #[tokio::test]
    async fn gzip_chunked_fragmented_response() {
        let content = "hello fragmented gzip world";
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(content.as_bytes()).unwrap();
        let compressed = encoder.finish().unwrap();

        let mid = compressed.len() / 2;
        let part1: Vec<u8> = compressed[..mid].to_vec();
        let part2: Vec<u8> = compressed[mid..].to_vec();

        let addr = raw_streaming_server(move |_req, mut stream| {
            let p1 = part1.clone();
            let p2 = part2.clone();
            async move {
                let headers = "HTTP/1.1 200 OK\r\nContent-Encoding: gzip\r\nTransfer-Encoding: chunked\r\n\r\n";
                stream.write_all(headers.as_bytes()).await.unwrap();
                stream.flush().await.unwrap();

                let chunk1 = format!("{:x}\r\n", p1.len());
                stream.write_all(chunk1.as_bytes()).await.unwrap();
                stream.write_all(&p1).await.unwrap();
                stream.write_all(b"\r\n").await.unwrap();
                stream.flush().await.unwrap();

                tokio::time::sleep(Duration::from_millis(50)).await;

                let chunk2 = format!("{:x}\r\n", p2.len());
                stream.write_all(chunk2.as_bytes()).await.unwrap();
                stream.write_all(&p2).await.unwrap();
                stream.write_all(b"\r\n").await.unwrap();
                stream.write_all(b"0\r\n\r\n").await.unwrap();
                stream.flush().await.unwrap();
            }
        })
        .await;

        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();
        let resp = client
            .get(&format!("http://{addr}/"))
            .unwrap()
            .send()
            .await
            .unwrap();

        assert_eq!(resp.text().await.unwrap(), content);
    }
}

#[cfg(feature = "deflate")]
mod fragmented_deflate {
    use super::*;
    use flate2::Compression;
    use flate2::write::ZlibEncoder;
    use std::io::Write;
    use tokio::io::AsyncWriteExt;

    #[tokio::test]
    async fn deflate_chunked_fragmented_response() {
        let content = "hello fragmented deflate world";
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(content.as_bytes()).unwrap();
        let compressed = encoder.finish().unwrap();

        let third = compressed.len() / 3;
        let parts: Vec<Vec<u8>> = vec![
            compressed[..third].to_vec(),
            compressed[third..third * 2].to_vec(),
            compressed[third * 2..].to_vec(),
        ];

        let addr = raw_streaming_server(move |_req, mut stream| {
            let parts = parts.clone();
            async move {
                let headers = "HTTP/1.1 200 OK\r\nContent-Encoding: deflate\r\nTransfer-Encoding: chunked\r\n\r\n";
                stream.write_all(headers.as_bytes()).await.unwrap();
                stream.flush().await.unwrap();

                for part in &parts {
                    let chunk_hdr = format!("{:x}\r\n", part.len());
                    stream.write_all(chunk_hdr.as_bytes()).await.unwrap();
                    stream.write_all(part).await.unwrap();
                    stream.write_all(b"\r\n").await.unwrap();
                    stream.flush().await.unwrap();
                    tokio::time::sleep(Duration::from_millis(30)).await;
                }

                stream.write_all(b"0\r\n\r\n").await.unwrap();
                stream.flush().await.unwrap();
            }
        })
        .await;

        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();
        let resp = client
            .get(&format!("http://{addr}/"))
            .unwrap()
            .send()
            .await
            .unwrap();

        assert_eq!(resp.text().await.unwrap(), content);
    }
}

#[cfg(feature = "brotli")]
mod fragmented_brotli {
    use super::*;
    use tokio::io::AsyncWriteExt;

    #[tokio::test]
    async fn brotli_chunked_fragmented_response() {
        let content = "hello fragmented brotli world";
        let mut compressed = Vec::new();
        {
            let mut writer = brotli::CompressorWriter::new(&mut compressed, 4096, 6, 22);
            std::io::Write::write_all(&mut writer, content.as_bytes()).unwrap();
            drop(writer);
        }

        let bytes: Vec<u8> = compressed;
        let addr = raw_streaming_server(move |_req, mut stream| {
            let bytes = bytes.clone();
            async move {
                let headers =
                    "HTTP/1.1 200 OK\r\nContent-Encoding: br\r\nTransfer-Encoding: chunked\r\n\r\n";
                stream.write_all(headers.as_bytes()).await.unwrap();
                stream.flush().await.unwrap();

                for byte in &bytes[..5.min(bytes.len())] {
                    stream.write_all(b"1\r\n").await.unwrap();
                    stream.write_all(&[*byte]).await.unwrap();
                    stream.write_all(b"\r\n").await.unwrap();
                    stream.flush().await.unwrap();
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }

                if bytes.len() > 5 {
                    let rest = &bytes[5..];
                    let chunk_hdr = format!("{:x}\r\n", rest.len());
                    stream.write_all(chunk_hdr.as_bytes()).await.unwrap();
                    stream.write_all(rest).await.unwrap();
                    stream.write_all(b"\r\n").await.unwrap();
                    stream.flush().await.unwrap();
                }

                stream.write_all(b"0\r\n\r\n").await.unwrap();
                stream.flush().await.unwrap();
            }
        })
        .await;

        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();
        let resp = client
            .get(&format!("http://{addr}/"))
            .unwrap()
            .send()
            .await
            .unwrap();

        assert_eq!(resp.text().await.unwrap(), content);
    }
}

// ── Bug-Finding Tests ─────────────────────────────────────────────────

// BUG: decompress.rs:323-341 compares the entire Content-Encoding header value as
// raw bytes. A stacked encoding like "gzip, identity" doesn't match any arm and
// falls through to `_ => None`, returning the body still compressed.
#[cfg(feature = "gzip")]
#[tokio::test]
async fn stacked_content_encoding_not_handled() {
    use flate2::Compression;
    use flate2::write::GzEncoder;
    use std::io::Write;

    let content = "hello stacked encoding";

    let mut gzip_encoder = GzEncoder::new(Vec::new(), Compression::fast());
    gzip_encoder.write_all(content.as_bytes()).unwrap();
    let compressed = gzip_encoder.finish().unwrap();

    let addr = raw_streaming_server(move |_request_bytes, mut stream| {
        let compressed = compressed.clone();
        async move {
            use tokio::io::AsyncWriteExt;
            let response = format!(
                "HTTP/1.1 200 OK\r\n\
                 Content-Encoding: gzip, identity\r\n\
                 Content-Length: {}\r\n\
                 \r\n",
                compressed.len()
            );
            stream.write_all(response.as_bytes()).await.unwrap();
            stream.write_all(&compressed).await.unwrap();
            stream.flush().await.unwrap();
        }
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();

    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    let body = resp.text().await.unwrap();
    assert_eq!(
        body, content,
        "BUG: decompress.rs:328-341 matches Content-Encoding as exact bytes. \
         Stacked encodings like 'gzip, identity' are not recognized and the body \
         is returned still compressed."
    );
}

// ── Case-Insensitive Content-Encoding Tests (RFC 9110) ───────────────

#[cfg(feature = "gzip")]
mod case_insensitive_encoding {
    use super::*;
    use flate2::Compression;
    use flate2::write::GzEncoder;
    use std::io::Write;
    use tokio::io::AsyncWriteExt;

    fn gzip_compress(input: &[u8]) -> Vec<u8> {
        let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
        encoder.write_all(input).unwrap();
        encoder.finish().unwrap()
    }

    #[tokio::test]
    async fn uppercase_gzip_decompressed() {
        let content = "uppercase GZIP test";
        let compressed = gzip_compress(content.as_bytes());

        let addr = raw_streaming_server(move |_req, mut stream| {
            let compressed = compressed.clone();
            async move {
                let response = format!(
                    "HTTP/1.1 200 OK\r\n\
                     Content-Encoding: GZIP\r\n\
                     Content-Length: {}\r\n\
                     \r\n",
                    compressed.len()
                );
                stream.write_all(response.as_bytes()).await.unwrap();
                stream.write_all(&compressed).await.unwrap();
                stream.flush().await.unwrap();
            }
        })
        .await;

        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap();
        let resp = client
            .get(&format!("http://{addr}/"))
            .unwrap()
            .send()
            .await
            .unwrap();
        assert_eq!(resp.text().await.unwrap(), content);
    }

    #[tokio::test]
    async fn mixed_case_gzip_decompressed() {
        let content = "mixed case Gzip test";
        let compressed = gzip_compress(content.as_bytes());

        let addr = raw_streaming_server(move |_req, mut stream| {
            let compressed = compressed.clone();
            async move {
                let response = format!(
                    "HTTP/1.1 200 OK\r\n\
                     Content-Encoding: Gzip\r\n\
                     Content-Length: {}\r\n\
                     \r\n",
                    compressed.len()
                );
                stream.write_all(response.as_bytes()).await.unwrap();
                stream.write_all(&compressed).await.unwrap();
                stream.flush().await.unwrap();
            }
        })
        .await;

        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap();
        let resp = client
            .get(&format!("http://{addr}/"))
            .unwrap()
            .send()
            .await
            .unwrap();
        assert_eq!(resp.text().await.unwrap(), content);
    }

    #[tokio::test]
    async fn x_gzip_alias_decompressed() {
        let content = "x-gzip alias test";
        let compressed = gzip_compress(content.as_bytes());

        let addr = raw_streaming_server(move |_req, mut stream| {
            let compressed = compressed.clone();
            async move {
                let response = format!(
                    "HTTP/1.1 200 OK\r\n\
                     Content-Encoding: x-gzip\r\n\
                     Content-Length: {}\r\n\
                     \r\n",
                    compressed.len()
                );
                stream.write_all(response.as_bytes()).await.unwrap();
                stream.write_all(&compressed).await.unwrap();
                stream.flush().await.unwrap();
            }
        })
        .await;

        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap();
        let resp = client
            .get(&format!("http://{addr}/"))
            .unwrap()
            .send()
            .await
            .unwrap();
        assert_eq!(resp.text().await.unwrap(), content);
    }
}

#[cfg(feature = "deflate")]
#[tokio::test]
async fn uppercase_deflate_decompressed() {
    use flate2::Compression;
    use flate2::write::ZlibEncoder;
    use std::io::Write;
    use tokio::io::AsyncWriteExt;

    let content = "uppercase DEFLATE test";
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::fast());
    encoder.write_all(content.as_bytes()).unwrap();
    let compressed = encoder.finish().unwrap();

    let addr = raw_streaming_server(move |_req, mut stream| {
        let compressed = compressed.clone();
        async move {
            let response = format!(
                "HTTP/1.1 200 OK\r\n\
                 Content-Encoding: DEFLATE\r\n\
                 Content-Length: {}\r\n\
                 \r\n",
                compressed.len()
            );
            stream.write_all(response.as_bytes()).await.unwrap();
            stream.write_all(&compressed).await.unwrap();
            stream.flush().await.unwrap();
        }
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.text().await.unwrap(), content);
}

#[cfg(feature = "brotli")]
#[tokio::test]
async fn uppercase_brotli_decompressed() {
    use std::io::Write;
    use tokio::io::AsyncWriteExt;

    let content = "uppercase BR test";
    let mut compressed = Vec::new();
    {
        let mut writer = brotli::CompressorWriter::new(&mut compressed, 4096, 6, 22);
        writer.write_all(content.as_bytes()).unwrap();
    }

    let addr = raw_streaming_server(move |_req, mut stream| {
        let compressed = compressed.clone();
        async move {
            let response = format!(
                "HTTP/1.1 200 OK\r\n\
                 Content-Encoding: BR\r\n\
                 Content-Length: {}\r\n\
                 \r\n",
                compressed.len()
            );
            stream.write_all(response.as_bytes()).await.unwrap();
            stream.write_all(&compressed).await.unwrap();
            stream.flush().await.unwrap();
        }
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.text().await.unwrap(), content);
}

#[cfg(feature = "zstd")]
#[tokio::test]
async fn uppercase_zstd_decompressed() {
    use tokio::io::AsyncWriteExt;

    let content = "uppercase ZSTD test";
    let compressed = zstd::encode_all(content.as_bytes(), 3).unwrap();

    let addr = raw_streaming_server(move |_req, mut stream| {
        let compressed = compressed.clone();
        async move {
            let response = format!(
                "HTTP/1.1 200 OK\r\n\
                 Content-Encoding: ZSTD\r\n\
                 Content-Length: {}\r\n\
                 \r\n",
                compressed.len()
            );
            stream.write_all(response.as_bytes()).await.unwrap();
            stream.write_all(&compressed).await.unwrap();
            stream.flush().await.unwrap();
        }
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.text().await.unwrap(), content);
}

// ── Body malformed/boundary tests ─────────────────────────────────────

#[cfg(feature = "gzip")]
#[tokio::test]
async fn malformed_gzip_body_returns_error() {
    use tokio::io::AsyncWriteExt;

    let addr = raw_streaming_server(move |_req, mut stream| async move {
        let body = b"this is not valid gzip compressed data";
        let header = format!(
            "HTTP/1.1 200 OK\r\nContent-Encoding: gzip\r\nContent-Length: {}\r\n\r\n",
            body.len()
        );
        stream.write_all(header.as_bytes()).await.unwrap();
        stream.write_all(body).await.unwrap();
        stream.flush().await.unwrap();
        stream.shutdown().await.unwrap();
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    let result = resp.text().await;
    assert!(
        result.is_err(),
        "malformed gzip body should cause decompression error, got: {:?}",
        result.ok()
    );
}

/// Server sends Content-Length: 5 but writes far more bytes on the wire.
/// After the client reads the 5-byte body, the remaining bytes corrupt the
/// connection, so the next request either fails or opens a new connection.
#[tokio::test]
async fn content_length_mismatch_too_long_poisons_reuse() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let conn_count = Arc::new(AtomicUsize::new(0));

    tokio::spawn({
        let conn_count = conn_count.clone();
        async move {
            loop {
                let (mut stream, _) = match listener.accept().await {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                conn_count.fetch_add(1, Ordering::SeqCst);
                tokio::spawn(async move {
                    let mut buf = [0u8; 4096];
                    let n = match stream.read(&mut buf).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => n,
                    };
                    if !buf[..n].windows(4).any(|w| w == b"\r\n\r\n") {
                        return;
                    }

                    // Content-Length claims 5 bytes but we write ~105.
                    let extra = "x".repeat(100);
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: keep-alive\r\n\r\nhello{extra}"
                    );
                    let _ = stream.write_all(response.as_bytes()).await;
                    let _ = stream.flush().await;

                    // Leave the connection open; the 100 extra bytes will
                    // corrupt any subsequent request the client tries to
                    // pipeline on the same TCP stream.
                    let _ = tokio::time::timeout(Duration::from_millis(300), stream.read(&mut buf))
                        .await;
                });
            }
        }
    });

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .pool_idle_timeout(Duration::from_secs(60))
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();

    let url = format!("http://{addr}/");

    // First request: reads exactly 5 bytes (Content-Length) — succeeds.
    let resp = client.get(&url).unwrap().send().await.unwrap();
    let body = resp.text().await.unwrap();
    assert_eq!(body, "hello");

    // Give the pool a moment to return the (now-corrupted) connection.
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Second request: either fails (dirty connection) or succeeds but
    // must use a fresh connection.
    let before = conn_count.load(Ordering::SeqCst);
    let result = client.get(&url).unwrap().send().await;
    match result {
        Ok(resp) => {
            let _ = resp.text().await.unwrap();
            let after = conn_count.load(Ordering::SeqCst);
            assert!(
                after > before,
                "expected a new connection after Content-Length \
                 mismatch (before={before}, after={after})"
            );
        }
        Err(_) => {
            // Corrupted connection produced an error — acceptable.
        }
    }
}

#[cfg(feature = "gzip")]
#[tokio::test]
async fn content_length_removed_after_decompression() {
    use flate2::Compression;
    use flate2::write::GzEncoder;
    use std::io::Write;

    let content = "content length should disappear";
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(content.as_bytes()).unwrap();
    let compressed = encoder.finish().unwrap();

    let (addr, _counter) = h1_server_with(move |_req| {
        let compressed = compressed.clone();
        async move {
            Ok::<_, Infallible>(
                Response::builder()
                    .header("content-encoding", "gzip")
                    .header("content-length", compressed.len().to_string())
                    .body(Full::new(Bytes::from(compressed)))
                    .unwrap(),
            )
        }
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    // Content-Length must be removed because the decompressed body size
    // no longer matches the original value.
    assert!(
        resp.headers().get("content-length").is_none(),
        "Content-Length should be stripped after decompression"
    );

    let text = resp.text().await.unwrap();
    assert_eq!(text, content);
}

#[cfg(feature = "gzip")]
#[tokio::test]
async fn decompressed_body_empty_is_ok() {
    use flate2::Compression;
    use flate2::write::GzEncoder;

    // A gzip stream with no payload — just header + footer.
    let encoder = GzEncoder::new(Vec::new(), Compression::default());
    let compressed = encoder.finish().unwrap();
    // Should still be a valid gzip file (header + trailer, zero payload).

    let (addr, _counter) = h1_server_with(move |_req| {
        let compressed = compressed.clone();
        async move {
            Ok::<_, Infallible>(
                Response::builder()
                    .header("content-encoding", "gzip")
                    .body(Full::new(Bytes::from(compressed)))
                    .unwrap(),
            )
        }
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    let text = resp.text().await.unwrap();
    assert_eq!(
        text, "",
        "empty gzip body should decompress to empty string"
    );
}

// ── Round-trip and trailer pass-through tests ──────────────────────────

/// 64KB gzip round-trip: compress, serve, decompress, verify.
#[cfg(feature = "gzip")]
#[tokio::test]
async fn decompress_gzip_round_trip_with_large_body() {
    use flate2::Compression;
    use flate2::write::GzEncoder;
    use std::io::Write;

    let content = "A".repeat(65536); // 64 KB
    let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
    encoder.write_all(content.as_bytes()).unwrap();
    let compressed = encoder.finish().unwrap();

    let (addr, _counter) = h1_server_with(move |_req| {
        let compressed = compressed.clone();
        async move {
            Ok::<_, Infallible>(
                Response::builder()
                    .header("content-encoding", "gzip")
                    .header("content-length", compressed.len().to_string())
                    .body(Full::new(Bytes::from(compressed)))
                    .unwrap(),
            )
        }
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    let text = resp.text().await.unwrap();
    assert_eq!(text, content);
}

/// Send gzip-compressed chunked response followed by a trailer header.
/// Verify the decompressed body is correct AND that `TrailersReceived`
/// fires with the expected trailer headers.
#[cfg(feature = "gzip")]
#[tokio::test]
async fn trailer_frame_passes_through_decompress() {
    use flate2::Compression;
    use flate2::write::GzEncoder;
    use std::io::Write;
    use tokio::io::AsyncWriteExt;

    use aioduct::observer::{ConnectionEvent, RequestEvent, RequestObserver, RequestPhase};

    let content = "hello trailer decompress";
    let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
    encoder.write_all(content.as_bytes()).unwrap();
    let compressed = encoder.finish().unwrap();

    let events: Arc<Mutex<Vec<RequestPhase>>> = Arc::new(Mutex::new(Vec::new()));
    let events_clone = events.clone();

    struct TrailerObserver(Arc<Mutex<Vec<RequestPhase>>>);
    impl RequestObserver for TrailerObserver {
        fn on_event(&self, event: &RequestEvent) {
            self.0.lock().unwrap().push(event.phase.clone());
        }
        fn on_connection_event(&self, _event: &ConnectionEvent) {}
    }

    let addr = raw_streaming_server(move |_req, mut stream| {
        let compressed = compressed.clone();
        async move {
            let chunk_header = format!("{:x}\r\n", compressed.len());
            let response_header = "HTTP/1.1 200 OK\r\n\
                 Content-Encoding: gzip\r\n\
                 Transfer-Encoding: chunked\r\n\
                 Trailer: x-response-time\r\n\
                 \r\n";
            stream.write_all(response_header.as_bytes()).await.unwrap();
            stream.write_all(chunk_header.as_bytes()).await.unwrap();
            stream.write_all(&compressed).await.unwrap();
            stream.write_all(b"\r\n").await.unwrap();
            // Terminating chunk + trailer
            stream.write_all(b"0\r\n").await.unwrap();
            stream.write_all(b"x-response-time: 42\r\n").await.unwrap();
            stream.write_all(b"\r\n").await.unwrap();
            stream.flush().await.unwrap();
        }
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .timeout(Duration::from_secs(5))
        .request_observer(TrailerObserver(events_clone))
        .build()
        .unwrap();

    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    // Use into_bytes_stream so the observer fires TrailersReceived.
    let mut stream = resp.into_bytes_stream();
    let mut body = Vec::new();
    while let Some(chunk) = stream.next().await {
        body.extend_from_slice(&chunk.unwrap());
    }

    assert_eq!(
        String::from_utf8(body).unwrap(),
        content,
        "decompressed body must match original"
    );

    let captured = events.lock().unwrap();
    let has_trailers = captured.iter().any(|p| {
        matches!(p, RequestPhase::TrailersReceived { headers }
            if headers.iter().any(|(k, v)| k == "x-response-time" && v == "42"))
    });
    assert!(
        has_trailers,
        "expected TrailersReceived with x-response-time: 42, got: {captured:?}"
    );
}

/// Round-trip brotli: compress, serve with Content-Encoding: br, decompress.
#[cfg(feature = "brotli")]
#[tokio::test]
async fn decompress_brotli_round_trip() {
    use std::io::Write;

    let content = "hello brotli round trip test payload with sufficient length to compress well";
    let mut compressed = Vec::new();
    {
        let mut writer = brotli::CompressorWriter::new(&mut compressed, 4096, 6, 22);
        writer.write_all(content.as_bytes()).unwrap();
        drop(writer);
    }

    let (addr, _counter) = h1_server_with(move |_req| {
        let compressed = compressed.clone();
        async move {
            Ok::<_, Infallible>(
                Response::builder()
                    .header("content-encoding", "br")
                    .body(Full::new(Bytes::from(compressed)))
                    .unwrap(),
            )
        }
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    let text = resp.text().await.unwrap();
    assert_eq!(text, content);
}

/// Round-trip zstd: compress, serve with Content-Encoding: zstd, decompress.
#[cfg(feature = "zstd")]
#[tokio::test]
async fn decompress_zstd_round_trip() {
    let content = "hello zstd round trip test payload with sufficient length to compress well";
    let compressed = zstd::encode_all(content.as_bytes(), 3).unwrap();

    let (addr, _counter) = h1_server_with(move |_req| {
        let compressed = compressed.clone();
        async move {
            Ok::<_, Infallible>(
                Response::builder()
                    .header("content-encoding", "zstd")
                    .body(Full::new(Bytes::from(compressed)))
                    .unwrap(),
            )
        }
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    let text = resp.text().await.unwrap();
    assert_eq!(text, content);
}

// ── Decompression Edge-Case Tests ──────────────────────────────────────

/// Corrupt gzip body (valid header/footer, zeroed payload) must produce a
/// decode error. The error is at the application (decompression) level, not
/// transport corruption, so the pooled connection must remain usable: a second
/// request on the same client must succeed.
#[cfg(feature = "gzip")]
#[tokio::test]
async fn corrupt_gzip_body_propagates_decode_error() {
    use flate2::Compression;
    use flate2::write::GzEncoder;
    use std::io::Write;

    // Build a valid gzip body, then corrupt its middle bytes.
    let valid_content = "second request valid content";
    let valid = {
        let mut e = GzEncoder::new(Vec::new(), Compression::default());
        e.write_all(valid_content.as_bytes()).unwrap();
        e.finish().unwrap()
    };

    let corrupt_original =
        "first request content that will be corrupted in the middle of the gzip stream";
    let mut corrupt = {
        let mut e = GzEncoder::new(Vec::new(), Compression::default());
        e.write_all(corrupt_original.as_bytes()).unwrap();
        e.finish().unwrap()
    };

    // Keep the gzip header and footer intact; zero-fill the middle section so
    // the decoder recognizes the format but fails during stream decompression.
    let start = corrupt.len() / 3;
    let end = (corrupt.len() * 2) / 3;
    for byte in &mut corrupt[start..end] {
        *byte = 0;
    }

    let request_count = Arc::new(AtomicU32::new(0));

    let (addr, _counter) = h1_server_with({
        let request_count = request_count.clone();
        let valid = valid.clone();
        let corrupt = corrupt.clone();
        move |_req: Request<hyper::body::Incoming>| {
            let count = request_count.fetch_add(1, Ordering::SeqCst);
            let body = if count == 0 {
                corrupt.clone()
            } else {
                valid.clone()
            };
            async move {
                Ok::<_, Infallible>(
                    Response::builder()
                        .header("content-encoding", "gzip")
                        .body(Full::new(Bytes::from(body)))
                        .unwrap(),
                )
            }
        }
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();

    // Request 1: corrupt body → must error.
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();
    let result = resp.text().await;
    assert!(
        result.is_err(),
        "corrupt gzip body must produce a decode error, got: {:?}",
        result.ok()
    );

    // Request 2: same client, valid body → must succeed.
    // The pool connection was NOT evicted by the application-level error.
    let resp2 = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();
    let text2 = resp2.text().await.unwrap();
    assert_eq!(text2, valid_content);
}

/// Content-Encoding declares brotli ("br") but the body is actually gzip.
/// With both decompressors enabled, the brotli decoder receives gzip bytes
/// and must return an error — not silently pass through or return wrong data.
#[cfg(all(feature = "gzip", feature = "brotli"))]
#[tokio::test]
async fn content_encoding_brotli_with_gzip_body_errors() {
    use flate2::Compression;
    use flate2::write::GzEncoder;
    use std::io::Write;

    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(b"gzip body served as brotli").unwrap();
    let gzip_body = encoder.finish().unwrap();

    let (addr, _counter) = h1_server_with(move |_req| {
        let gzip_body = gzip_body.clone();
        async move {
            Ok::<_, Infallible>(
                Response::builder()
                    .header("content-encoding", "br")
                    .body(Full::new(Bytes::from(gzip_body)))
                    .unwrap(),
            )
        }
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    let result = resp.text().await;
    assert!(
        result.is_err(),
        "brotli Content-Encoding with gzip body must error, got: {:?}",
        result.ok()
    );
}

/// Decompression bomb: 100 MB of zeros → ~100 KB gzip.
///
/// The `max_decoded_size(1_000_000)` limit (1 MB) should cause the body read
/// to error before allocating the full 100 MB.
#[cfg(feature = "gzip")]
#[tokio::test]
async fn gzip_bomb_rejected_by_max_decoded_size() {
    use flate2::Compression;
    use flate2::write::GzEncoder;
    use std::io::Write;

    let big = vec![0u8; 100_000_000]; // 100 MB zeros
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(&big).unwrap();
    let compressed = encoder.finish().unwrap();

    let (addr, _counter) = h1_server_with(move |_req| {
        let compressed = compressed.clone();
        async move {
            Ok::<_, Infallible>(
                Response::builder()
                    .header("content-encoding", "gzip")
                    .header("content-length", compressed.len().to_string())
                    .body(Full::new(Bytes::from(compressed)))
                    .unwrap(),
            )
        }
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .max_decoded_size(Some(1_000_000)) // 1 MB limit
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap();

    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    let result = resp.text().await;
    assert!(
        result.is_err(),
        "decompression bomb must be rejected by max_decoded_size limit"
    );
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("exceeds max size"),
        "error should mention max size, got: {err_msg}"
    );
}

// ── Per-request no_decompression ────────────────────────────────────────────

/// A per-request `no_decompression()` call returns the raw compressed body and
/// suppresses the Accept-Encoding request header, overriding the client default.
#[cfg(feature = "gzip")]
#[tokio::test]
async fn per_request_no_decompression_returns_raw_bytes() {
    use flate2::Compression;
    use flate2::write::GzEncoder;
    use std::io::Write;

    let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
    encoder.write_all(b"raw gzip payload").unwrap();
    let compressed = encoder.finish().unwrap();
    let expected = compressed.clone();

    let captured_accept = Arc::new(Mutex::new(None::<String>));
    let cap = captured_accept.clone();
    let (addr, _counter) = h1_server_with(move |req: Request<hyper::body::Incoming>| {
        let cap = cap.clone();
        let body = compressed.clone();
        async move {
            *cap.lock().unwrap() = req
                .headers()
                .get("accept-encoding")
                .map(|v| v.to_str().unwrap_or("").to_string());
            Ok::<_, Infallible>(
                Response::builder()
                    .header("content-encoding", "gzip")
                    .body(Full::new(Bytes::from(body)))
                    .unwrap(),
            )
        }
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .no_decompression()
        .send()
        .await
        .unwrap();

    // Content-Encoding is preserved and the body is the raw gzip bytes.
    assert_eq!(resp.headers().get("content-encoding").unwrap(), "gzip");
    let bytes = resp.bytes().await.unwrap();
    assert_eq!(bytes.as_ref(), expected.as_slice());

    // No Accept-Encoding was sent for this request.
    assert!(
        captured_accept.lock().unwrap().is_none(),
        "no_decompression() must suppress the Accept-Encoding header"
    );
}

/// Without no_decompression(), the same client still decompresses (sanity that
/// the override is per-request, not global).
#[cfg(feature = "gzip")]
#[tokio::test]
async fn no_decompression_is_per_request() {
    use flate2::Compression;
    use flate2::write::GzEncoder;
    use std::io::Write;

    let make_body = || {
        let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
        encoder.write_all(b"decoded text").unwrap();
        encoder.finish().unwrap()
    };
    let (addr, _counter) = h1_server_with(move |_req| {
        let body = make_body();
        async move {
            Ok::<_, Infallible>(
                Response::builder()
                    .header("content-encoding", "gzip")
                    .body(Full::new(Bytes::from(body)))
                    .unwrap(),
            )
        }
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();

    // Default request: decompressed.
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.text().await.unwrap(), "decoded text");

    // Same client, no_decompression request: raw bytes (not equal to plaintext).
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .no_decompression()
        .send()
        .await
        .unwrap();
    let raw = resp.bytes().await.unwrap();
    assert_ne!(raw.as_ref(), b"decoded text");
}

/// Accept-Encoding never advertises `br` when the brotli feature is disabled.
#[cfg(all(feature = "gzip", not(feature = "brotli")))]
#[tokio::test]
async fn accept_encoding_omits_br_without_brotli_feature() {
    let (addr, _counter) = h1_server_with(|req: Request<hyper::body::Incoming>| async move {
        let accept = req
            .headers()
            .get("accept-encoding")
            .map(|v| v.to_str().unwrap_or("").to_string())
            .unwrap_or_default();
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(accept))))
    })
    .await;
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();
    let accept = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(
        !accept.contains("br"),
        "must not advertise br without brotli, got: {accept}"
    );
}
