# Link Header Parsing

aioduct can parse `Link` headers (RFC 8288) from HTTP responses. Link headers are commonly used for pagination, resource discovery, and relation metadata.

## Parsing Link Headers

Use `Response::links()` to extract all Link header values:

```rust,no_run
use aioduct::{Client, Link};
use aioduct::runtime::TokioRuntime;

# async fn example() -> Result<(), aioduct::Error> {
let client = Client::<TokioRuntime>::new();
let resp = client.get("https://api.example.com/items?page=1")?
    .send()
    .await?;

for link in resp.links() {
    println!("URI: {}", link.uri);
    if let Some(ref rel) = link.rel {
        println!("  rel: {rel}");
    }
}
# Ok(())
# }
```

## Link Fields

The `Link` struct contains:

| Field | Type | Description |
|-------|------|-------------|
| `uri` | `String` | The target URI |
| `rel` | `Option<String>` | Relation type (e.g., `next`, `prev`, `last`) |
| `title` | `Option<String>` | Human-readable title |
| `media_type` | `Option<String>` | Expected media type of the target |
| `anchor` | `Option<String>` | Context URI for the link |

## Common Patterns

### Pagination

Many APIs use Link headers for pagination:

```text
Link: <https://api.example.com/items?page=2>; rel="next",
      <https://api.example.com/items?page=5>; rel="last"
```

```rust,no_run
# use aioduct::response::Response;
fn next_page_url(resp: &Response) -> Option<String> {
    resp.links()
        .into_iter()
        .find(|l| l.rel.as_deref() == Some("next"))
        .map(|l| l.uri)
}
```

### Direct Parsing

You can also parse Link headers directly from a `HeaderMap`:

```rust
use aioduct::link::parse_link_headers;
use http::HeaderMap;

let mut headers = HeaderMap::new();
headers.insert(
    "link",
    "<https://example.com>; rel=\"canonical\"".parse().unwrap(),
);

let links = parse_link_headers(&headers);
assert_eq!(links[0].rel.as_deref(), Some("canonical"));
```
