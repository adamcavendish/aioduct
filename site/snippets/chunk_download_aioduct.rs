// features: tokio,json
// runtime: tokio
use aioduct::TokioClient;

#[tokio::main]
async fn main() -> Result<(), aioduct::Error> {
    let client = TokioClient::builder().build()?;

    let result = client
        .chunk_download("https://releases.ubuntu.com/24.04/ubuntu-24.04-live-server-amd64.iso")
        .chunks(8)
        .download()
        .await?;

    println!("Downloaded {} bytes in 8 parallel chunks", result.data.len());
    Ok(())
}
