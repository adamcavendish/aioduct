use aioduct::WasiClient;

fn main() {
    let client = WasiClient::builder()
        .user_agent("aioduct-wasi-p2-demo/0.2")
        .build()
        .unwrap();

    // Simple GET
    println!("=== GET https://httpbin.org/get ===");
    match client.get("https://httpbin.org/get") {
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
    println!("\n=== POST https://httpbin.org/post ===");
    let payload = serde_json::json!({
        "message": "hello from WASI",
        "runtime": "wasm32-wasip2"
    });
    match client.post("https://httpbin.org/post") {
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
    println!("\n=== GET https://httpbin.org/status/404 ===");
    match client.get("https://httpbin.org/status/404") {
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
