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

## Using NetrcMiddleware

The simplest approach is to add `NetrcMiddleware` to your client. It reads the netrc file once and injects Basic Auth headers for matching hosts:

```rust,no_run
use aioduct::Client;
use aioduct::runtime::TokioRuntime;
use aioduct::NetrcMiddleware;

let client = Client::<TokioRuntime>::builder()
    .middleware(NetrcMiddleware::from_default().unwrap())
    .build();

// Requests to api.example.com automatically get Basic Auth
let resp = client
    .get("https://api.example.com/data")?
    .send()
    .await?;
```

## Loading from a Specific Path

```rust,no_run
use std::path::Path;
use aioduct::NetrcMiddleware;

let middleware = NetrcMiddleware::from_path(Path::new("/etc/netrc")).unwrap();
```

## Parsing Directly

You can also use the `Netrc` type directly for credential lookup without middleware:

```rust,no_run
use aioduct::Netrc;

let netrc = Netrc::parse(
    "machine example.com login user1 password pass1\n\
     default login anon password anon\n"
);
```

## Behavior

- If a request already has an `Authorization` header, the middleware does not overwrite it.
- Machine names are matched exactly against the request URI's host.
- The `default` entry matches any host not explicitly listed.
- Both `password` and `passwd` keywords are accepted.
- The `account` and `macdef` keywords are recognized and skipped.
