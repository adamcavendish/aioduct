use aioduct::runtime::TokioRuntime;

use crate::cli::Cli;

pub struct ExtraRequestConfig {
    headers: Vec<(http::HeaderName, http::HeaderValue)>,
    auth: Option<(String, String)>,
}

impl ExtraRequestConfig {
    pub fn from_cli(cli: &Cli) -> Self {
        let mut headers = Vec::new();
        for h in &cli.headers {
            if let Some((name, value)) = h.split_once(':')
                && let (Ok(n), Ok(v)) = (
                    name.trim().parse::<http::HeaderName>(),
                    value.trim().parse::<http::HeaderValue>(),
                )
            {
                headers.push((n, v));
            }
        }
        if let Some(ref referer) = cli.referer
            && let Ok(v) = referer.parse::<http::HeaderValue>()
        {
            headers.push((http::header::REFERER, v));
        }
        let auth = match (&cli.http_user, &cli.http_passwd) {
            (Some(u), Some(p)) => Some((u.clone(), p.clone())),
            _ => None,
        };
        Self { headers, auth }
    }

    pub fn apply_to<'a>(
        &self,
        mut req: aioduct::RequestBuilder<'a, TokioRuntime>,
    ) -> aioduct::RequestBuilder<'a, TokioRuntime> {
        for (name, value) in &self.headers {
            req = req.header(name.clone(), value.clone());
        }
        if let Some((user, pass)) = &self.auth {
            req = req.basic_auth(user, Some(pass.as_str()));
        }
        req
    }
}
