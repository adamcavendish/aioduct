use std::path::{Path, PathBuf};

pub fn from_url_and_headers(url: &str, headers: &http::HeaderMap) -> String {
    if let Some(cd) = headers.get("content-disposition")
        && let Ok(cd_str) = cd.to_str()
        && let Some(name) = parse_content_disposition(cd_str)
    {
        return sanitize_file_name(&name);
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
                && let Ok(decoded) = percent_decode(encoded)
            {
                let name = decoded.trim_matches('"');
                if !name.is_empty() {
                    return Some(name.to_string());
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
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let Ok(byte) = u8::from_str_radix(
                std::str::from_utf8(&bytes[i + 1..i + 3]).map_err(|_| ())?,
                16,
            )
        {
            result.push(byte);
            i += 3;
            continue;
        }
        result.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(result).map_err(|_| ())
}

pub fn from_url(url: &str) -> String {
    let path = url
        .split('?')
        .next()
        .unwrap_or(url)
        .split('#')
        .next()
        .unwrap_or(url);

    let name = path.rsplit('/').next().unwrap_or("download");
    sanitize_file_name(name)
}

pub fn sanitize_file_name(name: &str) -> String {
    let normalized = name.replace('\\', "/");
    let name = normalized.rsplit('/').next().unwrap_or("download").trim();
    let name = name.trim_matches(char::from(0));

    if name.is_empty() || name == "." || name == ".." {
        return "download".to_string();
    }

    let sanitized: String = name
        .chars()
        .map(|c| if is_windows_invalid_char(c) { '_' } else { c })
        .collect();
    let sanitized = sanitized.trim_end_matches(['.', ' ']).to_string();

    if sanitized.is_empty() || sanitized == "." || sanitized == ".." {
        "download".to_string()
    } else if is_windows_reserved_name(&sanitized) {
        format!("_{sanitized}")
    } else {
        sanitized
    }
}

fn is_windows_invalid_char(c: char) -> bool {
    matches!(c, '<' | '>' | ':' | '"' | '|' | '?' | '*' | '/' | '\\') || c.is_control()
}

fn is_windows_reserved_name(name: &str) -> bool {
    let stem = name
        .split('.')
        .next()
        .unwrap_or(name)
        .trim_end_matches(['.', ' ']);
    matches!(
        stem.to_ascii_uppercase().as_str(),
        "CON"
            | "PRN"
            | "AUX"
            | "NUL"
            | "COM1"
            | "COM2"
            | "COM3"
            | "COM4"
            | "COM5"
            | "COM6"
            | "COM7"
            | "COM8"
            | "COM9"
            | "LPT1"
            | "LPT2"
            | "LPT3"
            | "LPT4"
            | "LPT5"
            | "LPT6"
            | "LPT7"
            | "LPT8"
            | "LPT9"
    )
}

pub fn sanitize_relative_path(path: &str) -> PathBuf {
    let normalized = path.replace('\\', "/");
    let mut out = PathBuf::new();

    for raw in normalized.split('/') {
        let segment = raw.trim();
        if segment.is_empty() || segment == "." || segment == ".." {
            continue;
        }
        out.push(sanitize_file_name(segment));
    }

    if out.as_os_str().is_empty() {
        PathBuf::from("download")
    } else {
        out
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
    fn content_disposition_sanitized_to_basename() {
        let mut headers = http::HeaderMap::new();
        headers.insert(
            "content-disposition",
            http::HeaderValue::from_static("attachment; filename=\"../../secret.txt\""),
        );

        assert_eq!(
            from_url_and_headers("https://example.com/fallback", &headers),
            "secret.txt"
        );
    }

    #[test]
    fn relative_path_drops_traversal_segments() {
        assert_eq!(
            sanitize_relative_path("../safe/../../file.bin"),
            PathBuf::from("safe/file.bin")
        );
        assert_eq!(
            sanitize_relative_path("/absolute/path/file.bin"),
            PathBuf::from("absolute/path/file.bin")
        );
    }

    #[test]
    fn windows_special_names_are_sanitized() {
        assert_eq!(sanitize_file_name("C:"), "C_");
        assert_eq!(sanitize_file_name("CON.txt"), "_CON.txt");
        assert_eq!(sanitize_file_name("nul"), "_nul");
        assert_eq!(sanitize_file_name("report. "), "report");
        assert_eq!(sanitize_file_name("bad<name>|?.txt"), "bad_name___.txt");
        assert_eq!(
            sanitize_relative_path("dir/COM1/file:name?.txt"),
            PathBuf::from("dir/_COM1/file_name_.txt")
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
