use aioduct::Client;
use aioduct::runtime::TokioRuntime;

#[tokio::main]
async fn main() -> Result<(), aioduct::Error> {
    let client = Client::<TokioRuntime>::builder().build();

    let url = "https://releases.ubuntu.com/24.04/ubuntu-24.04.2-desktop-amd64.iso.zsync";

    // Parallel chunk download — splits the file into ranges and downloads concurrently
    println!("Starting parallel chunk download...");
    println!("URL: {url}");

    let result = client
        .chunk_download(url)
        .chunks(4) // 4 parallel range requests
        .download()
        .await?;

    println!("Total size: {} bytes", result.total_size);
    println!("Data length: {} bytes", result.data.len());

    // Verify data integrity
    assert_eq!(result.total_size as usize, result.data.len());
    println!("Download complete and verified!");

    Ok(())
}
