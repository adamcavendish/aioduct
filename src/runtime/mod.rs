use std::future::Future;
use std::io;
use std::marker::PhantomData;
use std::net::SocketAddr;
use std::time::Duration;

/// Abstraction over async runtimes (tokio, smol, compio).
#[allow(async_fn_in_trait)]
pub trait Runtime: Send + Sync + 'static {
    type TcpStream: hyper::rt::Read + hyper::rt::Write + Send + Unpin + 'static;
    type Sleep: Future<Output = ()> + Send;

    fn connect(addr: SocketAddr) -> impl Future<Output = io::Result<Self::TcpStream>> + Send;
    fn resolve(host: &str, port: u16) -> impl Future<Output = io::Result<SocketAddr>> + Send;
    fn sleep(duration: Duration) -> Self::Sleep;
    fn spawn<F>(future: F)
    where
        F: Future<Output = ()> + Send + 'static;
}

/// Executor adapter that delegates to `R::spawn` for hyper's HTTP/2 handshake.
pub struct HyperExecutor<R>(PhantomData<fn() -> R>);

impl<R> Clone for HyperExecutor<R> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<R> Copy for HyperExecutor<R> {}

impl<R, F> hyper::rt::Executor<F> for HyperExecutor<R>
where
    R: Runtime,
    F: Future<Output = ()> + Send + 'static,
{
    fn execute(&self, fut: F) {
        R::spawn(fut);
    }
}

/// Create a [`HyperExecutor`] for the given runtime.
pub fn hyper_executor<R: Runtime>() -> HyperExecutor<R> {
    HyperExecutor(PhantomData)
}

#[cfg(feature = "tokio")]
pub mod tokio_rt;
#[cfg(feature = "tokio")]
pub use tokio_rt::TokioRuntime;

#[cfg(feature = "smol")]
pub mod smol_rt;
#[cfg(feature = "smol")]
pub use smol_rt::SmolRuntime;

#[cfg(feature = "compio")]
pub mod compio_rt;
#[cfg(feature = "compio")]
pub use compio_rt::CompioRuntime;
