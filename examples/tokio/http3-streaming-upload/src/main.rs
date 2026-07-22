use std::time::Duration;

use aioduct::TokioClient;
use bytes::{Buf as _, Bytes};
use http_body_util::BodyExt as _;

fn h3_client(write_timeout: Duration) -> Result<TokioClient, aioduct::Error> {
    TokioClient::builder()
        .tls(aioduct::tls::RustlsConnector::danger_accept_invalid_certs())
        .http3(true)?
        .write_timeout(write_timeout)
        .timeout(Duration::from_secs(5))
        .build()
}

async fn stream_ordered_chunks() -> Result<(), aioduct::Error> {
    const CHUNKS: u32 = 8;
    let (address, _, _) =
        aioduct_test_server::h3::h3_server_streaming(|_request, mut stream| async move {
            let mut expected = 0_u32;
            while let Some(mut chunk) = stream.recv_data().await.unwrap() {
                assert_eq!(chunk.get_u32(), expected);
                expected += 1;
            }
            assert_eq!(expected, CHUNKS);
            stream
                .send_response(http::Response::builder().status(200).body(()).unwrap())
                .await
                .unwrap();
            stream.finish().await.unwrap();
        })
        .await;

    let frames = futures_util::stream::iter((0..CHUNKS).map(|index| {
        Ok::<_, aioduct::Error>(hyper::body::Frame::data(Bytes::copy_from_slice(
            &index.to_be_bytes(),
        )))
    }));
    let body = http_body_util::StreamBody::new(frames).boxed_unsync();
    let url = format!("https://127.0.0.1:{}/upload", address.port());
    let response = h3_client(Duration::from_secs(1))?
        .post(&url)?
        .body_stream(body)
        .send()
        .await?;

    println!(
        "streamed {CHUNKS} ordered chunks over {:?}: {}",
        response.version(),
        response.status()
    );
    Ok(())
}

async fn demonstrate_write_timeout() -> Result<(), aioduct::Error> {
    let (address, _, _) =
        aioduct_test_server::h3::h3_server_streaming(|_request, mut stream| async move {
            while stream.recv_data().await.unwrap_or(None).is_some() {}
        })
        .await;
    let frames =
        futures_util::stream::pending::<Result<hyper::body::Frame<Bytes>, aioduct::Error>>();
    let body = http_body_util::StreamBody::new(frames).boxed_unsync();
    let url = format!("https://127.0.0.1:{}/stalled", address.port());
    let error = h3_client(Duration::from_millis(100))?
        .post(&url)?
        .body_stream(body)
        .send()
        .await
        .expect_err("a stalled body producer must reach the write timeout");
    assert!(error.error().is_write_timeout());
    println!("stalled producer failed closed: {error}");
    Ok(())
}

async fn demonstrate_trailer_rejection() -> Result<(), aioduct::Error> {
    let (address, _, _) =
        aioduct_test_server::h3::h3_server_streaming(|_request, mut stream| async move {
            while stream.recv_data().await.unwrap_or(None).is_some() {}
        })
        .await;
    let mut trailers = http::HeaderMap::new();
    trailers.insert(
        "x-upload-checksum",
        http::HeaderValue::from_static("sha256:demo"),
    );
    let frames = futures_util::stream::iter([Ok::<_, aioduct::Error>(
        hyper::body::Frame::trailers(trailers),
    )]);
    let body = http_body_util::StreamBody::new(frames).boxed_unsync();
    let url = format!("https://127.0.0.1:{}/trailers", address.port());
    let error = h3_client(Duration::from_secs(1))?
        .post(&url)?
        .body_stream(body)
        .send()
        .await
        .expect_err("HTTP/3 request trailers are intentionally unsupported");
    assert!(matches!(error.error(), aioduct::Error::Unsupported(_)));
    println!("request trailers failed closed: {error}");
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), aioduct::Error> {
    stream_ordered_chunks().await?;
    demonstrate_write_timeout().await?;
    demonstrate_trailer_rejection().await?;
    Ok(())
}
