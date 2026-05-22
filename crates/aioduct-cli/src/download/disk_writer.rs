use std::fs::{File, OpenOptions};
use std::io;
use std::path::Path;

pub struct DiskWriter {
    file: File,
    total_length: u64,
}

impl DiskWriter {
    pub fn open_or_create(path: &Path, total_length: u64) -> io::Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?;
        file.set_len(total_length)?;
        Ok(Self { file, total_length })
    }

    pub fn total_length(&self) -> u64 {
        self.total_length
    }

    pub fn write_at(&self, offset: u64, data: &[u8]) -> io::Result<()> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::FileExt;
            self.file.write_all_at(data, offset)
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::FileExt;
            self.file.seek_write(data, offset)
        }
    }

    pub fn sync(&self) -> io::Result<()> {
        self.file.sync_data()
    }

    #[cfg(test)]
    pub fn null() -> Self {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/null")
            .unwrap();
        Self {
            file,
            total_length: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_at_offset() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.bin");

        let writer = DiskWriter::open_or_create(&path, 1024).unwrap();
        writer.write_at(100, b"hello").unwrap();
        writer.write_at(500, b"world").unwrap();
        writer.sync().unwrap();

        let data = std::fs::read(&path).unwrap();
        assert_eq!(data.len(), 1024);
        assert_eq!(&data[100..105], b"hello");
        assert_eq!(&data[500..505], b"world");
        assert_eq!(&data[0..5], &[0u8; 5]);
    }

    #[test]
    fn preallocates_to_total_length() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("prealloc.bin");

        let writer = DiskWriter::open_or_create(&path, 8192).unwrap();
        assert_eq!(writer.total_length(), 8192);

        let meta = std::fs::metadata(&path).unwrap();
        assert_eq!(meta.len(), 8192);
    }
}
