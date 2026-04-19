use http::Uri;

use crate::error::{Error, Result};

/// HTTP proxy configuration.
#[derive(Clone, Debug)]
pub struct ProxyConfig {
    pub(crate) uri: Uri,
    pub(crate) auth: Option<ProxyAuth>,
}

#[derive(Clone, Debug)]
pub(crate) struct ProxyAuth {
    pub username: String,
    pub password: String,
}

impl ProxyConfig {
    /// Create a proxy config from an `http://` URI.
    pub fn http(uri: &str) -> Result<Self> {
        let uri: Uri = uri.parse().map_err(|e| Error::InvalidUrl(format!("{e}")))?;
        if uri.scheme_str() != Some("http") {
            return Err(Error::InvalidUrl(
                "proxy URI must use http:// scheme".into(),
            ));
        }
        Ok(Self { uri, auth: None })
    }

    /// Set basic authentication credentials for the proxy.
    pub fn basic_auth(mut self, username: &str, password: &str) -> Self {
        self.auth = Some(ProxyAuth {
            username: username.to_owned(),
            password: password.to_owned(),
        });
        self
    }

    pub(crate) fn authority(&self) -> Result<&http::uri::Authority> {
        self.uri
            .authority()
            .ok_or_else(|| Error::InvalidUrl("proxy URI missing authority".into()))
    }

    pub(crate) fn connect_header(&self, _target_authority: &str) -> Option<String> {
        self.auth.as_ref().map(|auth| {
            use base64::engine::{Engine, general_purpose::STANDARD};
            let credentials = format!("{}:{}", auth.username, auth.password);
            let encoded = STANDARD.encode(credentials);
            format!("Basic {encoded}")
        })
    }
}
