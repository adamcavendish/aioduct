// features: tokio,json
// runtime: tokio
use aioduct::HttpEngine;
use aioduct::runtime::TokioRuntime;
use aioduct::runtime::tokio_rt::TcpConnector;

#[tokio::main]
async fn main() -> Result<(), aioduct::Error> {
    let client = HttpEngine::<TokioRuntime, TcpConnector>::builder(TcpConnector).build();

    let download = client
        .chunk_download("https://releases.ubuntu.com/24.04/ubuntu-24.04-live-server-amd64.iso")
        .chunks(8)
        .build()?;

    let bytes = download.execute().await?;
    println!("Downloaded {} bytes in 8 parallel chunks", bytes.len());
    Ok(())
}
