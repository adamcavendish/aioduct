mod chunk_download_local;
mod chunk_download_send;

pub use chunk_download_local::ChunkDownloadLocal;
pub use chunk_download_send::ChunkDownload;

/// Result of a parallel chunk download.
#[derive(Debug)]
pub struct ChunkDownloadResult {
    /// Total size of the downloaded file in bytes.
    pub total_size: u64,
    /// The reassembled file data.
    pub data: bytes::Bytes,
}
