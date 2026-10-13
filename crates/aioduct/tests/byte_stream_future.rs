#![cfg(all(
    not(target_arch = "wasm32"),
    any(feature = "tokio", feature = "smol", feature = "compio")
))]

use aioduct::{Error, traits::ByteStreamExt};
use aioduct_test_server::h1::spawn_h1_server_with;
use bytes::Bytes;
use http_body_util::Full;
use std::convert::Infallible;
#[cfg(any(feature = "tokio", feature = "smol"))]
use std::future::Future;

fn server_url() -> String {
    let addr = spawn_h1_server_with(|_| async {
        Ok::<_, Infallible>(hyper::Response::new(Full::new(Bytes::from_static(
            b"generic body",
        ))))
    });
    format!("http://{addr}/")
}

async fn read<S: ByteStreamExt>(mut stream: S) -> Result<Vec<u8>, Error> {
    let mut bytes = Vec::new();
    while let Some(chunk) = ByteStreamExt::next(&mut stream).await {
        bytes.extend_from_slice(&chunk?);
    }
    Ok(bytes)
}
#[cfg(any(feature = "tokio", feature = "smol"))]
fn read_send<S>(stream: S) -> impl Future<Output = Result<Vec<u8>, Error>> + Send
where
    S: ByteStreamExt + Send,
    for<'a> S::Next<'a>: Send,
{
    read(stream)
}

#[cfg(feature = "tokio")]
#[tokio::test]
async fn generic_byte_reader_runs_in_tokio_send_task() {
    let response = aioduct::TokioClient::new()
        .get(&server_url())
        .unwrap()
        .send()
        .await
        .unwrap();
    let bytes = tokio::spawn(read_send(response.into_bytes_stream()))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(bytes, b"generic body");
}

#[cfg(feature = "smol")]
#[test]
fn generic_byte_reader_runs_in_smol_send_task() {
    let url = server_url();
    smol::block_on(async {
        let response = aioduct::SmolClient::new()
            .get(&url)
            .unwrap()
            .send()
            .await
            .unwrap();
        let bytes = smol::spawn(read_send(response.into_bytes_stream()))
            .await
            .unwrap();
        assert_eq!(bytes, b"generic body");
    });
}

#[cfg(feature = "compio")]
#[test]
fn generic_byte_reader_runs_on_compio_without_send_bound() {
    let url = server_url();
    compio_runtime::Runtime::new().unwrap().block_on(async {
        let response = aioduct::CompioClient::new()
            .get_local(&url)
            .unwrap()
            .send()
            .await
            .unwrap();
        let bytes = read(response.into_bytes_stream()).await.unwrap();
        assert_eq!(bytes, b"generic body");
    });
}
