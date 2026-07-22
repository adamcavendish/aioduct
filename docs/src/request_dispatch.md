# Request Dispatch Guarantees

aioduct's native clients share one request-dispatch contract across direct
requests, forwarded requests, pooled connections, retries, and proxies. This
page defines when a request may be sent again, how the outbound protocol is
selected, and which timeout owns connection establishment.

These guarantees apply to the native Tokio, smol, and compio clients. Blocking
clients inherit them from the wrapped native client. Browser Fetch and wasi-p2
use platform-managed transports, so their pooling, proxy, and protocol retry
decisions remain host-controlled.

## Replay Safety

Body replayability and request replay eligibility are different properties:

- An empty body can be reproduced without retaining bytes.
- A buffered body can be reproduced from its stored bytes.
- A streaming body is one-shot unless the transport returns the original,
  untouched request before serialization starts.

A complete request is dispatched again only when its method and retry policy
permit another attempt, its body and aioduct-owned protocol metadata can be
reproduced, and the transport provides sufficient processing evidence. Body
replayability alone never authorizes a retry.

In particular:

- A configured retry never replaces a consumed streaming body with an empty
  body.
- A stale HTTP/1 connection that may have delivered a request does not cause an
  ambiguous non-idempotent request to be sent again.
- HTTP/2 `REFUSED_STREAM` and qualifying GOAWAY boundaries may prove that a
  stream was not processed. HTTP/3 `H3_REQUEST_REJECTED` can provide equivalent
  evidence, but upstream `h3` does not expose enough GOAWAY boundary state to
  authorize replay.
- Replay preserves explicit aioduct-owned protocol metadata, including the
  protocol information needed by HTTP/2 extended CONNECT. Arbitrary user
  extensions are not cloned.

## One-Shot Bodies and Pooling

A one-shot body may start on a ready pooled HTTP/1.1 or HTTP/2 connection.
aioduct uses Hyper's request-returning dispatch operation for this path. If the
connection rejects the request before serialization, Hyper returns the exact
request and aioduct may dispatch that same request once on a fresh connection.

Once serialization is accepted, a later write or connection failure does not
make the body replayable. HTTP/3 does not expose equivalent request recovery,
so one-shot HTTP/3 requests start on a fresh connection.

## Forwarding Translation

Forwarding rewrites the upstream URI and runs `on_request` before final
protocol classification. One final dispatch plan then controls all of the
following:

- origin-form versus full URI request targets;
- request version accepted by the selected encoder;
- exact or negotiable upstream protocol requirements;
- pool identity, ALPN, and connection establishment;
- ordinary requests, HTTP/1.1 upgrades, and HTTP/2 extended CONNECT;
- protocol-aware request and response header cleanup.

HTTP/1.0, HTTP/1.1, HTTP/2, and HTTP/3 ingress can be translated to HTTP/1.1,
HTTP/2, or HTTP/3 egress where the selected request mode is valid. An inbound
version is never passed unchanged to an encoder that cannot represent it.

Hop-by-hop cleanup parses every `Connection` field value and removes every
field it names. Successful upgrades preserve only their required connection
fields. When HTTP/1.1 trailer negotiation applies, aioduct regenerates the
canonical `Connection: TE` and `TE: trailers` fields; HTTP/1.0 egress strips
`TE`. HTTP/2 and HTTP/3 egress may retain only canonical `TE: trailers`, but
actual HTTP/3 trailer frames still fail closed as described in the
[HTTP/3 limitations](http3.md#deferred-protocol-capabilities). Other `TE`
values remain invalid for HTTP/2 and HTTP/3.

Forwarded request bodies stay streaming. A real
`Request<hyper::body::Incoming>` is not collected merely to support retries.

## HTTP/3 Request Lifecycle

HTTP/3 sends request headers before consuming the complete body. The transport
supports data frames followed by the request-stream FIN:

```text
headers -> data* -> FIN
```

The upload and response directions are driven concurrently. An early final
response does not itself stop the upload. After handing the response to the
caller, aioduct continues the upload in a detached supervisor until the body and
FIN complete, the peer sends STOP_SENDING, the request is canceled, or the
upload fails. Producer stalls and QUIC flow-control stalls are separate timeout
conditions: body polling uses the request write timeout, while `send_data` and
`finish` require transport progress within that budget.

Request trailers are not sent with upstream `h3`. A trailer observed
before response handoff fails the request with `Error::Unsupported`, including
when the trailer and final response become ready together. A trailer emitted
after response handoff still fails and cancels the detached upload, but it
cannot retroactively replace the response already returned by `send()`.
Response trailers fail with `Error::Unsupported` when the response body reaches
them. Extended CONNECT, 0-RTT, and GOAWAY-based replay also remain fail-closed or
deferred as described in the
[HTTP/3 limitations](http3.md#deferred-protocol-capabilities).

## Proxy Route Consistency

Each wire dispatch attempt resolves one immutable proxy route before pool
lookup. The snapshot includes the effective destination port, `NO_PROXY`
decision, selected proxy or chain, resolved credentials, route identity, and
protocol policy. The same snapshot controls pooled checkout, exact request
recovery, fresh acquisition, and transparent stale fallback.

A configured retry, digest-auth retry, or redirect hop is a new wire attempt
and resolves a new snapshot. This lets a selector or credential resolver react
between attempts without allowing the pool key and actual route to disagree
within one attempt.

Implicit destination ports participate in `NO_PROXY` matching: HTTP defaults
to 80 and HTTPS defaults to 443. Explicit non-default ports remain distinct.

## Proxy Establishment

HTTP and HTTPS proxy hops establish transparent CONNECT tunnels, including the
final hop to a plain HTTP origin. HTTPS proxies negotiate HTTP/1.1 for the
textual CONNECT exchange. Genuine HTTP/2 proxy CONNECT is a separate future
transport and is not emulated by advertising H2 and writing HTTP/1.1 bytes.

Any successful 2xx CONNECT response establishes a tunnel. Informational,
redirect, client-error, and server-error responses fail. CONNECT parsing is
bounded and leaves bytes following the response header section available to
the tunneled protocol.

One absolute connection-acquisition deadline begins on a pool miss before
connection coordination. It covers:

- waiting for another connection attempt and reserving pool capacity;
- DNS resolution and TCP connection;
- TLS to every HTTPS proxy;
- CONNECT or SOCKS negotiation for every hop;
- TLS to the origin inside the completed tunnel.

A transparent fallback from a stale pooled connection starts a new acquisition
deadline when fresh acquisition begins. Request upload and response body
timeouts remain separate from this connection deadline.

HTTP/3 proxy tunneling through CONNECT-UDP is not supported. A configured proxy
uses the documented TCP transport fallback. Standard absolute-form HTTP
forward-proxy dispatch is also separate from the transparent CONNECT mode.

## Runtime Coverage

| Runtime | Dispatch coverage |
|---------|-------------------|
| Tokio | Send path, HTTP/1.0 semantics, HTTP/1.1, HTTP/2, HTTP/3, TLS, and proxies |
| smol | Shared Send path, HTTP/1.0 semantics, HTTP/1.1, HTTP/2, TLS, and proxies |
| compio | Local path, HTTP/1.0 semantics, HTTP/1.1, HTTP/2, TLS, and proxies |
| blocking | Guarantees inherited from the wrapped native client |
| wasm | Browser-managed Fetch transport |
| wasi-p2 | Host-managed WASI HTTP transport |

Every dispatch change requires focused policy tests and a representative real
transport regression. Protocol tests assert exact origin receipt counts so a
successful retry cannot hide a duplicate side effect.
