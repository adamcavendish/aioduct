use aioduct::TokioClient;
use quick_xml::Reader;
use quick_xml::events::Event;

use super::request_config::ExtraRequestConfig;

pub struct RemoteFile {
    pub url: String,
    pub relative_path: String,
    pub size: Option<u64>,
}

pub async fn enumerate(
    client: &TokioClient,
    base_url: &str,
    extra: &ExtraRequestConfig,
    max_depth: Option<u32>,
) -> Result<Vec<RemoteFile>, aioduct::Error> {
    let mut results = Vec::new();
    let mut queue: Vec<(String, u32)> = vec![(base_url.to_string(), 0)];

    while let Some((url, depth)) = queue.pop() {
        if let Some(max) = max_depth
            && max > 0
            && depth > max
        {
            continue;
        }

        let entries = propfind(client, &url, extra).await?;

        for entry in entries {
            if entry.is_collection {
                // Don't recurse into the directory itself (href == request URL)
                if entry.href != url && !is_self_reference(&url, &entry.href) {
                    queue.push((entry.href, depth + 1));
                }
            } else {
                let relative = entry
                    .href
                    .strip_prefix(base_url)
                    .unwrap_or(&entry.href)
                    .trim_start_matches('/')
                    .to_string();
                results.push(RemoteFile {
                    url: entry.href,
                    relative_path: relative,
                    size: entry.content_length,
                });
            }
        }
    }

    Ok(results)
}

struct DavEntry {
    href: String,
    is_collection: bool,
    content_length: Option<u64>,
}

async fn propfind(
    client: &TokioClient,
    url: &str,
    extra: &ExtraRequestConfig,
) -> Result<Vec<DavEntry>, aioduct::Error> {
    let body = r#"<?xml version="1.0" encoding="utf-8"?>
<D:propfind xmlns:D="DAV:">
  <D:allprop/>
</D:propfind>"#;

    let mut req = client.request(http::Method::from_bytes(b"PROPFIND").unwrap(), url)?;
    req = req.header(
        http::HeaderName::from_static("depth"),
        http::HeaderValue::from_static("1"),
    );
    req = req.header(
        http::header::CONTENT_TYPE,
        http::HeaderValue::from_static("application/xml"),
    );
    req = extra.apply_to(req);
    req = req.body(body.as_bytes().to_vec());

    let resp = req.send().await?;
    let status = resp.status().as_u16();

    if status != 207 && !resp.status().is_success() {
        return Err(aioduct::Error::Status(resp.status()));
    }

    let xml = resp.text().await?;
    parse_multistatus(&xml, url)
}

fn parse_multistatus(xml: &str, base_url: &str) -> Result<Vec<DavEntry>, aioduct::Error> {
    let mut reader = Reader::from_str(xml);
    let mut entries = Vec::new();

    let mut in_response = false;
    let mut in_href = false;
    let mut in_resourcetype = false;
    let mut in_contentlength = false;

    let mut current_href = String::new();
    let mut current_is_collection = false;
    let mut current_length: Option<u64> = None;

    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => {
                let name = e.name();
                let local = local_name(name.as_ref());
                match local {
                    b"response" => {
                        in_response = true;
                        current_href.clear();
                        current_is_collection = false;
                        current_length = None;
                    }
                    b"href" if in_response => {
                        in_href = true;
                    }
                    b"resourcetype" if in_response => {
                        in_resourcetype = true;
                    }
                    b"collection" if in_resourcetype => {
                        current_is_collection = true;
                    }
                    b"getcontentlength" if in_response => {
                        in_contentlength = true;
                    }
                    _ => {}
                }
            }
            Ok(Event::End(e)) => {
                let name = e.name();
                let local = local_name(name.as_ref());
                match local {
                    b"response" => {
                        if in_response && !current_href.is_empty() {
                            let full_url = resolve_href(base_url, &current_href);
                            entries.push(DavEntry {
                                href: full_url,
                                is_collection: current_is_collection,
                                content_length: current_length,
                            });
                        }
                        in_response = false;
                    }
                    b"href" => in_href = false,
                    b"resourcetype" => in_resourcetype = false,
                    b"getcontentlength" => in_contentlength = false,
                    _ => {}
                }
            }
            Ok(Event::Text(e)) => {
                if in_href {
                    if let Ok(text) = e.decode() {
                        current_href = text.into_owned();
                    }
                } else if in_contentlength && let Ok(text) = e.decode() {
                    current_length = text.trim().parse().ok();
                }
            }
            Ok(Event::Eof) => break,
            Err(e) => {
                tracing::warn!("WebDAV XML parse error: {e}");
                break;
            }
            _ => {}
        }
    }

    Ok(entries)
}

