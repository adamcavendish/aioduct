use http::HeaderMap;

/// A parsed HTTP `Link` header entry (RFC 8288).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Link {
    uri: String,
    rel: Option<String>,
    title: Option<String>,
    media_type: Option<String>,
    anchor: Option<String>,
}

impl Link {
    /// The target URI of this link.
    pub fn uri(&self) -> &str {
        &self.uri
    }

    /// The link relation type (e.g., "next", "prev", "stylesheet").
    pub fn rel(&self) -> Option<&str> {
        self.rel.as_deref()
    }

    /// The human-readable title of the link.
    pub fn title(&self) -> Option<&str> {
        self.title.as_deref()
    }

    /// The media type hint (e.g., "text/html").
    pub fn media_type(&self) -> Option<&str> {
        self.media_type.as_deref()
    }

    /// The anchor (context IRI) for the link.
    pub fn anchor(&self) -> Option<&str> {
        self.anchor.as_deref()
    }
}

/// Parse all `Link` headers from a response header map.
pub fn parse_link_headers(headers: &HeaderMap) -> Vec<Link> {
    let mut links = Vec::new();
    for value in headers.get_all(http::header::LINK) {
        if let Ok(s) = value.to_str() {
            parse_link_value(s, &mut links);
        }
    }
    links
}

fn parse_link_value(s: &str, links: &mut Vec<Link>) {
    for entry in split_links(s) {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }

        let Some(uri_end) = entry.find('>') else {
            continue;
        };
        let uri_part = entry.get(..uri_end).unwrap_or("");
        let uri = uri_part.trim_start_matches('<').trim();
        if uri.is_empty() {
            continue;
        }

        let mut link = Link {
            uri: uri.to_owned(),
            rel: None,
            title: None,
            media_type: None,
            anchor: None,
        };

        let params_str = entry.get(uri_end + 1..).unwrap_or("");
        for param in params_str.split(';') {
            let param = param.trim();
            if let Some((key, val)) = param.split_once('=') {
                let key = key.trim().to_lowercase();
                let val = val.trim().trim_matches('"');
                match key.as_str() {
                    "rel" => link.rel = Some(val.to_owned()),
                    "title" => link.title = Some(val.to_owned()),
                    "type" => link.media_type = Some(val.to_owned()),
                    "anchor" => link.anchor = Some(val.to_owned()),
                    _ => {}
                }
            }
        }

        links.push(link);
    }
}

fn split_links(s: &str) -> Vec<&str> {
    let mut results = Vec::new();
    let mut depth = 0;
    let mut start = 0;
    for (i, ch) in s.char_indices() {
        match ch {
            '<' => depth += 1,
            '>' => depth -= 1,
            ',' if depth == 0 => {
                results.push(&s[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    results.push(&s[start..]);
    results
}

#[cfg(test)]
mod tests {
    use super::*;
    use http::header::HeaderValue;

    fn link_headers(values: &[&str]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for v in values {
            headers.append(http::header::LINK, HeaderValue::from_str(v).unwrap());
        }
        headers
    }

    #[test]
    fn single_link() {
        let headers = link_headers(&["<https://example.com/next>; rel=\"next\""]);
        let links = parse_link_headers(&headers);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].uri(), "https://example.com/next");
        assert_eq!(links[0].rel(), Some("next"));
    }

    #[test]
    fn multiple_links_single_header() {
        let headers = link_headers(&[
            "<https://example.com/1>; rel=\"next\", <https://example.com/0>; rel=\"prev\"",
        ]);
        let links = parse_link_headers(&headers);
        assert_eq!(links.len(), 2);
        assert_eq!(links[0].rel(), Some("next"));
        assert_eq!(links[1].rel(), Some("prev"));
    }

    #[test]
    fn multiple_link_headers() {
        let headers = link_headers(&[
            "<https://example.com/1>; rel=\"next\"",
            "<https://example.com/0>; rel=\"prev\"",
        ]);
        let links = parse_link_headers(&headers);
        assert_eq!(links.len(), 2);
    }

    #[test]
    fn link_with_type_and_title() {
        let headers = link_headers(&[
            "<https://example.com/style.css>; rel=\"stylesheet\"; type=\"text/css\"; title=\"Main\"",
        ]);
        let links = parse_link_headers(&headers);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].rel(), Some("stylesheet"));
        assert_eq!(links[0].media_type(), Some("text/css"));
        assert_eq!(links[0].title(), Some("Main"));
    }

    #[test]
    fn link_with_anchor() {
        let headers =
            link_headers(&["<https://example.com/license>; rel=\"license\"; anchor=\"#section1\""]);
        let links = parse_link_headers(&headers);
        assert_eq!(links[0].anchor(), Some("#section1"));
    }

    #[test]
    fn empty_link_header() {
        let headers = HeaderMap::new();
        let links = parse_link_headers(&headers);
        assert!(links.is_empty());
    }

    #[test]
    fn malformed_link_skipped() {
        let headers = link_headers(&["not a link, <https://example.com>; rel=\"valid\""]);
        let links = parse_link_headers(&headers);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].uri(), "https://example.com");
    }

    #[test]
    fn relative_uri() {
        let headers = link_headers(&["</page/2>; rel=\"next\""]);
        let links = parse_link_headers(&headers);
        assert_eq!(links[0].uri(), "/page/2");
        assert_eq!(links[0].rel(), Some("next"));
    }
}
