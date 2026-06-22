use super::*;

// Stacked encodings must be decoded in order instead of treating the complete
// Content-Encoding value as one token.
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
        "stacked Content-Encoding values like 'gzip, identity' must be decoded"
    );
}

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
