# Netrc Support

aioduct can read `.netrc` files and automatically inject credentials into requests. This follows the same convention used by curl, wget, and other HTTP tools.

## What is `.netrc`?

A `.netrc` file maps hostnames to login credentials:

```text
machine api.example.com
  login myuser
  password mytoken

machine registry.npmjs.org
  login npm_user
  password npm_pass

default
  login anonymous
  password guest
```

The file is typically located at `~/.netrc` (or `%USERPROFILE%\_netrc` on Windows). The `$NETRC` environment variable overrides the default path.

## Configuring a Native Client

Load credentials explicitly, then configure the native client with `.netrc(netrc)`. The client never reads a credential file automatically:

```rust,no_run
use aioduct::TokioClient;
use aioduct::Netrc;

let client = TokioClient::builder()
    .netrc(Netrc::load_default()?)
    .build()?;

// Requests to api.example.com automatically get Basic Auth
let resp = client
    .get("https://api.example.com/data")?
    .send()
    .await?;
```

## Loading from a Specific Path

```rust,no_run
use std::path::Path;
use aioduct::Netrc;

let netrc = Netrc::load(Path::new("/etc/netrc"))?;
```

## Parsing Directly

You can also use the `Netrc` type directly for credential lookup:

```rust,no_run
use aioduct::Netrc;

let netrc = Netrc::parse(
    "machine example.com login user1 password pass1\n\
     default login anon password anon\n"
);
```

## Behavior

- If a request already has an `Authorization` header, netrc does not overwrite it.
- Machine names are matched exactly against the request URI's host.
- The `default` entry matches any host not explicitly listed.
- Both `password` and `passwd` keywords are accepted.
- The `account` and `macdef` keywords are recognized and skipped.

- Credentials are sent only over HTTPS or loopback HTTP.
- Cross-origin redirects clear the old Authorization field before matching the new destination.
- Same-target retries reuse prepared authentication; Digest authentication is not overwritten.
- Native Tokio, smol, compio and their blocking wrappers share this configuration.
  Fetch and WASI guest clients have no automatic netrc integration; applications can use
  the portable parser/lookup with their existing request authentication methods.
