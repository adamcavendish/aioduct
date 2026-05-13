//! Multi-runtime test infrastructure.
//!
//! Provides `runtime_test!` macro that stamps out a test for each supported
//! async runtime (tokio, smol). The test body is written once; the macro
//! generates runtime-specific wrappers.
//!
//! The server always runs on a background tokio thread — it's just a TCP
//! listener, so any client runtime can connect to it.

/// Start a tokio-based test server on a background thread. Returns the
/// `SocketAddr` the server is listening on. Works regardless of the calling
/// async runtime because it spawns its own tokio runtime in a dedicated thread.
pub fn spawn_server_with<F, Fut>(handler: F) -> std::net::SocketAddr
where
    F: Fn(hyper::Request<hyper::body::Incoming>) -> Fut + Send + Clone + 'static,
    Fut: std::future::Future<
            Output = Result<
                hyper::Response<http_body_util::Full<bytes::Bytes>>,
                std::convert::Infallible,
            >,
        > + Send,
{
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            tx.send(addr).unwrap();

            loop {
                let (stream, _) = listener.accept().await.unwrap();
                let io = aioduct::runtime::tokio_rt::TokioIo::new(stream);
                let handler = handler.clone();
                tokio::spawn(async move {
                    let _ = hyper::server::conn::http1::Builder::new()
                        .serve_connection(io, hyper::service::service_fn(handler))
                        .await;
                });
            }
        });
    });
    rx.recv().unwrap()
}

/// Convenience: start a server that returns "hello aioduct" for every request.
pub fn spawn_server() -> std::net::SocketAddr {
    spawn_server_with(|_req| async {
        Ok::<_, std::convert::Infallible>(hyper::Response::new(http_body_util::Full::new(
            bytes::Bytes::from("hello aioduct"),
        )))
    })
}

/// Stamps out a test function for each supported runtime.
///
/// # Usage
///
/// ```ignore
/// runtime_test! {
///     async fn test_basic_get() {
///         let addr = spawn_server();
///         let client = new_client();
///         let resp = client
///             .get(&format!("http://{addr}/"))
///             .unwrap()
///             .send()
///             .await
///             .unwrap();
///         assert_eq!(resp.status(), http::StatusCode::OK);
///     }
/// }
/// ```
///
/// Inside the test body:
/// - `new_client()` creates a default `HttpEngine` for the current runtime
/// - `new_client_builder()` returns an `HttpEngineBuilder` for the current runtime
/// - `spawn_server()` / `spawn_server_with(handler)` start a test server
///   (these are runtime-agnostic, from the `common::multi_runtime` module)
#[macro_export]
macro_rules! runtime_test {
    (
        $(
            $(#[$meta:meta])*
            async fn $name:ident() $body:block
        )*
    ) => {
        $(
            #[cfg(feature = "tokio")]
            paste::paste! {
                #[tokio::test]
                $(#[$meta])*
                async fn [<$name _tokio>]() {
                    #[allow(unused)]
                    fn new_client() -> aioduct::HttpEngine<
                        aioduct::runtime::TokioRuntime,
                        aioduct::runtime::tokio_rt::TcpConnector,
                    > {
                        aioduct::HttpEngine::new(aioduct::runtime::tokio_rt::TcpConnector)
                    }

                    #[allow(unused)]
                    fn new_client_builder() -> aioduct::HttpEngineBuilder<
                        aioduct::runtime::TokioRuntime,
                        aioduct::runtime::tokio_rt::TcpConnector,
                    > {
                        aioduct::HttpEngine::builder(aioduct::runtime::tokio_rt::TcpConnector)
                    }

                    $body
                }
            }

            #[cfg(feature = "smol")]
            paste::paste! {
                #[test]
                $(#[$meta])*
                fn [<$name _smol>]() {
                    smol::block_on(async {
                        #[allow(unused)]
                        fn new_client() -> aioduct::HttpEngine<
                            aioduct::runtime::smol_rt::SmolRuntime,
                            aioduct::runtime::smol_rt::TcpConnector,
                        > {
                            aioduct::HttpEngine::new(aioduct::runtime::smol_rt::TcpConnector)
                        }

                        #[allow(unused)]
                        fn new_client_builder() -> aioduct::HttpEngineBuilder<
                            aioduct::runtime::smol_rt::SmolRuntime,
                            aioduct::runtime::smol_rt::TcpConnector,
                        > {
                            aioduct::HttpEngine::builder(aioduct::runtime::smol_rt::TcpConnector)
                        }

                        $body
                    });
                }
            }
        )*
    };
}
