use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use http::header::{HeaderValue, WWW_AUTHENTICATE};
use http::{HeaderMap, Method, StatusCode, Uri};

#[derive(Clone)]
pub(crate) struct DigestAuth {
    username: String,
    password: String,
    nonce_count: Arc<AtomicU32>,
}

impl DigestAuth {
    pub(crate) fn new(username: String, password: String) -> Self {
        Self {
            username,
            password,
            nonce_count: Arc::new(AtomicU32::new(1)),
        }
    }

    pub(crate) fn needs_retry(&self, status: StatusCode, headers: &HeaderMap) -> bool {
        status == StatusCode::UNAUTHORIZED && headers.contains_key(WWW_AUTHENTICATE)
    }

    pub(crate) fn authorize(
        &self,
        method: &Method,
        uri: &Uri,
        headers: &HeaderMap,
    ) -> Option<HeaderValue> {
        let challenge = headers.get(WWW_AUTHENTICATE)?.to_str().ok()?;
        if !challenge.to_ascii_lowercase().starts_with("digest ") {
            return None;
        }

        let params = parse_challenge(&challenge[7..]);

        let realm = params.get("realm")?;
        let nonce = params.get("nonce")?;
        let qop = params.get("qop");
        let opaque = params.get("opaque");
        let algorithm = params.get("algorithm").map(|s| s.as_str()).unwrap_or("MD5");

        let path = uri.path_and_query().map(|pq| pq.as_str()).unwrap_or("/");

        let nc = self.nonce_count.fetch_add(1, Ordering::Relaxed);
        let nc_str = format!("{nc:08x}");
        let cnonce = format!("{:016x}", rand_u64());

        let ha1 = md5_hex(&format!("{}:{}:{}", self.username, realm, self.password));

        let ha2 = md5_hex(&format!("{}:{}", method.as_str(), path));

        let response = if qop.is_some_and(|q| q.contains("auth")) {
            md5_hex(&format!("{ha1}:{nonce}:{nc_str}:{cnonce}:auth:{ha2}"))
        } else {
            md5_hex(&format!("{ha1}:{nonce}:{ha2}"))
        };

        let mut value = format!(
            "Digest username=\"{}\", realm=\"{}\", nonce=\"{}\", uri=\"{}\", response=\"{}\"",
            self.username, realm, nonce, path, response
        );

        if qop.is_some_and(|q| q.contains("auth")) {
            value.push_str(&format!(", qop=auth, nc={nc_str}, cnonce=\"{cnonce}\""));
        }

        if let Some(opaque) = opaque {
            value.push_str(&format!(", opaque=\"{opaque}\""));
        }

        if algorithm != "MD5" {
            value.push_str(&format!(", algorithm={algorithm}"));
        }

        HeaderValue::from_str(&value).ok()
    }
}

fn parse_challenge(s: &str) -> HashMap<String, String> {
    let mut params = HashMap::new();
    let mut remaining = s.trim();

    while !remaining.is_empty() {
        remaining = remaining.trim_start_matches([',', ' ']);
        if remaining.is_empty() {
            break;
        }

        let eq_pos = match remaining.find('=') {
            Some(p) => p,
            None => break,
        };

        let key = remaining[..eq_pos].trim().to_ascii_lowercase();
        remaining = &remaining[eq_pos + 1..];

        let value = if remaining.starts_with('"') {
            remaining = &remaining[1..];
            match remaining.find('"') {
                Some(end) => {
                    let val = &remaining[..end];
                    remaining = &remaining[end + 1..];
                    val.to_string()
                }
                None => {
                    let val = remaining.to_string();
                    remaining = "";
                    val
                }
            }
        } else {
            let end = remaining.find(',').unwrap_or(remaining.len());
            let val = remaining[..end].trim().to_string();
            remaining = &remaining[end..];
            val
        };

        params.insert(key, value);
    }

    params
}

fn md5_hex(input: &str) -> String {
    use std::fmt::Write;

    let digest = md5_compute(input.as_bytes());
    let mut hex = String::with_capacity(32);
    for byte in &digest {
        write!(hex, "{byte:02x}").unwrap();
    }
    hex
}

