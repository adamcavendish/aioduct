use std::marker::PhantomData;

use bytes::Bytes;
use http::header::{HOST, HeaderMap, HeaderName, HeaderValue};
use http::{Method, Uri, Version};
use http_body_util::BodyExt;

use crate::client::Client;
use crate::error::{Error, HyperBody, Result};
use crate::response::Response;
use crate::runtime::Runtime;

pub struct RequestBuilder<'a, R: Runtime> {
    client: &'a Client<R>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Option<Bytes>,
    version: Option<Version>,
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
            _runtime: PhantomData,
        }
    }

    pub fn header(mut self, name: HeaderName, value: HeaderValue) -> Self {
        self.headers.insert(name, value);
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

    pub async fn send(self) -> Result<Response> {
        let body_bytes = self.body.unwrap_or_default();
        let body: HyperBody = http_body_util::Full::new(body_bytes)
            .map_err(|never| match never {})
            .boxed();

        let mut headers = self.headers;

        if !headers.contains_key(HOST) {
            if let Some(authority) = self.uri.authority() {
                let host_value: HeaderValue = authority
                    .as_str()
                    .parse()
                    .map_err(|e| Error::Other(Box::new(e)))?;
                headers.insert(HOST, host_value);
            }
        }

        let path_and_query = self
            .uri
            .path_and_query()
            .map(|pq| pq.as_str())
            .unwrap_or("/");
        let req_uri: Uri = path_and_query
            .parse()
            .map_err(|e| Error::Other(Box::new(e)))?;

        let mut builder = http::Request::builder().method(self.method).uri(req_uri);

        if let Some(version) = self.version {
            builder = builder.version(version);
        }

        for (name, value) in &headers {
            builder = builder.header(name, value);
        }

        let request = builder.body(body)?;

        self.client.execute(request, &self.uri).await
    }
}
