# Problem Details (RFC 9457)

aioduct can parse RFC 9457 Problem Details responses — a standardized JSON format for HTTP API errors with the `application/problem+json` content type.

> Requires the `json` feature.

## Parsing Problem Details

Use `Response::problem_details()` to check and parse a Problem Details response:

```rust,no_run
use aioduct::{Client, ProblemDetails};
use aioduct::runtime::TokioRuntime;

# async fn example() -> Result<(), aioduct::Error> {
let client = Client::<TokioRuntime>::new();
let resp = client.get("https://api.example.com/resource")?
    .send()
    .await?;

if let Some(result) = resp.problem_details().await {
    let problem: ProblemDetails = result?;
    println!("type: {:?}", problem.problem_type);
    println!("title: {:?}", problem.title);
    println!("status: {:?}", problem.status);
    println!("detail: {:?}", problem.detail);
}
# Ok(())
# }
```

The method returns `None` if the `Content-Type` is not `application/problem+json`.

## ProblemDetails Fields

| Field | Type | Description |
|-------|------|-------------|
| `problem_type` | `Option<String>` | A URI identifying the problem type |
| `title` | `Option<String>` | Short human-readable summary |
| `status` | `Option<u16>` | The HTTP status code |
| `detail` | `Option<String>` | Detailed human-readable explanation |
| `instance` | `Option<String>` | URI identifying the specific occurrence |
| `extensions` | `HashMap<String, Value>` | Any additional fields |

## Example Response

A typical Problem Details response:

```json
{
  "type": "https://example.com/probs/out-of-credit",
  "title": "You do not have enough credit.",
  "status": 403,
  "detail": "Your current balance is 30, but that costs 50.",
  "instance": "/account/12345/msgs/abc"
}
```

## Extensions

Any JSON fields beyond the standard five are captured in the `extensions` map:

```rust,no_run
# use aioduct::ProblemDetails;
# fn example(problem: ProblemDetails) {
if let Some(balance) = problem.extensions.get("balance") {
    println!("balance: {balance}");
}
# }
```
