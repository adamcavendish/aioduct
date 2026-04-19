use std::marker::PhantomData;

use crate::runtime::Runtime;

pub enum HttpConnection {
    H1(hyper::client::conn::http1::SendRequest<crate::error::HyperBody>),
    H2(hyper::client::conn::http2::SendRequest<crate::error::HyperBody>),
    #[cfg(feature = "http3")]
    H3(h3::client::SendRequest<h3_quinn::OpenStreams, bytes::Bytes>),
}

pub struct PooledConnection<R: Runtime> {
    pub(crate) conn: HttpConnection,
    _runtime: PhantomData<R>,
}

impl<R: Runtime> PooledConnection<R> {
    pub fn new_h1(
        sender: hyper::client::conn::http1::SendRequest<crate::error::HyperBody>,
    ) -> Self {
        Self {
            conn: HttpConnection::H1(sender),
            _runtime: PhantomData,
        }
    }

    pub fn new_h2(
        sender: hyper::client::conn::http2::SendRequest<crate::error::HyperBody>,
    ) -> Self {
        Self {
            conn: HttpConnection::H2(sender),
            _runtime: PhantomData,
        }
    }

    #[cfg(feature = "http3")]
    pub fn new_h3(sender: h3::client::SendRequest<h3_quinn::OpenStreams, bytes::Bytes>) -> Self {
        Self {
            conn: HttpConnection::H3(sender),
            _runtime: PhantomData,
        }
    }

    pub fn is_ready(&self) -> bool {
        match &self.conn {
            HttpConnection::H1(s) => s.is_ready(),
            HttpConnection::H2(s) => s.is_ready(),
            #[cfg(feature = "http3")]
            HttpConnection::H3(_) => true,
        }
    }
}
