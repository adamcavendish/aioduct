use aioduct::runtime::TokioRuntime;
use aioduct::{Client, Error};

type TokioClient = Client<TokioRuntime>;

#[tokio::main]
async fn main() {
    let client = TokioClient::builder().build();

    // error_for_status() converts 4xx/5xx into errors
    match fetch_with_status_check(&client).await {
        Ok(body) => println!("Success: {body}"),
        Err(e) => println!("Status error: {e}"),
    }

    // Match specific error variants
    match fetch_nonexistent(&client).await {
        Ok(_) => println!("Unexpected success"),
        Err(Error::Status(status)) => {
            println!("Got HTTP error status: {status}");
        }
        Err(Error::Timeout) => {
            println!("Request timed out");
        }
        Err(e) => {
            println!("Other error: {e}");
        }
    }

    // error_for_status_ref() checks without consuming the response
    let resp = client
        .get("https://httpbin.org/status/200")
        .unwrap()
        .send()
        .await
        .unwrap();

    match resp.error_for_status_ref() {
        Ok(r) => println!("\nStatus {} is OK, can still use response", r.status()),
        Err(e) => println!("Error: {e}"),
    }
    println!("Body: {}", resp.text().await.unwrap());
}

async fn fetch_with_status_check(client: &TokioClient) -> Result<String, Error> {
    let resp = client
        .get("https://httpbin.org/status/404")?
        .send()
        .await?
        .error_for_status()?;
    resp.text().await
}

async fn fetch_nonexistent(client: &TokioClient) -> Result<String, Error> {
    client
        .get("https://httpbin.org/status/503")?
        .send()
        .await?
        .error_for_status()?
        .text()
        .await
}
