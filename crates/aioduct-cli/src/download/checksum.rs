use std::fmt;
use std::path::Path;
use std::sync::{Arc, Mutex};

use sha2::{Digest, Sha256};
use tokio::io::AsyncReadExt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChecksumAlgorithm {
    Sha256,
}

impl ChecksumAlgorithm {
    pub fn label(self) -> &'static str {
        match self {
            Self::Sha256 => "sha-256",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChecksumSpec {
    algorithm: ChecksumAlgorithm,
    expected_hex: String,
}

impl ChecksumSpec {
    pub fn algorithm_label(&self) -> &'static str {
        self.algorithm.label()
    }

    pub fn pending_label(&self) -> String {
        format!("{} pending", self.algorithm_label())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChecksumReport {
    pub algorithm: ChecksumAlgorithm,
    pub expected_hex: String,
    pub actual_hex: String,
    pub verified: bool,
}

impl ChecksumReport {
    pub fn status_label(&self) -> String {
        if self.verified {
            format!("{} verified", self.algorithm.label())
        } else {
            format!("{} mismatch", self.algorithm.label())
        }
    }

    pub fn summary(&self) -> String {
        if self.verified {
            format!("checksum verified ({})", self.algorithm.label())
        } else {
            format!(
                "checksum mismatch ({}): expected {}, got {}",
                self.algorithm.label(),
                short_digest(&self.expected_hex),
                short_digest(&self.actual_hex)
            )
        }
    }
}

#[derive(Debug)]
pub enum ChecksumError {
    Io(std::io::Error),
    Mismatch(ChecksumReport),
}

impl fmt::Display for ChecksumError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "checksum read error: {e}"),
            Self::Mismatch(report) => write!(f, "{}", report.summary()),
        }
    }
}

impl std::error::Error for ChecksumError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            Self::Mismatch(_) => None,
        }
    }
}

impl From<std::io::Error> for ChecksumError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

pub type SharedChecksumStatus = Arc<Mutex<String>>;

pub fn shared_status(spec: Option<&ChecksumSpec>) -> SharedChecksumStatus {
    Arc::new(Mutex::new(
        spec.map(ChecksumSpec::pending_label)
            .unwrap_or_else(|| "not configured".to_string()),
    ))
}

pub fn read_status(status: &SharedChecksumStatus) -> String {
    status.lock().unwrap().clone()
}

pub fn set_status(status: &SharedChecksumStatus, value: impl Into<String>) {
    *status.lock().unwrap() = value.into();
}

pub fn parse(value: &str) -> Result<ChecksumSpec, String> {
    let (algorithm, digest) = value
        .split_once('=')
        .ok_or_else(|| "checksum must be TYPE=DIGEST, for example sha-256=abcdef...".to_string())?;
    let algorithm = match algorithm.trim().to_ascii_lowercase().as_str() {
        "sha-256" | "sha256" => ChecksumAlgorithm::Sha256,
        other => {
            return Err(format!(
                "unsupported checksum type '{other}' (supported: sha-256)"
            ));
        }
    };

    let expected_hex = digest.trim().to_ascii_lowercase();
    let expected_len = match algorithm {
        ChecksumAlgorithm::Sha256 => 64,
    };
    if expected_hex.len() != expected_len {
        return Err(format!(
            "{} checksum must be {expected_len} hex characters",
            algorithm.label()
        ));
    }
    hex::decode(&expected_hex)
        .map_err(|e| format!("{} checksum is not valid hex: {e}", algorithm.label()))?;

    Ok(ChecksumSpec {
        algorithm,
        expected_hex,
    })
}

pub async fn verify_file(
    path: &Path,
    spec: &ChecksumSpec,
) -> Result<ChecksumReport, ChecksumError> {
    let actual_hex = match spec.algorithm {
        ChecksumAlgorithm::Sha256 => sha256_file(path).await?,
    };
    let verified = actual_hex.eq_ignore_ascii_case(&spec.expected_hex);
    let report = ChecksumReport {
        algorithm: spec.algorithm,
        expected_hex: spec.expected_hex.clone(),
        actual_hex,
        verified,
    };

    if report.verified {
        Ok(report)
    } else {
        Err(ChecksumError::Mismatch(report))
    }
}

async fn sha256_file(path: &Path) -> Result<String, std::io::Error> {
    let mut file = tokio::fs::File::open(path).await?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buf).await?;
        if read == 0 {
            break;
        }
        hasher.update(&buf[..read]);
    }
    Ok(hex::encode(hasher.finalize()))
}

fn short_digest(digest: &str) -> String {
    if digest.len() <= 16 {
        digest.to_string()
    } else {
        format!("{}...", &digest[..16])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_sha256_checksum() {
        let spec =
            parse("sha-256=e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855")
                .unwrap();
        assert_eq!(spec.algorithm_label(), "sha-256");
        assert_eq!(spec.pending_label(), "sha-256 pending");
    }

    #[test]
    fn rejects_bad_checksum_shape() {
        assert!(parse("md5=abc").unwrap_err().contains("unsupported"));
        assert!(parse("sha-256=abc").unwrap_err().contains("64 hex"));
        assert!(
            parse("sha-256=zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz")
                .unwrap_err()
                .contains("valid hex")
        );
    }

    #[tokio::test]
    async fn verifies_sha256_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty.bin");
        tokio::fs::write(&path, b"").await.unwrap();
        let spec = parse("sha256=e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855")
            .unwrap();
        let report = verify_file(&path, &spec).await.unwrap();
        assert!(report.verified);
        assert_eq!(report.status_label(), "sha-256 verified");
    }

    #[tokio::test]
    async fn reports_sha256_mismatch() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("data.bin");
        tokio::fs::write(&path, b"hello").await.unwrap();
        let spec = parse("sha256=e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855")
            .unwrap();
        let err = verify_file(&path, &spec).await.unwrap_err();
        match err {
            ChecksumError::Mismatch(report) => {
                assert!(!report.verified);
                assert_eq!(report.status_label(), "sha-256 mismatch");
            }
            ChecksumError::Io(_) => panic!("expected mismatch"),
        }
    }
}
