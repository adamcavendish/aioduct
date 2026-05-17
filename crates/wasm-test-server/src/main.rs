use std::convert::Infallible;
use std::net::SocketAddr;
use std::time::Duration;

use aioduct::runtime::tokio_rt::TokioIo;
use bytes::Bytes;
use http::header::HeaderValue;
use http_body_util::Full;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Method, Request, Response};
use tokio::net::TcpListener;

const BIND_ADDR: &str = "127.0.0.1:9877";

fn cors_headers(resp: &mut Response<Full<Bytes>>) {
    let h = resp.headers_mut();
    h.insert("access-control-allow-origin", HeaderValue::from_static("*"));
    h.insert(
        "access-control-allow-methods",
        HeaderValue::from_static("GET, POST, PUT, PATCH, DELETE, HEAD, OPTIONS"),
    );
    h.insert(
        "access-control-allow-headers",
        HeaderValue::from_static("*"),
    );
    h.insert(
        "access-control-expose-headers",
        HeaderValue::from_static("*"),
    );
}

async fn handler(req: Request<hyper::body::Incoming>) -> Result<Response<Full<Bytes>>, Infallible> {
    if req.method() == Method::OPTIONS {
        let mut resp = Response::new(Full::new(Bytes::new()));
        cors_headers(&mut resp);
        return Ok(resp);
    }

    let path = req.uri().path().to_string();
    let method = req.method().clone();

    let mut resp = match path.as_str() {
        "/hello" => Response::new(Full::new(Bytes::from("hello aioduct"))),

        "/echo-method" => {
            let mut r = Response::new(Full::new(Bytes::from(method.to_string())));
            r.headers_mut().insert(
                "x-echo-method",
                HeaderValue::from_str(method.as_str()).unwrap(),
            );
            r
        }

        "/echo-headers" => {
            let mut lines = Vec::new();
            for (name, value) in req.headers() {
                if let Ok(v) = value.to_str() {
                    lines.push(format!("{name}: {v}"));
                }
            }
            lines.sort();
            Response::new(Full::new(Bytes::from(lines.join("\n"))))
        }

        p if p.starts_with("/status/") => {
            let code: u16 = p["/status/".len()..].parse().unwrap_or(200);
            let mut r = Response::new(Full::new(Bytes::new()));
            *r.status_mut() = http::StatusCode::from_u16(code).unwrap_or(http::StatusCode::OK);
            r
        }

        "/echo-body" => {
            use http_body_util::BodyExt;
            let body = req
                .into_body()
                .collect()
                .await
                .map(|c| c.to_bytes())
                .unwrap_or_default();
            Response::new(Full::new(body))
        }

        "/echo-url" => {
            let uri = req.uri().to_string();
            Response::new(Full::new(Bytes::from(uri)))
        }

        p if p.starts_with("/delay/") => {
            let ms: u64 = p["/delay/".len()..].parse().unwrap_or(0);
            tokio::time::sleep(Duration::from_millis(ms)).await;
            Response::new(Full::new(Bytes::from("delayed")))
        }

        p if p.starts_with("/bytes/") => {
            let n: usize = p["/bytes/".len()..].parse().unwrap_or(0);
            let body = vec![0xABu8; n];
            Response::new(Full::new(Bytes::from(body)))
        }

        "/response-headers" => {
            let mut r = Response::new(Full::new(Bytes::from("ok")));
            if let Some(query) = req.uri().query() {
                for pair in query.split('&') {
                    if let Some((k, v)) = pair.split_once('=')
                        && let (Ok(name), Ok(val)) = (
                            k.parse::<http::header::HeaderName>(),
                            HeaderValue::from_str(v),
                        )
                    {
                        r.headers_mut().insert(name, val);
                    }
                }
            }
            r
        }

        _ => {
            let mut r = Response::new(Full::new(Bytes::from("not found")));
            *r.status_mut() = http::StatusCode::NOT_FOUND;
            r
        }
    };

    cors_headers(&mut resp);
    Ok(resp)
}

#[tokio::main]
async fn main() {
    let addr: SocketAddr = BIND_ADDR.parse().unwrap();
    let listener = TcpListener::bind(addr).await.unwrap();
    eprintln!("wasm-test-server listening on {addr}");

    tokio::spawn(async move {
        loop {
            let (stream, _) = match listener.accept().await {
                Ok(conn) => conn,
                Err(_) => continue,
            };
            tokio::spawn(async move {
                let io = TokioIo::new(stream);
                let _ = http1::Builder::new()
                    .serve_connection(io, service_fn(handler))
                    .await;
            });
        }
    });

    tokio::signal::ctrl_c().await.ok();
}
