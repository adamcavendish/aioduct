use std::path::{Path, PathBuf};

pub fn from_url_and_headers(url: &str, headers: &http::HeaderMap) -> String {
    if let Some(cd) = headers.get("content-disposition") {
        if let Ok(cd_str) = cd.to_str() {
            if let Some(name) = parse_content_disposition(cd_str) {
                return name;
            }
        }
    }

    from_url(url)
}

fn parse_content_disposition(cd: &str) -> Option<String> {
    for part in cd.split(';') {
        let part = part.trim();
        if let Some(rest) = part.strip_prefix("filename*=") {
            if let Some(encoded) = rest
                .strip_prefix("UTF-8''")
                .or_else(|| rest.strip_prefix("utf-8''"))
            {
                if let Ok(decoded) = percent_decode(encoded) {
                    let name = decoded.trim_matches('"');
                    if !name.is_empty() {
                        return Some(name.to_string());
                    }
                }
            }
        } else if let Some(rest) = part.strip_prefix("filename=") {
            let name = rest.trim_matches('"');
            if !name.is_empty() {
                return Some(name.to_string());
            }
        }
    }
    None
}

fn percent_decode(s: &str) -> Result<String, ()> {
    let mut result = Vec::new();
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(byte) = u8::from_str_radix(
                std::str::from_utf8(&bytes[i + 1..i + 3]).map_err(|_| ())?,
                16,
            ) {
                result.push(byte);
                i += 3;
                continue;
            }
        }
        result.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(result).map_err(|_| ())
}

fn from_url(url: &str) -> String {
    let path = url
        .split('?')
        .next()
        .unwrap_or(url)
        .split('#')
        .next()
        .unwrap_or(url);

    let name = path.rsplit('/').next().unwrap_or("download");
    let name = name.trim();

    if name.is_empty() {
        "download".to_string()
    } else {
        name.to_string()
    }
}

pub fn auto_rename(path: &Path) -> PathBuf {
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("download");
    let ext = path.extension().and_then(|s| s.to_str());
    let parent = path.parent().unwrap_or(Path::new("."));

    for i in 1..1000 {
        let name = match ext {
            Some(e) => format!("{stem}.{i}.{e}"),
            None => format!("{stem}.{i}"),
        };
        let candidate = parent.join(&name);
        if !candidate.exists() {
            return candidate;
        }
    }

    path.to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_simple() {
        assert_eq!(from_url("https://example.com/file.iso"), "file.iso");
    }

    #[test]
    fn url_with_query() {
        assert_eq!(
            from_url("https://example.com/file.tar.gz?token=abc"),
            "file.tar.gz"
        );
    }

    #[test]
    fn url_trailing_slash() {
        assert_eq!(from_url("https://example.com/"), "download");
    }

    #[test]
    fn content_disposition_simple() {
        assert_eq!(
            parse_content_disposition("attachment; filename=\"report.pdf\""),
            Some("report.pdf".to_string()),
        );
    }

    #[test]
    fn content_disposition_utf8() {
        assert_eq!(
            parse_content_disposition("attachment; filename*=UTF-8''r%C3%A9sum%C3%A9.pdf"),
            Some("résumé.pdf".to_string()),
        );
    }

    #[test]
    fn percent_decode_basic() {
        assert_eq!(
            percent_decode("hello%20world"),
            Ok("hello world".to_string())
        );
    }
}
