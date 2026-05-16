use std::collections::HashMap;
use std::sync::Arc;

use http::header::{HeaderValue, WWW_AUTHENTICATE};
use http::{HeaderMap, Method, StatusCode, Uri};

#[derive(Clone)]
pub(crate) struct DigestAuth {
    username: String,
    password: String,
    nonce_counts: Arc<std::sync::Mutex<HashMap<String, u32>>>,
}

impl DigestAuth {
    pub(crate) fn new(username: String, password: String) -> Self {
        Self {
            username,
            password,
            nonce_counts: Arc::new(std::sync::Mutex::new(HashMap::new())),
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

        let nc = {
            let Ok(mut counts) = self.nonce_counts.lock() else {
                return None;
            };
            if counts.len() > 64 {
                counts.clear();
            }
            let entry = counts.entry(nonce.to_string()).or_insert(0);
            *entry += 1;
            *entry
        };
        let nc_str = format!("{nc:08x}");
        let cnonce = format!("{:016x}", rand_u64());

        let hash_fn: fn(&str) -> String = match algorithm {
            "SHA-256" | "SHA-256-sess" => sha256_hex,
            _ => md5_hex,
        };

        let mut ha1 = hash_fn(&format!("{}:{}:{}", self.username, realm, self.password));
        if algorithm.ends_with("-sess") {
            ha1 = hash_fn(&format!("{ha1}:{nonce}:{cnonce}"));
        }

        let ha2 = hash_fn(&format!("{}:{}", method.as_str(), path));

        let response = if qop.is_some_and(|q| has_qop_auth(q)) {
            hash_fn(&format!("{ha1}:{nonce}:{nc_str}:{cnonce}:auth:{ha2}"))
        } else {
            hash_fn(&format!("{ha1}:{nonce}:{ha2}"))
        };

        let mut value = format!(
            "Digest username=\"{}\", realm=\"{}\", nonce=\"{}\", uri=\"{}\", response=\"{}\"",
            self.username, realm, nonce, path, response
        );

        if qop.is_some_and(|q| has_qop_auth(q)) {
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
        let _ = write!(hex, "{byte:02x}");
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

fn sha256_hex(input: &str) -> String {
    use std::fmt::Write;

    let digest = sha256_compute(input.as_bytes());
    let mut hex = String::with_capacity(64);
    for byte in &digest {
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

fn sha256_compute(data: &[u8]) -> [u8; 32] {
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];

    let bit_len = (data.len() as u64) * 8;
    let mut padded = data.to_vec();
    padded.push(0x80);
    while padded.len() % 64 != 56 {
        padded.push(0);
    }
    padded.extend_from_slice(&bit_len.to_be_bytes());

    for chunk in padded.chunks_exact(64) {
        let mut w = [0u32; 64];
        for (i, word) in chunk.chunks_exact(4).enumerate() {
            w[i] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        sha256_round(&mut h, &w);
    }

    let mut result = [0u8; 32];
    for (i, &word) in h.iter().enumerate() {
        result[i * 4..i * 4 + 4].copy_from_slice(&word.to_be_bytes());
    }
    result
}

fn sha256_round(state: &mut [u32; 8], w: &[u32; 64]) {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];

    let (mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh) = (
        state[0], state[1], state[2], state[3], state[4], state[5], state[6], state[7],
    );

    for i in 0..64 {
        let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
        let ch = (e & f) ^ ((!e) & g);
        let temp1 = hh
            .wrapping_add(s1)
            .wrapping_add(ch)
            .wrapping_add(K[i])
            .wrapping_add(w[i]);
        let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
        let maj = (a & b) ^ (a & c) ^ (b & c);
        let temp2 = s0.wrapping_add(maj);

        hh = g;
        g = f;
        f = e;
        e = d.wrapping_add(temp1);
        d = c;
        c = b;
        b = a;
        a = temp1.wrapping_add(temp2);
    }

    state[0] = state[0].wrapping_add(a);
    state[1] = state[1].wrapping_add(b);
    state[2] = state[2].wrapping_add(c);
    state[3] = state[3].wrapping_add(d);
    state[4] = state[4].wrapping_add(e);
    state[5] = state[5].wrapping_add(f);
    state[6] = state[6].wrapping_add(g);
    state[7] = state[7].wrapping_add(hh);
}

fn has_qop_auth(qop: &str) -> bool {
    qop.split(',').map(str::trim).any(|t| t == "auth")
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
    fn sess_algorithm_uses_double_hash() {
        let auth = DigestAuth::new("user".into(), "pass".into());
        let uri: Uri = "http://example.com/path".parse().unwrap();
        let mut headers = HeaderMap::new();
        headers.insert(
            WWW_AUTHENTICATE,
            HeaderValue::from_static(
                r#"Digest realm="test", nonce="abc", qop="auth", algorithm=SHA-256-sess"#,
            ),
        );
        let value = auth.authorize(&Method::GET, &uri, &headers).unwrap();
        let v = value.to_str().unwrap().to_string();
        assert!(v.contains("algorithm=SHA-256-sess"));

        // Parse cnonce and nc from the output
        let cnonce = v
            .split("cnonce=\"")
            .nth(1)
            .unwrap()
            .split('"')
            .next()
            .unwrap();
        let nc = v
            .split("nc=")
            .nth(1)
            .unwrap()
            .split(',')
            .next()
            .unwrap()
            .trim();
        let response = v
            .split("response=\"")
            .nth(1)
            .unwrap()
            .split('"')
            .next()
            .unwrap();

        // Independently compute the expected response using -sess double hash
        let ha1_inner = sha256_hex("user:test:pass");
        let ha1 = sha256_hex(&format!("{ha1_inner}:abc:{cnonce}"));
        let ha2 = sha256_hex("GET:/path");
        let expected = sha256_hex(&format!("{ha1}:abc:{nc}:{cnonce}:auth:{ha2}"));
        assert_eq!(response, expected);
    }

    #[test]
    fn nonce_count_is_per_nonce() {
        let auth = DigestAuth::new("user".into(), "pass".into());
        let uri: Uri = "http://example.com/path".parse().unwrap();
        let mut h1 = HeaderMap::new();
        h1.insert(
            WWW_AUTHENTICATE,
            HeaderValue::from_static(r#"Digest realm="test", nonce="aaa", qop="auth""#),
        );
        let mut h2 = HeaderMap::new();
        h2.insert(
            WWW_AUTHENTICATE,
            HeaderValue::from_static(r#"Digest realm="test", nonce="bbb", qop="auth""#),
        );
        let v1 = auth.authorize(&Method::GET, &uri, &h1).unwrap();
        assert!(v1.to_str().unwrap().contains("nc=00000001"));
        let v2 = auth.authorize(&Method::GET, &uri, &h2).unwrap();
        assert!(v2.to_str().unwrap().contains("nc=00000001"));
        let v3 = auth.authorize(&Method::GET, &uri, &h1).unwrap();
        assert!(v3.to_str().unwrap().contains("nc=00000002"));
    }

    #[test]
    fn md5_long_input() {
        let input = "a".repeat(100);
        let hash = md5_hex(&input);
        assert_eq!(hash.len(), 32);
        assert!(hash.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
