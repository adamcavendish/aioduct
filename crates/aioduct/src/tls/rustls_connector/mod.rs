mod config;
mod stream;
mod verifier;

pub use config::{AlpnProtocol, RustlsConnector};
pub use stream::TlsStream;

#[cfg(all(test, feature = "rustls", feature = "tokio"))]
use stream::{read_tls, write_tls};
#[cfg(all(test, feature = "rustls", feature = "tokio"))]
use verifier::NoVerifier;

#[cfg(all(test, feature = "rustls", feature = "tokio"))]
mod tests;
