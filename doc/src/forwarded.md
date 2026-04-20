# Forwarded Header

aioduct provides a builder and parser for the `Forwarded` HTTP header (RFC 7239), which standardizes proxy-related metadata previously carried by `X-Forwarded-For`, `X-Forwarded-Proto`, and `X-Forwarded-Host`.

## Building Forwarded Headers

Use `ForwardedElement` to construct header values:

```rust
use aioduct::ForwardedElement;

let elem = ForwardedElement::new()
    .forwarded_for("192.0.2.60")
    .proto("https")
    .host("example.com");

assert_eq!(
    elem.to_header_value(),
    "for=192.0.2.60;host=example.com;proto=https"
);
```

## Parameters

Each `ForwardedElement` supports four parameters:

| Method | Parameter | Description |
|--------|-----------|-------------|
| `by()` | `by` | The proxy that received the request |
| `forwarded_for()` | `for` | The client that made the request |
| `host()` | `host` | The original `Host` header value |
| `proto()` | `proto` | The protocol used (`http` or `https`) |

## IPv6 Addresses

IPv6 addresses are automatically quoted and bracketed per the RFC:

```rust
use std::net::IpAddr;
use aioduct::ForwardedElement;

let ip: IpAddr = "2001:db8::1".parse().unwrap();
let elem = ForwardedElement::new().forwarded_for_ip(ip);
assert_eq!(elem.to_header_value(), r#"for="[2001:db8::1]""#);
```

## Multiple Hops

Use `format_forwarded()` to join multiple elements (one per proxy hop):

```rust
use aioduct::forwarded::{ForwardedElement, format_forwarded};

let elems = vec![
    ForwardedElement::new().forwarded_for("192.0.2.43"),
    ForwardedElement::new().forwarded_for("198.51.100.17"),
];
assert_eq!(
    format_forwarded(&elems),
    "for=192.0.2.43, for=198.51.100.17"
);
```

## Parsing

Parse a `Forwarded` header value back into elements:

```rust
use aioduct::forwarded::parse_forwarded;

let elems = parse_forwarded("for=192.0.2.60;proto=https, for=198.51.100.17");
assert_eq!(elems.len(), 2);
assert_eq!(elems[0].forwarded_for.as_deref(), Some("192.0.2.60"));
assert_eq!(elems[0].proto.as_deref(), Some("https"));
```
