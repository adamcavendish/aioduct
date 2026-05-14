use std::collections::HashMap;
use std::sync::Mutex;

use aioduct::runtime::compio_rt::TcpConnector;
use aioduct::{CacheEntry, CacheStore, CompioClient, HttpCache, Method, Uri};

/// A custom cache store that logs every operation and wraps a simple HashMap.
struct LoggingCacheStore {
    entries: Mutex<HashMap<(Method, Uri), CacheEntry>>,
}

impl LoggingCacheStore {
    fn new() -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
        }
    }
}

impl CacheStore for LoggingCacheStore {
    fn get(&self, method: &Method, uri: &Uri) -> Option<CacheEntry> {
        let entries = self.entries.lock().unwrap();
        let result = entries.get(&(method.clone(), uri.clone())).cloned();
        println!(
            "[cache] GET {method} {uri} -> {}",
            if result.is_some() { "HIT" } else { "MISS" }
        );
        result
    }

    fn put(&self, method: &Method, uri: &Uri, entry: CacheEntry) {
        println!("[cache] PUT {method} {uri}");
        self.entries
            .lock()
            .unwrap()
            .insert((method.clone(), uri.clone()), entry);
    }

    fn remove(&self, method: &Method, uri: &Uri) {
        println!("[cache] REMOVE {method} {uri}");
        self.entries
            .lock()
            .unwrap()
            .remove(&(method.clone(), uri.clone()));
    }

    fn clear(&self) {
        println!("[cache] CLEAR");
        self.entries.lock().unwrap().clear();
    }

    fn len(&self) -> usize {
        self.entries.lock().unwrap().len()
    }
}

fn main() -> Result<(), aioduct::Error> {
    compio_runtime::Runtime::new().unwrap().block_on(async {
        let store = LoggingCacheStore::new();
        let cache = HttpCache::with_store(store);
        let client = CompioClient::builder_local(TcpConnector)
            .cache(cache)
            .build_local();

        // First request — hits the server, stores in the custom cache
        let resp = client
            .get_local("https://httpbin.org/cache/60")?
            .send()
            .await?;
        println!("First request: {}", resp.status());
        let body1 = resp.text().await?;

        // Second request — served from the custom cache (no network round-trip)
        let resp = client
            .get_local("https://httpbin.org/cache/60")?
            .send()
            .await?;
        println!("Second request: {}", resp.status());
        let body2 = resp.text().await?;

        assert_eq!(body1, body2);
        println!("Cache hit confirmed — bodies match");

        Ok(())
    })
}
