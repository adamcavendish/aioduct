use aioduct::WasiClient;

fn main() {
    let client = WasiClient::builder()
        .user_agent("aioduct-wasi-p2-demo/0.2")
        .build()
        .unwrap();
    let base_url =
        std::env::var("AIODUCT_WASI_DEMO_URL").unwrap_or_else(|_| "https://httpbin.org".into());

    // Simple GET
    let get_url = format!("{base_url}/get");
    println!("=== GET {get_url} ===");
    match client.get(&get_url) {
        Ok(req) => match req.send() {
            Ok(resp) => {
                println!("Status: {}", resp.status());
                for (name, value) in resp.headers() {
                    if let Ok(v) = value.to_str() {
                        println!("  {name}: {v}");
                    }
                }
                match resp.text() {
                    Ok(body) => println!("\nBody:\n{body}"),
                    Err(e) => eprintln!("Body read error: {e}"),
                }
            }
            Err(e) => eprintln!("Request error: {e}"),
        },
        Err(e) => eprintln!("Build error: {e}"),
    }

    // POST JSON
    let post_url = format!("{base_url}/post");
    println!("\n=== POST {post_url} ===");
    let payload = serde_json::json!({
        "message": "hello from WASI",
        "runtime": "wasm32-wasip2"
    });
    match client.post(&post_url) {
        Ok(req) => match req.json(&payload) {
            Ok(req) => match req.send() {
                Ok(resp) => {
                    println!("Status: {}", resp.status());
                    match resp.json::<serde_json::Value>() {
                        Ok(json) => {
                            println!(
                                "Body:\n{}",
                                serde_json::to_string_pretty(&json).unwrap_or_default()
                            );
                        }
                        Err(e) => eprintln!("JSON parse error: {e}"),
                    }
                }
                Err(e) => eprintln!("Request error: {e}"),
            },
            Err(e) => eprintln!("JSON serialize error: {e}"),
        },
        Err(e) => eprintln!("Build error: {e}"),
    }

    // Error handling
    let not_found_url = format!("{base_url}/status/404");
    println!("\n=== GET {not_found_url} ===");
    match client.get(&not_found_url) {
        Ok(req) => match req.send() {
            Ok(resp) => match resp.error_for_status() {
                Ok(_) => println!("Unexpected success"),
                Err(e) => println!("Expected error: {e}"),
            },
            Err(e) => eprintln!("Request error: {e}"),
        },
        Err(e) => eprintln!("Build error: {e}"),
    }
}
