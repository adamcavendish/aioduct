use std::fmt::Write as _;
use std::marker::PhantomData;
use std::time::Duration;

use bytes::Bytes;
use http::header::{AUTHORIZATION, HeaderMap, HeaderName, HeaderValue};
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

    pub fn bearer_auth(mut self, token: &str) -> Self {
        let value = HeaderValue::from_str(&format!("Bearer {token}")).expect("valid bearer token");
        self.headers.insert(AUTHORIZATION, value);
        self
    }

    pub fn basic_auth(mut self, username: &str, password: Option<&str>) -> Self {
        use base64::engine::{Engine, general_purpose::STANDARD};
        let credentials = match password {
            Some(pw) => format!("{username}:{pw}"),
            None => format!("{username}:"),
        };
        let encoded = STANDARD.encode(credentials);
        let value =
            HeaderValue::from_str(&format!("Basic {encoded}")).expect("valid basic auth header");
        self.headers.insert(AUTHORIZATION, value);
        self
    }

    pub fn query(mut self, params: &[(&str, &str)]) -> Self {
        use percent_encoding::{AsciiSet, CONTROLS, utf8_percent_encode};
        const QUERY_ENCODE: &AsciiSet = &CONTROLS
            .add(b' ')
            .add(b'"')
            .add(b'#')
            .add(b'<')
            .add(b'>')
            .add(b'&')
            .add(b'=')
            .add(b'+');

        let mut uri_str = self.uri.to_string();
        let has_query = self.uri.query().is_some();
        for (i, (key, val)) in params.iter().enumerate() {
            let sep = if i == 0 && !has_query { '?' } else { '&' };
            let key = utf8_percent_encode(key, QUERY_ENCODE);
            let val = utf8_percent_encode(val, QUERY_ENCODE);
            write!(uri_str, "{sep}{key}={val}").unwrap();
        }
        if let Ok(new_uri) = uri_str.parse() {
            self.uri = new_uri;
        }
        self
    }

    pub fn body(mut self, body: impl Into<Bytes>) -> Self {
        self.body = Some(body.into());
        self
    }

    #[cfg(feature = "json")]
    pub fn json(mut self, value: &impl serde::Serialize) -> Result<Self> {
        let bytes = serde_json::to_vec(value).map_err(|e| Error::Other(Box::new(e)))?;
        self.headers.insert(
            http::header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );
        self.body = Some(bytes.into());
        Ok(self)
    }

    pub fn form(mut self, params: &[(&str, &str)]) -> Self {
        use percent_encoding::{AsciiSet, CONTROLS, utf8_percent_encode};
        const FORM_ENCODE: &AsciiSet = &CONTROLS
            .add(b' ')
            .add(b'"')
            .add(b'#')
            .add(b'<')
            .add(b'>')
            .add(b'&')
            .add(b'=')
            .add(b'+')
            .add(b'%');

        let mut encoded = String::new();
        for (i, (key, val)) in params.iter().enumerate() {
            if i > 0 {
                encoded.push('&');
            }
            let k = utf8_percent_encode(key, FORM_ENCODE);
            let v = utf8_percent_encode(val, FORM_ENCODE);
            write!(encoded, "{k}={v}").unwrap();
        }
        self.headers.insert(
            http::header::CONTENT_TYPE,
            HeaderValue::from_static("application/x-www-form-urlencoded"),
        );
        self.body = Some(encoded.into());
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
