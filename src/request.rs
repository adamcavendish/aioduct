use std::marker::PhantomData;
use std::time::Duration;

use bytes::Bytes;
use http::header::{HeaderMap, HeaderName, HeaderValue};
use http::{Method, Uri, Version};

use crate::client::Client;
use crate::error::{Error, Result};
use crate::response::Response;
use crate::runtime::Runtime;
use crate::timeout::Timeout;

pub struct RequestBuilder<'a, R: Runtime> {
    client: &'a Client<R>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Option<Bytes>,
    version: Option<Version>,
    timeout: Option<Duration>,
    _runtime: PhantomData<R>,
}

impl<'a, R: Runtime> RequestBuilder<'a, R> {
    pub(crate) fn new(client: &'a Client<R>, method: Method, uri: Uri) -> Self {
        Self {
            client,
            method,
            uri,
            headers: HeaderMap::new(),
            body: None,
            version: None,
            timeout: None,
            _runtime: PhantomData,
        }
    }

    pub fn header(mut self, name: HeaderName, value: HeaderValue) -> Self {
        self.headers.insert(name, value);
        self
    }

    pub fn headers(mut self, headers: HeaderMap) -> Self {
        self.headers.extend(headers);
        self
    }

    pub fn header_str(mut self, name: &str, value: &str) -> Result<Self> {
        let name: HeaderName = name.parse().map_err(|e| Error::Other(Box::new(e)))?;
        let value: HeaderValue = value.parse().map_err(|e| Error::Other(Box::new(e)))?;
        self.headers.insert(name, value);
        Ok(self)
    }

    pub fn body(mut self, body: impl Into<Bytes>) -> Self {
        self.body = Some(body.into());
        self
    }

    pub fn version(mut self, version: Version) -> Self {
        self.version = Some(version);
        self
    }

    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    pub async fn send(self) -> Result<Response> {
        let effective_timeout = self.timeout.or(self.client.default_timeout());
        let execute_fut =
            self.client
                .execute(self.method, self.uri, self.headers, self.body, self.version);

        match effective_timeout {
            Some(duration) => {
                Timeout::WithTimeout {
                    future: execute_fut,
                    sleep: R::sleep(duration),
                }
                .await
            }
            None => {
                Timeout::<_, R::Sleep>::NoTimeout {
                    future: execute_fut,
                }
                .await
            }
        }
    }
}