fn local_name(name: &[u8]) -> &[u8] {
    // Strip namespace prefix: "D:href" → "href"
    if let Some(pos) = name.iter().position(|&b| b == b':') {
        &name[pos + 1..]
    } else {
        name
    }
}

fn resolve_href(base_url: &str, href: &str) -> String {
    if href.starts_with("http://") || href.starts_with("https://") {
        return href.to_string();
    }
    // Relative path: combine with base URL's origin
    if let Some(pos) = base_url.find("://")
        && let Some(slash_pos) = base_url[pos + 3..].find('/')
    {
        let origin = &base_url[..pos + 3 + slash_pos];
        return format!("{origin}{href}");
    }
    format!("{base_url}{href}")
}

fn is_self_reference(request_url: &str, href: &str) -> bool {
    let norm_req = request_url.trim_end_matches('/');
    let norm_href = href.trim_end_matches('/');
    norm_req == norm_href
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_simple_multistatus() {
        let xml = r#"<?xml version="1.0" encoding="utf-8"?>
<D:multistatus xmlns:D="DAV:">
  <D:response>
    <D:href>/files/</D:href>
    <D:propstat>
      <D:prop>
        <D:resourcetype><D:collection/></D:resourcetype>
      </D:prop>
      <D:status>HTTP/1.1 200 OK</D:status>
    </D:propstat>
  </D:response>
  <D:response>
    <D:href>/files/doc.pdf</D:href>
    <D:propstat>
      <D:prop>
        <D:resourcetype/>
        <D:getcontentlength>12345</D:getcontentlength>
      </D:prop>
      <D:status>HTTP/1.1 200 OK</D:status>
    </D:propstat>
  </D:response>
  <D:response>
    <D:href>/files/subdir/</D:href>
    <D:propstat>
      <D:prop>
        <D:resourcetype><D:collection/></D:resourcetype>
      </D:prop>
      <D:status>HTTP/1.1 200 OK</D:status>
    </D:propstat>
  </D:response>
</D:multistatus>"#;

        let entries = parse_multistatus(xml, "http://example.com/files/").unwrap();
        assert_eq!(entries.len(), 3);

        // First is the directory itself
        assert!(entries[0].is_collection);
        assert_eq!(entries[0].href, "http://example.com/files/");

        // Second is a file
        assert!(!entries[1].is_collection);
        assert_eq!(entries[1].href, "http://example.com/files/doc.pdf");
        assert_eq!(entries[1].content_length, Some(12345));

        // Third is a subdirectory
        assert!(entries[2].is_collection);
        assert_eq!(entries[2].href, "http://example.com/files/subdir/");
    }

    #[test]
    fn self_reference_detection() {
        assert!(is_self_reference(
            "http://example.com/files/",
            "http://example.com/files/"
        ));
        assert!(is_self_reference(
            "http://example.com/files/",
            "http://example.com/files"
        ));
        assert!(!is_self_reference(
            "http://example.com/files/",
            "http://example.com/files/subdir/"
        ));
    }

    #[test]
    fn resolve_relative_href() {
        assert_eq!(
            resolve_href("http://example.com/dav/", "/dav/file.txt"),
            "http://example.com/dav/file.txt"
        );
        assert_eq!(
            resolve_href("http://example.com/dav/", "http://example.com/dav/file.txt"),
            "http://example.com/dav/file.txt"
        );
    }
}
