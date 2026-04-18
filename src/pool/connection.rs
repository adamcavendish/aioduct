use std::marker::PhantomData;

use crate::runtime::Runtime;

pub enum HttpConnection {
    H1(hyper::client::conn::http1::SendRequest<crate::error::HyperBody>),
    H2(hyper::client::conn::http2::SendRequest<crate::error::HyperBody>),
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

    pub fn is_ready(&self) -> bool {
        match &self.conn {
            HttpConnection::H1(s) => s.is_ready(),
            HttpConnection::H2(s) => s.is_ready(),
        }
    }
}
