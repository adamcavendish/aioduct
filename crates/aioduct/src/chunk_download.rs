use std::marker::PhantomData;
use std::sync::{Arc, Mutex};

use bytes::{BufMut, BytesMut};
use http::HeaderValue;
use http::header::{ACCEPT_RANGES, CONTENT_LENGTH, RANGE};

use crate::client::Client;
use crate::error::Error;
use crate::runtime::Runtime;

/// Parallel range-request downloader for large files.
pub struct ChunkDownload<R: Runtime> {
    client: Client<R>,
    url: String,
    chunks: usize,
    _runtime: PhantomData<R>,
}

impl<R: Runtime> std::fmt::Debug for ChunkDownload<R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChunkDownload")
            .field("url", &self.url)
            .finish()
    }
}

/// Result of a parallel chunk download.
#[derive(Debug)]
pub struct ChunkDownloadResult {
    /// Total size of the downloaded file in bytes.
    pub total_size: u64,
    /// The reassembled file data.
    pub data: bytes::Bytes,
}

type ChunkResults = Arc<Mutex<Vec<Option<std::result::Result<bytes::Bytes, Error>>>>>;

impl<R: Runtime> ChunkDownload<R> {
    pub(crate) fn new(client: Client<R>, url: String) -> Self {
        Self {
            client,
            url,
            chunks: 4,
            _runtime: PhantomData,
        }
    }

    /// Set the number of parallel chunks (default: 4).
    pub fn chunks(mut self, n: usize) -> Self {
        self.chunks = n.max(1);
        self
    }

    /// Execute the download and return the reassembled data.
    pub async fn download(self) -> Result<ChunkDownloadResult, Error> {
        let client = self.client;
        let url = self.url;

        let head_resp = client.head(&url)?.send().await?;

        if !head_resp.status().is_success() {
            return Err(Error::Other(
                format!("HEAD request failed: {}", head_resp.status()).into(),
            ));
        }

        let accepts_ranges = head_resp
            .headers()
            .get(ACCEPT_RANGES)
            .and_then(|v| v.to_str().ok())
            .map(|v| v.contains("bytes"))
            .unwrap_or(false);

        let content_length = head_resp
            .headers()
            .get(CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok());

        let total_size = match content_length {
            Some(len) if accepts_ranges && len > 0 => len,
            _ => {
                let resp = client.get(&url)?.send().await?;
                let data = resp.bytes().await?;
                let len = data.len() as u64;
                return Ok(ChunkDownloadResult {
                    total_size: len,
                    data,
                });
            }
        };

        let num_chunks = (self.chunks as u64).min(total_size) as usize;
        let chunk_size = total_size / num_chunks as u64;

        let results: ChunkResults = Arc::new(Mutex::new((0..num_chunks).map(|_| None).collect()));
        let done_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));

        for i in 0..num_chunks {
            let start = i as u64 * chunk_size;
            let end = if i == num_chunks - 1 {
                total_size - 1
            } else {
                (i as u64 + 1) * chunk_size - 1
            };

            let url = url.clone();
            let range_value = format!("bytes={start}-{end}");
            let client = client.clone();
            let results = Arc::clone(&results);
            let done_count = Arc::clone(&done_count);

            R::spawn(async move {
                let result: std::result::Result<bytes::Bytes, Error> = async {
                    let range_header = HeaderValue::from_str(&range_value)
                        .map_err(|e| Error::Other(Box::new(e)))?;
                    let resp = client.get(&url)?.header(RANGE, range_header).send().await?;

                    if resp.status() != http::StatusCode::PARTIAL_CONTENT
                        && !resp.status().is_success()
                    {
                        return Err(Error::Other(
                            format!("chunk request failed: {}", resp.status()).into(),
                        ));
                    }

                    resp.bytes().await
                }
                .await;

                results.lock().unwrap()[i] = Some(result);
                done_count.fetch_add(1, std::sync::atomic::Ordering::Release);
            });
        }

        loop {
            if done_count.load(std::sync::atomic::Ordering::Acquire) == num_chunks {
                break;
            }
            R::sleep(std::time::Duration::from_millis(1)).await;
        }

        let chunk_data = Arc::try_unwrap(results)
            .map_err(|_| Error::Other("failed to unwrap results".into()))?
            .into_inner()
            .map_err(|_| Error::Other("chunk result mutex poisoned".into()))?;

        let mut buf = BytesMut::with_capacity(total_size as usize);
        for result in chunk_data {
            let data = result.ok_or_else(|| Error::Other("missing chunk".into()))??;
            buf.put(data);
        }

        Ok(ChunkDownloadResult {
            total_size,
            data: buf.freeze(),
        })
    }
}
