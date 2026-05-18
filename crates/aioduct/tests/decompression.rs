#![cfg(feature = "tokio")]

use std::convert::Infallible;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use bytes::Bytes;
use http_body_util::Full;
use hyper::Request;
use hyper::Response;

use aioduct::HttpEngineSend;
use aioduct::runtime::TokioRuntime;
use aioduct::runtime::tokio_rt::TcpConnector;

use aioduct_test_server::h1::h1_server_with;
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
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new(TcpConnector);
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
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new(TcpConnector);
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
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
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
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new(TcpConnector);
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

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new(TcpConnector);
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

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new(TcpConnector);
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

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new(TcpConnector);
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

        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new(TcpConnector);
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

        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new(TcpConnector);
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

        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new(TcpConnector);
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

        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new(TcpConnector);
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

        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new(TcpConnector);

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

        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
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

        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new(TcpConnector);
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

        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new(TcpConnector);
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

        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new(TcpConnector);
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

        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new(TcpConnector);
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

        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new(TcpConnector);
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

        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new(TcpConnector);
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

        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new(TcpConnector);
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

        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new(TcpConnector);
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

        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new(TcpConnector);
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

        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new(TcpConnector);
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

        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new(TcpConnector);
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

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
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

        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
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

        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
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

        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
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

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
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

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
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

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
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
