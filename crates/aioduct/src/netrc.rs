use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use http::Uri;
use http::header::{AUTHORIZATION, HeaderValue};

use crate::error::AioductBody;
use crate::middleware::Middleware;

/// A parsed .netrc file mapping machine names to credentials.
#[derive(Debug, Clone)]
pub struct Netrc {
    entries: Arc<HashMap<String, NetrcEntry>>,
    default: Option<Arc<NetrcEntry>>,
}

/// A single machine entry from a .netrc file.
#[derive(Debug, Clone)]
struct NetrcEntry {
    login: String,
    password: String,
}

impl Netrc {
    /// Load the default netrc file (`~/.netrc` or `$NETRC`).
    pub fn load_default() -> Result<Self, io::Error> {
        let path = default_netrc_path()?;
        Self::load(&path)
    }

    /// Load a netrc file from a specific path.
    pub fn load(path: &Path) -> Result<Self, io::Error> {
        let content = std::fs::read_to_string(path)?;
        Ok(Self::parse(&content))
    }

    /// Parse netrc content from a string.
    pub fn parse(content: &str) -> Self {
        let mut entries = HashMap::new();
        let mut default = None;

        let tokens: Vec<&str> = content.split_whitespace().collect();
        let mut i = 0;

        while i < tokens.len() {
            match tokens[i] {
                "machine" => {
                    i += 1;
                    if i >= tokens.len() {
                        break;
                    }
                    let machine = tokens[i].to_string();
                    i += 1;
                    let entry = parse_entry(&tokens, &mut i);
                    if let Some(entry) = entry {
                        entries.insert(machine, entry);
                    }
                }
                "default" => {
                    i += 1;
                    let entry = parse_entry(&tokens, &mut i);
                    if let Some(entry) = entry {
                        default = Some(Arc::new(entry));
                    }
                }
                _ => {
                    i += 1;
                }
            }
        }

        Self {
            entries: Arc::new(entries),
            default,
        }
    }

    fn lookup(&self, host: &str) -> Option<(&str, &str)> {
        if let Some(entry) = self.entries.get(host) {
            return Some((&entry.login, &entry.password));
        }
        if let Some(ref entry) = self.default {
            return Some((&entry.login, &entry.password));
        }
        None
    }
}

fn parse_entry(tokens: &[&str], i: &mut usize) -> Option<NetrcEntry> {
    let mut login = None;
    let mut password = None;

    while *i < tokens.len() {
        match tokens[*i] {
            "login" => {
                *i += 1;
                if *i < tokens.len() {
                    login = Some(tokens[*i].to_string());
                }
                *i += 1;
            }
            "password" | "passwd" => {
                *i += 1;
                if *i < tokens.len() {
                    password = Some(tokens[*i].to_string());
                }
                *i += 1;
            }
            "account" | "macdef" => {
                *i += 1;
                if *i < tokens.len() {
                    *i += 1;
                }
            }
            "machine" | "default" => {
                break;
            }
            _ => {
                *i += 1;
            }
        }
    }

    Some(NetrcEntry {
        login: login.unwrap_or_default(),
        password: password.unwrap_or_default(),
    })
}

fn default_netrc_path() -> Result<PathBuf, io::Error> {
    if let Ok(path) = std::env::var("NETRC") {
        return Ok(PathBuf::from(path));
    }

    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .map_err(|_| io::Error::new(io::ErrorKind::NotFound, "no home directory"))?;

    let path = PathBuf::from(home).join(".netrc");
    if path.exists() {
        return Ok(path);
    }

    let alt_path = PathBuf::from(
        std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .unwrap_or_default(),
    )
    .join("_netrc");
    if alt_path.exists() {
        return Ok(alt_path);
    }

    Ok(path)
}

