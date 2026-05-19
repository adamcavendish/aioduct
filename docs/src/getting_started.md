# Getting Started

## Installation

Add aioduct to your `Cargo.toml` with at least one runtime feature:

```toml
[dependencies]
aioduct = { version = "0.2.0-alpha.1", features = ["tokio"] }
```

For HTTPS support, add the `rustls` backend and exactly one rustls crypto provider:

```toml
[dependencies]
aioduct = { version = "0.2.0-alpha.1", features = ["tokio", "rustls", "rustls-ring"] }
```

To use rustls with AWS-LC instead, select the AWS-LC provider:

```toml
[dependencies]
aioduct = { version = "0.2.0-alpha.1", features = ["tokio", "rustls", "rustls-aws-lc-rs"] }
```

For JSON serialization/deserialization:

```toml
[dependencies]
aioduct = { version = "0.2.0-alpha.1", features = ["tokio", "rustls", "rustls-ring", "json"] }
```

## Quick Example

```rust,no_run
use aioduct::{TokioClient, StatusCode};

#[tokio::main]
async fn main() -> Result<(), aioduct::Error> {
    let client = TokioClient::new();

    let resp = client
        .get("http://httpbin.org/get")?
        .send()
        .await?;

    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.text().await?;
    println!("{body}");
    Ok(())
}
```

## HTTPS with rustls

```rust,no_run
use aioduct::TokioClient;

#[tokio::main]
async fn main() -> Result<(), aioduct::Error> {
    let client = TokioClient::with_rustls();

    let resp = client
        .get("https://httpbin.org/get")?
        .send()
        .await?;

    println!("status: {}", resp.status());
    Ok(())
}
```

## Sending JSON

Requires the `json` feature.

```rust,no_run
use aioduct::TokioClient;
use serde::{Deserialize, Serialize};

#[derive(Serialize)]
struct CreateUser {
    name: String,
    email: String,
}

#[derive(Deserialize)]
struct User {
    id: u64,
    name: String,
}

#[tokio::main]
async fn main() -> Result<(), aioduct::Error> {
    let client = TokioClient::with_rustls();

    let resp = client
        .post("https://api.example.com/users")?
        .json(&CreateUser {
            name: "Alice".into(),
            email: "alice@example.com".into(),
        })?
        .send()
        .await?;

    let user: User = resp.json().await?;
    println!("created user {} with id {}", user.name, user.id);
    Ok(())
}
```

## Using the smol Runtime

```rust,no_run
use aioduct::SmolClient;

fn main() -> Result<(), aioduct::Error> {
    smol::block_on(async {
        let client = SmolClient::new();

        let resp = client
            .get("http://httpbin.org/get")?
            .send()
            .await?;

        println!("status: {}", resp.status());
        Ok(())
    })
}
```

## Client Configuration

```rust,no_run
use std::time::Duration;
use aioduct::TokioClient;

let client = TokioClient::builder()
    .timeout(Duration::from_secs(30))
    .max_redirects(5)
    .pool_idle_timeout(Duration::from_secs(90))
    .pool_max_idle_per_host(10)
    .build()?;
```
