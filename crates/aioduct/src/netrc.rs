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
}