/// Middleware that automatically applies credentials from a `.netrc` file.
///
/// When a request's host matches a `machine` entry in the netrc file,
/// the corresponding `login` and `password` are applied as HTTP Basic Auth.
#[derive(Debug, Clone)]
pub struct NetrcMiddleware {
    netrc: Netrc,
}

impl NetrcMiddleware {
    /// Create middleware that reads the default `~/.netrc` file.
    pub fn from_default() -> Result<Self, io::Error> {
        Ok(Self {
            netrc: Netrc::load_default()?,
        })
    }

    /// Create middleware from a specific netrc file.
    pub fn from_path(path: &Path) -> Result<Self, io::Error> {
        Ok(Self {
            netrc: Netrc::load(path)?,
        })
    }

    /// Create middleware from a pre-parsed [`Netrc`] instance.
    pub fn new(netrc: Netrc) -> Self {
        Self { netrc }
    }
}

impl Middleware for NetrcMiddleware {
    fn on_request(&self, request: &mut http::Request<AioductBody>, uri: &Uri) {
        if request.headers().contains_key(AUTHORIZATION) {
            return;
        }

        let host = uri.host().unwrap_or("");
        if let Some((login, password)) = self.netrc.lookup(host) {
            let encoded = base64::Engine::encode(
                &base64::engine::general_purpose::STANDARD,
                format!("{login}:{password}"),
            );
            if let Ok(val) = HeaderValue::from_str(&format!("Basic {encoded}")) {
                request.headers_mut().insert(AUTHORIZATION, val);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::BodyExt;

    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn parse_simple_netrc() {
        let netrc = Netrc::parse(
            "machine example.com login user1 password pass1\n\
             machine api.test.io login user2 password pass2\n",
        );
        assert_eq!(netrc.lookup("example.com"), Some(("user1", "pass1")));
        assert_eq!(netrc.lookup("api.test.io"), Some(("user2", "pass2")));
        assert_eq!(netrc.lookup("unknown.com"), None);
    }

    #[test]
    fn parse_with_default() {
        let netrc = Netrc::parse(
            "machine specific.com login alice password secret\n\
             default login anon password anon\n",
        );
        assert_eq!(netrc.lookup("specific.com"), Some(("alice", "secret")));
        assert_eq!(netrc.lookup("anything.else"), Some(("anon", "anon")));
    }

    #[test]
    fn parse_multiline_format() {
        let netrc = Netrc::parse("machine host.example.com\n  login myuser\n  password mypass\n");
        assert_eq!(netrc.lookup("host.example.com"), Some(("myuser", "mypass")));
    }

    #[test]
    fn empty_netrc() {
        let netrc = Netrc::parse("");
        assert_eq!(netrc.lookup("any"), None);
    }

    #[test]
    fn comments_and_extra_whitespace() {
        let netrc = Netrc::parse("  machine   example.com   login   user1   password   pass1  \n");
        assert_eq!(netrc.lookup("example.com"), Some(("user1", "pass1")));
    }

    #[test]
    fn passwd_keyword() {
        let netrc = Netrc::parse("machine example.com login user1 passwd pass1\n");
        assert_eq!(netrc.lookup("example.com"), Some(("user1", "pass1")));
    }

    #[test]
    fn account_and_macdef_skipped() {
        let netrc = Netrc::parse(
            "machine example.com login user1 account acct1 macdef init password pass1\n",
        );
        assert_eq!(netrc.lookup("example.com"), Some(("user1", "pass1")));
    }

    #[test]
    fn missing_login_defaults_to_empty() {
        let netrc = Netrc::parse("machine example.com password pass1\n");
        assert_eq!(netrc.lookup("example.com"), Some(("", "pass1")));
    }

    #[test]
    fn missing_password_defaults_to_empty() {
        let netrc = Netrc::parse("machine example.com login user1\n");
        assert_eq!(netrc.lookup("example.com"), Some(("user1", "")));
    }

    #[test]
    fn multiple_machines_with_default_fallback() {
        let netrc = Netrc::parse(
            "machine a.com login a password pa\n\
             machine b.com login b password pb\n\
             default login d password pd\n",
        );
        assert_eq!(netrc.lookup("a.com"), Some(("a", "pa")));
        assert_eq!(netrc.lookup("b.com"), Some(("b", "pb")));
        assert_eq!(netrc.lookup("c.com"), Some(("d", "pd")));
    }

    #[test]
    fn truncated_machine_at_end() {
        let netrc = Netrc::parse("machine");
        assert_eq!(netrc.lookup("anything"), None);
    }

    #[test]
    fn unknown_tokens_skipped() {
        let netrc =
            Netrc::parse("machine example.com login user1 unknown_key val password pass1\n");
        assert_eq!(netrc.lookup("example.com"), Some(("user1", "pass1")));
    }

    #[test]
    fn netrc_middleware_sets_basic_auth() {
        use http::Uri;
        let netrc = Netrc::parse("machine api.example.com login myuser password mypass\n");
        let mw = NetrcMiddleware::new(netrc);

        let uri: Uri = "http://api.example.com/path".parse().unwrap();
        let body: AioductBody = http_body_util::Empty::new()
            .map_err(|never| match never {})
            .boxed();
        let mut req = http::Request::builder().uri(&uri).body(body).unwrap();
        mw.on_request(&mut req, &uri);

        let auth = req
            .headers()
            .get("authorization")
            .unwrap()
            .to_str()
            .unwrap();
        assert!(auth.starts_with("Basic "));
    }

    #[test]
    fn netrc_middleware_skips_when_auth_present() {
        use http::Uri;
        let netrc = Netrc::parse("machine api.example.com login myuser password mypass\n");
        let mw = NetrcMiddleware::new(netrc);

        let uri: Uri = "http://api.example.com/path".parse().unwrap();
        let body: AioductBody = http_body_util::Empty::new()
            .map_err(|never| match never {})
            .boxed();
        let mut req = http::Request::builder()
            .uri(&uri)
            .header("authorization", "Bearer existing")
            .body(body)
            .unwrap();
        mw.on_request(&mut req, &uri);

        assert_eq!(
            req.headers()
                .get("authorization")
                .unwrap()
                .to_str()
                .unwrap(),
            "Bearer existing"
        );
    }

    #[test]
    fn netrc_middleware_no_match() {
        use http::Uri;
        let netrc = Netrc::parse("machine other.com login user password pass\n");
        let mw = NetrcMiddleware::new(netrc);

        let uri: Uri = "http://api.example.com/path".parse().unwrap();
        let body: AioductBody = http_body_util::Empty::new()
            .map_err(|never| match never {})
            .boxed();
        let mut req = http::Request::builder().uri(&uri).body(body).unwrap();
        mw.on_request(&mut req, &uri);

        assert!(req.headers().get("authorization").is_none());
    }

    #[test]
    fn load_nonexistent_file_errors() {
        let result = Netrc::load(std::path::Path::new("/nonexistent/path/.netrc"));
        assert!(result.is_err());
    }

    #[test]
    fn debug_impl() {
        let netrc = Netrc::parse("machine a.com login u password p\n");
        let dbg = format!("{netrc:?}");
        assert!(dbg.contains("Netrc"));
    }

    #[test]
    fn login_token_at_end() {
        let netrc = Netrc::parse("machine example.com login");
        assert_eq!(netrc.lookup("example.com"), Some(("", "")));
    }

    #[test]
    fn password_token_at_end() {
        let netrc = Netrc::parse("machine example.com password");
        assert_eq!(netrc.lookup("example.com"), Some(("", "")));
    }

    #[test]
    fn account_token_at_end() {
        let netrc = Netrc::parse("machine example.com login u account");
        assert_eq!(netrc.lookup("example.com"), Some(("u", "")));
    }

    #[test]
    fn default_only() {
        let netrc = Netrc::parse("default login anon password anon");
        assert_eq!(netrc.lookup("any.host"), Some(("anon", "anon")));
    }

    #[test]
    fn default_without_login_password() {
        let netrc = Netrc::parse("default");
        assert_eq!(netrc.lookup("any.host"), Some(("", "")));
    }

    #[test]
    fn multiple_defaults_first_wins() {
        let netrc =
            Netrc::parse("default login first password p1\ndefault login second password p2");
        assert_eq!(netrc.lookup("any.host"), Some(("second", "p2")));
    }

    #[test]
    fn middleware_uri_without_host() {
        use http::Uri;
        let netrc = Netrc::parse("default login u password p\n");
        let mw = NetrcMiddleware::new(netrc);

        let uri: Uri = "/relative/path".parse().unwrap();
        let body: AioductBody = http_body_util::Empty::new()
            .map_err(|never| match never {})
            .boxed();
        let mut req = http::Request::builder().uri(&uri).body(body).unwrap();
        mw.on_request(&mut req, &uri);
        assert!(req.headers().contains_key("authorization"));
    }

    #[test]
    fn top_level_unknown_tokens_skipped() {
        let netrc = Netrc::parse("garbage foo machine example.com login u password p\n");
        assert_eq!(netrc.lookup("example.com"), Some(("u", "p")));
    }

    #[test]
    fn load_from_file_succeeds() {
        use std::io::Write;
        let dir = std::env::temp_dir().join("aioduct_netrc_test");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("test_netrc");
        {
            let mut f = std::fs::File::create(&path).unwrap();
            writeln!(f, "machine file.example.com login fuser password fpass").unwrap();
        }
        let netrc = Netrc::load(&path).unwrap();
        assert_eq!(netrc.lookup("file.example.com"), Some(("fuser", "fpass")));
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    fn middleware_from_path() {
        use std::io::Write;
        let dir = std::env::temp_dir().join("aioduct_netrc_mw_test");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("test_netrc_mw");
        {
            let mut f = std::fs::File::create(&path).unwrap();
            writeln!(f, "machine mw.example.com login mwuser password mwpass").unwrap();
        }
        let mw = NetrcMiddleware::from_path(&path).unwrap();
        let uri: http::Uri = "http://mw.example.com/test".parse().unwrap();
        let body: AioductBody = http_body_util::Empty::new()
            .map_err(|never| match never {})
            .boxed();
        let mut req = http::Request::builder().uri(&uri).body(body).unwrap();
        mw.on_request(&mut req, &uri);
        assert!(req.headers().contains_key("authorization"));
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    fn default_netrc_path_via_env() {
        let _lock = ENV_LOCK.lock().unwrap();
        use std::io::Write;
        let dir = std::env::temp_dir().join("aioduct_netrc_env_test");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("custom_netrc");
        {
            let mut f = std::fs::File::create(&path).unwrap();
            writeln!(f, "machine env.example.com login envuser password envpass").unwrap();
        }
        unsafe { std::env::set_var("NETRC", path.to_str().unwrap()) };
        let result = default_netrc_path();
        unsafe { std::env::remove_var("NETRC") };
        assert_eq!(result.unwrap(), path);
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    fn default_netrc_path_falls_back_to_home_dotnetrc() {
        let _lock = ENV_LOCK.lock().unwrap();
        use std::io::Write;
        let dir = std::env::temp_dir().join("aioduct_netrc_home_test");
        let _ = std::fs::create_dir_all(&dir);
        let netrc_path = dir.join(".netrc");
        {
            let mut f = std::fs::File::create(&netrc_path).unwrap();
            writeln!(f, "machine home.example.com login hu password hp").unwrap();
        }
        unsafe { std::env::remove_var("NETRC") };
        let saved_home = std::env::var("HOME").ok();
        unsafe { std::env::set_var("HOME", dir.to_str().unwrap()) };
        let result = default_netrc_path();
        if let Some(h) = saved_home {
            unsafe { std::env::set_var("HOME", h) };
        } else {
            unsafe { std::env::remove_var("HOME") };
        }
        assert_eq!(result.unwrap(), netrc_path);
        let _ = std::fs::remove_file(&netrc_path);
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    fn default_netrc_path_falls_back_to_underscore_netrc() {
        let _lock = ENV_LOCK.lock().unwrap();
        use std::io::Write;
        let dir = std::env::temp_dir().join("aioduct_netrc_alt_test");
        let _ = std::fs::create_dir_all(&dir);
        let alt_path = dir.join("_netrc");
        {
            let mut f = std::fs::File::create(&alt_path).unwrap();
            writeln!(f, "machine alt.example.com login au password ap").unwrap();
        }
        let dotnetrc = dir.join(".netrc");
        let _ = std::fs::remove_file(&dotnetrc);

        unsafe { std::env::remove_var("NETRC") };
        let saved_home = std::env::var("HOME").ok();
        unsafe { std::env::set_var("HOME", dir.to_str().unwrap()) };
        let result = default_netrc_path();
        if let Some(h) = saved_home {
            unsafe { std::env::set_var("HOME", h) };
        } else {
            unsafe { std::env::remove_var("HOME") };
        }
        assert_eq!(result.unwrap(), alt_path);
        let _ = std::fs::remove_file(&alt_path);
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    fn default_netrc_path_returns_dotnetrc_when_neither_exists() {
        let _lock = ENV_LOCK.lock().unwrap();
        let dir = std::env::temp_dir().join("aioduct_netrc_neither_test");
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::remove_file(dir.join(".netrc"));
        let _ = std::fs::remove_file(dir.join("_netrc"));

        unsafe { std::env::remove_var("NETRC") };
        let saved_home = std::env::var("HOME").ok();
        unsafe { std::env::set_var("HOME", dir.to_str().unwrap()) };
        let result = default_netrc_path();
        if let Some(h) = saved_home {
            unsafe { std::env::set_var("HOME", h) };
        } else {
            unsafe { std::env::remove_var("HOME") };
        }
        assert_eq!(result.unwrap(), dir.join(".netrc"));
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    fn load_default_via_env() {
        let _lock = ENV_LOCK.lock().unwrap();
        use std::io::Write;
        let dir = std::env::temp_dir().join("aioduct_netrc_load_default_test");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("load_default_netrc");
        {
            let mut f = std::fs::File::create(&path).unwrap();
            writeln!(f, "machine ld.example.com login ldu password ldp").unwrap();
        }
        unsafe { std::env::set_var("NETRC", path.to_str().unwrap()) };
        let netrc = Netrc::load_default().unwrap();
        unsafe { std::env::remove_var("NETRC") };
        assert_eq!(netrc.lookup("ld.example.com"), Some(("ldu", "ldp")));
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    fn middleware_from_default_via_env() {
        let _lock = ENV_LOCK.lock().unwrap();
        use std::io::Write;
        let dir = std::env::temp_dir().join("aioduct_netrc_mw_default_test");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("mw_default_netrc");
        {
            let mut f = std::fs::File::create(&path).unwrap();
            writeln!(f, "machine mwd.example.com login mwdu password mwdp").unwrap();
        }
        unsafe { std::env::set_var("NETRC", path.to_str().unwrap()) };
        let mw = NetrcMiddleware::from_default().unwrap();
        unsafe { std::env::remove_var("NETRC") };

        let uri: http::Uri = "http://mwd.example.com/test".parse().unwrap();
        let body: AioductBody = http_body_util::Empty::new()
            .map_err(|never| match never {})
            .boxed();
        let mut req = http::Request::builder().uri(&uri).body(body).unwrap();
        mw.on_request(&mut req, &uri);
        assert!(req.headers().contains_key("authorization"));

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&dir);
    }
}