fn md5_compute(data: &[u8]) -> [u8; 16] {
    let mut state: [u32; 4] = [0x67452301, 0xefcdab89, 0x98badcfe, 0x10325476];

    let bit_len = (data.len() as u64) * 8;
    let mut padded = data.to_vec();
    padded.push(0x80);
    while padded.len() % 64 != 56 {
        padded.push(0);
    }
    padded.extend_from_slice(&bit_len.to_le_bytes());

    for chunk in padded.chunks_exact(64) {
        let mut m = [0u32; 16];
        for (i, word) in chunk.chunks_exact(4).enumerate() {
            m[i] = u32::from_le_bytes([word[0], word[1], word[2], word[3]]);
        }
        md5_round(&mut state, &m);
    }

    let mut result = [0u8; 16];
    for (i, &word) in state.iter().enumerate() {
        result[i * 4..i * 4 + 4].copy_from_slice(&word.to_le_bytes());
    }
    result
}

fn md5_round(state: &mut [u32; 4], m: &[u32; 16]) {
    let (mut a, mut b, mut c, mut d) = (state[0], state[1], state[2], state[3]);

    const S: [[u32; 4]; 4] = [
        [7, 12, 17, 22],
        [5, 9, 14, 20],
        [4, 11, 16, 23],
        [6, 10, 15, 21],
    ];

    const K: [u32; 64] = [
        0xd76aa478, 0xe8c7b756, 0x242070db, 0xc1bdceee, 0xf57c0faf, 0x4787c62a, 0xa8304613,
        0xfd469501, 0x698098d8, 0x8b44f7af, 0xffff5bb1, 0x895cd7be, 0x6b901122, 0xfd987193,
        0xa679438e, 0x49b40821, 0xf61e2562, 0xc040b340, 0x265e5a51, 0xe9b6c7aa, 0xd62f105d,
        0x02441453, 0xd8a1e681, 0xe7d3fbc8, 0x21e1cde6, 0xc33707d6, 0xf4d50d87, 0x455a14ed,
        0xa9e3e905, 0xfcefa3f8, 0x676f02d9, 0x8d2a4c8a, 0xfffa3942, 0x8771f681, 0x6d9d6122,
        0xfde5380c, 0xa4beea44, 0x4bdecfa9, 0xf6bb4b60, 0xbebfbc70, 0x289b7ec6, 0xeaa127fa,
        0xd4ef3085, 0x04881d05, 0xd9d4d039, 0xe6db99e5, 0x1fa27cf8, 0xc4ac5665, 0xf4292244,
        0x432aff97, 0xab9423a7, 0xfc93a039, 0x655b59c3, 0x8f0ccc92, 0xffeff47d, 0x85845dd1,
        0x6fa87e4f, 0xfe2ce6e0, 0xa3014314, 0x4e0811a1, 0xf7537e82, 0xbd3af235, 0x2ad7d2bb,
        0xeb86d391,
    ];

    for i in 0..64 {
        let (f, g) = match i {
            0..16 => ((b & c) | ((!b) & d), i),
            16..32 => ((d & b) | ((!d) & c), (5 * i + 1) % 16),
            32..48 => (b ^ c ^ d, (3 * i + 5) % 16),
            _ => (c ^ (b | (!d)), (7 * i) % 16),
        };
        let temp = d;
        d = c;
        c = b;
        let round = i / 16;
        let shift = S[round][i % 4];
        b = b.wrapping_add(
            (a.wrapping_add(f).wrapping_add(K[i]).wrapping_add(m[g])).rotate_left(shift),
        );
        a = temp;
    }

    state[0] = state[0].wrapping_add(a);
    state[1] = state[1].wrapping_add(b);
    state[2] = state[2].wrapping_add(c);
    state[3] = state[3].wrapping_add(d);
}

fn rand_u64() -> u64 {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};
    RandomState::new().build_hasher().finish()
}

