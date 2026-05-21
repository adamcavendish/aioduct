use std::path::Path;

use serde::{Deserialize, Serialize};

use super::piece::bitfield::BitfieldMan;
use super::piece::storage::PieceStorage;

#[derive(Serialize, Deserialize)]
pub struct ControlFile {
    pub version: u32,
    pub url: String,
    pub total_length: u64,
    pub piece_length: u32,
    pub bitfield: String,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

impl ControlFile {
    pub fn new(url: &str, total_length: u64, piece_length: u32) -> Self {
        let now = now_iso8601();
        let bitfield = BitfieldMan::new(total_length, piece_length);
        Self {
            version: 1,
            url: url.to_string(),
            total_length,
            piece_length,
            bitfield: bitfield.to_hex(),
            etag: None,
            last_modified: None,
            created_at: now.clone(),
            updated_at: now,
        }
    }

    pub fn from_storage(
        storage: &PieceStorage,
        url: &str,
        etag: Option<&str>,
        last_modified: Option<&str>,
        created_at: &str,
    ) -> Self {
        Self {
            version: 1,
            url: url.to_string(),
            total_length: storage.total_length(),
            piece_length: storage.piece_length(),
            bitfield: storage.bitfield().to_hex(),
            etag: etag.map(|s| s.to_string()),
            last_modified: last_modified.map(|s| s.to_string()),
            created_at: created_at.to_string(),
            updated_at: now_iso8601(),
        }
    }

    pub fn load(path: &Path) -> std::io::Result<Self> {
        let content = std::fs::read_to_string(path)?;
        serde_json::from_str(&content)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        let content = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;

        let dir = path.parent().unwrap_or(Path::new("."));
        let mut tmp = tempfile::NamedTempFile::new_in(dir)?;
        std::io::Write::write_all(&mut tmp, content.as_bytes())?;
        tmp.persist(path).map_err(std::io::Error::other)?;
        Ok(())
    }

    pub fn to_storage(&self) -> Option<PieceStorage> {
        let bitfield = BitfieldMan::from_hex(&self.bitfield, self.total_length, self.piece_length)?;
        Some(PieceStorage::from_bitfield(bitfield))
    }

    pub fn control_path(download_path: &Path) -> std::path::PathBuf {
        let mut p = download_path.as_os_str().to_owned();
        p.push(".aioduct");
        std::path::PathBuf::from(p)
    }
}

fn now_iso8601() -> String {
    use std::time::SystemTime;
    let d = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = d.as_secs();
    let days = secs / 86400;
    let time_of_day = secs % 86400;
    let hours = time_of_day / 3600;
    let minutes = (time_of_day % 3600) / 60;
    let seconds = time_of_day % 60;
    // Approximate date from days since epoch (good enough for a timestamp)
    let (year, month, day) = days_to_date(days);
    format!("{year:04}-{month:02}-{day:02}T{hours:02}:{minutes:02}:{seconds:02}Z")
}

fn days_to_date(days: u64) -> (u64, u64, u64) {
    // Algorithm from http://howardhinnant.github.io/date_algorithms.html
    let z = days + 719468;
    let era = z / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_path_appends_extension() {
        let p = Path::new("/tmp/file.iso");
        assert_eq!(
            ControlFile::control_path(p),
            Path::new("/tmp/file.iso.aioduct")
        );
    }

    #[test]
    fn roundtrip_save_load() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.aioduct");

        let mut cf = ControlFile::new("http://example.com/file.bin", 10240, 1024);
        cf.etag = Some("\"abc123\"".to_string());
        cf.save(&path).unwrap();

        let loaded = ControlFile::load(&path).unwrap();
        assert_eq!(loaded.version, 1);
        assert_eq!(loaded.url, "http://example.com/file.bin");
        assert_eq!(loaded.total_length, 10240);
        assert_eq!(loaded.piece_length, 1024);
        assert_eq!(loaded.etag.as_deref(), Some("\"abc123\""));
    }

    #[test]
    fn storage_roundtrip() {
        let mut storage = PieceStorage::new(4096, 1024);
        storage.mark_complete(0);
        storage.mark_complete(2);

        let cf = ControlFile::from_storage(
            &storage,
            "http://x.com/f",
            None,
            None,
            "2025-01-01T00:00:00Z",
        );
        let restored = cf.to_storage().unwrap();
        assert!(restored.is_complete(0));
        assert!(!restored.is_complete(1));
        assert!(restored.is_complete(2));
        assert!(!restored.is_complete(3));
    }
}
