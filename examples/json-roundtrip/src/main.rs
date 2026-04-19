use aioduct::Client;
use aioduct::runtime::TokioRuntime;
use serde::{Deserialize, Serialize};

#[derive(Serialize)]
struct CreatePost {
    title: String,
    body: String,
    user_id: u32,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct PostResponse {
    id: Option<u64>,
    title: Option<String>,
}

#[tokio::main]
async fn main() -> Result<(), aioduct::Error> {
    let client = Client::<TokioRuntime>::builder().build();

    // POST JSON and deserialize the response
    let payload = CreatePost {
        title: "Hello from aioduct".into(),
        body: "This is a test post".into(),
        user_id: 1,
    };

    let resp: PostResponse = client
        .post("https://jsonplaceholder.typicode.com/posts")?
        .json(&payload)?
        .send()
        .await?
        .json()
        .await?;

    println!("Created post: {resp:?}");

    // GET JSON
    let resp: serde_json::Value = client
        .get("https://jsonplaceholder.typicode.com/posts/1")?
        .send()
        .await?
        .json()
        .await?;

    println!("\nFetched post title: {}", resp["title"]);

    Ok(())
}