impl std::fmt::Debug for DigestAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DigestAuth")
            .field("username", &self.username)
            .field("password", &"[redacted]")
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn md5_rfc1321_test_vectors() {
        assert_eq!(md5_hex(""), "d41d8cd98f00b204e9800998ecf8427e");
        assert_eq!(md5_hex("a"), "0cc175b9c0f1b6a831c399e269772661");
        assert_eq!(md5_hex("abc"), "900150983cd24fb0d6963f7d28e17f72");
        assert_eq!(
            md5_hex("message digest"),
            "f96b697d7cb7938d525a2f31aaf161d0"
        );
    }

    #[test]
    fn parse_challenge_basic() {
        let params = parse_challenge(r#"realm="test", nonce="abc123", qop="auth", opaque="xyz""#);
        assert_eq!(params.get("realm").unwrap(), "test");
        assert_eq!(params.get("nonce").unwrap(), "abc123");
        assert_eq!(params.get("qop").unwrap(), "auth");
        assert_eq!(params.get("opaque").unwrap(), "xyz");
    }

    #[test]
    fn digest_response_generation() {
        let auth = DigestAuth::new("user".into(), "pass".into());
        let uri: Uri = "http://example.com/dir/index.html".parse().unwrap();
        let mut headers = HeaderMap::new();
        headers.insert(
            WWW_AUTHENTICATE,
            HeaderValue::from_static(
                r#"Digest realm="testrealm@host.com", nonce="dcd98b", qop="auth""#,
            ),
        );

        let value = auth.authorize(&Method::GET, &uri, &headers);
        assert!(value.is_some());
        let v = value.unwrap().to_str().unwrap().to_string();
        assert!(v.starts_with("Digest "));
        assert!(v.contains("username=\"user\""));
        assert!(v.contains("realm=\"testrealm@host.com\""));
        assert!(v.contains("qop=auth"));
    }

    #[test]
    fn needs_retry_401_with_header() {
        let auth = DigestAuth::new("u".into(), "p".into());
        let mut headers = HeaderMap::new();
        headers.insert(
            WWW_AUTHENTICATE,
            HeaderValue::from_static("Digest realm=\"r\""),
        );
        assert!(auth.needs_retry(StatusCode::UNAUTHORIZED, &headers));
    }

    #[test]
    fn needs_retry_401_without_header() {
        let auth = DigestAuth::new("u".into(), "p".into());
        assert!(!auth.needs_retry(StatusCode::UNAUTHORIZED, &HeaderMap::new()));
    }

    #[test]
    fn needs_retry_200_with_header() {
        let auth = DigestAuth::new("u".into(), "p".into());
        let mut headers = HeaderMap::new();
        headers.insert(
            WWW_AUTHENTICATE,
            HeaderValue::from_static("Digest realm=\"r\""),
        );
        assert!(!auth.needs_retry(StatusCode::OK, &headers));
    }

    #[test]
    fn needs_retry_200_without_header() {
        let auth = DigestAuth::new("u".into(), "p".into());
        assert!(!auth.needs_retry(StatusCode::OK, &HeaderMap::new()));
    }

    #[test]
    fn authorize_no_www_authenticate() {
        let auth = DigestAuth::new("u".into(), "p".into());
        let uri: Uri = "http://example.com/".parse().unwrap();
        assert!(
            auth.authorize(&Method::GET, &uri, &HeaderMap::new())
                .is_none()
        );
    }

    #[test]
    fn authorize_non_digest_challenge() {
        let auth = DigestAuth::new("u".into(), "p".into());
        let uri: Uri = "http://example.com/".parse().unwrap();
        let mut headers = HeaderMap::new();
        headers.insert(
            WWW_AUTHENTICATE,
            HeaderValue::from_static("Basic realm=\"test\""),
        );
        assert!(auth.authorize(&Method::GET, &uri, &headers).is_none());
    }

    #[test]
    fn authorize_missing_realm() {
        let auth = DigestAuth::new("u".into(), "p".into());
        let uri: Uri = "http://example.com/".parse().unwrap();
        let mut headers = HeaderMap::new();
        headers.insert(
            WWW_AUTHENTICATE,
            HeaderValue::from_static("Digest nonce=\"abc\""),
        );
        assert!(auth.authorize(&Method::GET, &uri, &headers).is_none());
    }

    #[test]
    fn authorize_missing_nonce() {
        let auth = DigestAuth::new("u".into(), "p".into());
        let uri: Uri = "http://example.com/".parse().unwrap();
        let mut headers = HeaderMap::new();
        headers.insert(
            WWW_AUTHENTICATE,
            HeaderValue::from_static("Digest realm=\"test\""),
        );
        assert!(auth.authorize(&Method::GET, &uri, &headers).is_none());
    }

    #[test]
    fn authorize_without_qop() {
        let auth = DigestAuth::new("user".into(), "pass".into());
        let uri: Uri = "http://example.com/path".parse().unwrap();
        let mut headers = HeaderMap::new();
        headers.insert(
            WWW_AUTHENTICATE,
            HeaderValue::from_static(r#"Digest realm="test", nonce="abc""#),
        );
        let value = auth.authorize(&Method::GET, &uri, &headers).unwrap();
        let v = value.to_str().unwrap().to_string();
        assert!(v.starts_with("Digest "));
        assert!(!v.contains("qop="));
        assert!(!v.contains("cnonce="));
    }

    #[test]
    fn authorize_without_opaque() {
        let auth = DigestAuth::new("user".into(), "pass".into());
        let uri: Uri = "http://example.com/path".parse().unwrap();
        let mut headers = HeaderMap::new();
        headers.insert(
            WWW_AUTHENTICATE,
            HeaderValue::from_static(r#"Digest realm="test", nonce="abc", qop="auth""#),
        );
        let value = auth.authorize(&Method::GET, &uri, &headers).unwrap();
        let v = value.to_str().unwrap().to_string();
        assert!(!v.contains("opaque="));
    }

    #[test]
    fn authorize_with_non_md5_algorithm() {
        let auth = DigestAuth::new("user".into(), "pass".into());
        let uri: Uri = "http://example.com/path".parse().unwrap();
        let mut headers = HeaderMap::new();
        headers.insert(
            WWW_AUTHENTICATE,
            HeaderValue::from_static(r#"Digest realm="test", nonce="abc", algorithm=SHA-256"#),
        );
        let value = auth.authorize(&Method::GET, &uri, &headers).unwrap();
        let v = value.to_str().unwrap().to_string();
        assert!(v.contains("algorithm=SHA-256"));
    }

    #[test]
    fn authorize_uri_without_path() {
        let auth = DigestAuth::new("user".into(), "pass".into());
        let uri: Uri = "http://example.com".parse().unwrap();
        let mut headers = HeaderMap::new();
        headers.insert(
            WWW_AUTHENTICATE,
            HeaderValue::from_static(r#"Digest realm="test", nonce="abc""#),
        );
        let value = auth.authorize(&Method::GET, &uri, &headers);
        assert!(value.is_some());
    }

    #[test]
    fn authorize_nonce_count_increments() {
        let auth = DigestAuth::new("user".into(), "pass".into());
        let uri: Uri = "http://example.com/path".parse().unwrap();
        let mut headers = HeaderMap::new();
        headers.insert(
            WWW_AUTHENTICATE,
            HeaderValue::from_static(r#"Digest realm="test", nonce="abc", qop="auth""#),
        );
        let v1 = auth.authorize(&Method::GET, &uri, &headers).unwrap();
        let v2 = auth.authorize(&Method::GET, &uri, &headers).unwrap();
        let s1 = v1.to_str().unwrap().to_string();
        let s2 = v2.to_str().unwrap().to_string();
        assert!(s1.contains("nc=00000001"));
        assert!(s2.contains("nc=00000002"));
    }

    #[test]
    fn parse_challenge_empty() {
        let params = parse_challenge("");
        assert!(params.is_empty());
    }

    #[test]
    fn parse_challenge_no_equals() {
        let params = parse_challenge("just-a-key");
        assert!(params.is_empty());
    }

    #[test]
    fn parse_challenge_unterminated_quote() {
        let params = parse_challenge(r#"realm="unterminated"#);
        assert_eq!(params.get("realm").unwrap(), "unterminated");
    }

    #[test]
    fn parse_challenge_unquoted_values() {
        let params = parse_challenge("realm=test, nonce=abc123");
        assert_eq!(params.get("realm").unwrap(), "test");
        assert_eq!(params.get("nonce").unwrap(), "abc123");
    }

    #[test]
    fn debug_redacts_password() {
        let auth = DigestAuth::new("myuser".into(), "secret".into());
        let dbg = format!("{auth:?}");
        assert!(dbg.contains("myuser"));
        assert!(dbg.contains("[redacted]"));
        assert!(!dbg.contains("secret"));
    }

    #[test]
    fn md5_long_input() {
        let input = "a".repeat(100);
        let hash = md5_hex(&input);
        assert_eq!(hash.len(), 32);
        assert!(hash.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
