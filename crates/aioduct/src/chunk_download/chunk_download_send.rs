use std::marker::PhantomData;

use bytes::{BufMut, BytesMut};
use futures_channel::mpsc;
use futures_core::Stream;
use http::HeaderValue;
use http::header::{ACCEPT_RANGES, CONTENT_LENGTH, RANGE};

use crate::client::HttpEngineSend;
use crate::error::Error;
use crate::runtime::{ConnectorSend, RuntimePoll};

use super::ChunkDownloadResult;

/// Parallel range-request downloader for large files.
pub struct ChunkDownload<R: RuntimePoll, C: ConnectorSend> {
    client: HttpEngineSend<R, C>,
    url: String,
    chunks: usize,
    _runtime: PhantomData<(R, C)>,
}

impl<R: RuntimePoll, C: ConnectorSend> std::fmt::Debug for ChunkDownload<R, C> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChunkDownload")
            .field("url", &self.url)
            .finish()
    }
}

impl<R: RuntimePoll, C: ConnectorSend> ChunkDownload<R, C> {
    pub(crate) fn new(client: HttpEngineSend<R, C>, url: String) -> Self {
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

        let (tx, mut rx) = mpsc::unbounded::<(usize, Result<bytes::Bytes, Error>)>();

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
            let tx = tx.clone();

            R::spawn_send(async move {
                let result: Result<bytes::Bytes, Error> = async {
                    let range_header = HeaderValue::from_str(&range_value)
                        .map_err(|e| Error::Other(Box::new(e)))?;
                    let resp = client.get(&url)?.header(RANGE, range_header).send().await?;

                    if resp.status() != http::StatusCode::PARTIAL_CONTENT {
                        return Err(Error::Other(
                            format!(
                                "chunk request failed: expected 206 Partial Content, got {}",
                                resp.status()
                            )
                            .into(),
                        ));
                    }

                    resp.bytes().await
                }
                .await;

                let _ = tx.unbounded_send((i, result));
            });
        }

        drop(tx);

        let mut results: Vec<Option<Result<bytes::Bytes, Error>>> =
            (0..num_chunks).map(|_| None).collect();
        let mut received = 0;

        while received < num_chunks {
            let msg = std::future::poll_fn(|cx| std::pin::Pin::new(&mut rx).poll_next(cx)).await;
            match msg {
                Some((idx, result)) => {
                    results[idx] = Some(result);
                    received += 1;
                }
                None => {
                    return Err(Error::Other(
                        format!(
                            "chunk download tasks failed: received {received}/{num_chunks} results"
                        )
                        .into(),
                    ));
                }
            }
        }

        let mut buf = BytesMut::with_capacity(total_size as usize);
        for result in results {
            let data = result.ok_or_else(|| Error::Other("missing chunk".into()))??;
            buf.put(data);
        }

        Ok(ChunkDownloadResult {
            total_size,
            data: buf.freeze(),
        })
    }
}

#[cfg(all(test, feature = "tokio"))]
mod tests {
    use super::*;
    use crate::runtime::TokioRuntime;
    use crate::runtime::tokio_rt::TcpConnector;

    #[test]
    fn chunks_clamps_to_one() {
        let client = crate::HttpEngineSend::<TokioRuntime, TcpConnector>::new(TcpConnector);
        let dl = client.chunk_download("http://example.com/file");
        let dl = dl.chunks(0);
        assert_eq!(dl.chunks, 1);
    }

    #[test]
    fn chunks_accepts_large_value() {
        let client = crate::HttpEngineSend::<TokioRuntime, TcpConnector>::new(TcpConnector);
        let dl = client.chunk_download("http://example.com/file").chunks(100);
        assert_eq!(dl.chunks, 100);
    }

    #[test]
    fn debug_format_includes_url() {
        let client = crate::HttpEngineSend::<TokioRuntime, TcpConnector>::new(TcpConnector);
        let dl = client.chunk_download("http://example.com/large.bin");
        let dbg = format!("{dl:?}");
        assert!(dbg.contains("ChunkDownload"));
        assert!(dbg.contains("large.bin"));
    }

    #[test]
    fn range_splitting_single_chunk() {
        let total_size: u64 = 100;
        let start = 0u64;
        let end = total_size - 1;
        assert_eq!(start, 0);
        assert_eq!(end, 99);
    }

    #[test]
    fn range_splitting_even() {
        let total_size: u64 = 100;
        let num_chunks: usize = 4;
        let chunk_size = total_size / num_chunks as u64;
        let mut ranges = Vec::new();
        for i in 0..num_chunks {
            let start = i as u64 * chunk_size;
            let end = if i == num_chunks - 1 {
                total_size - 1
            } else {
                (i as u64 + 1) * chunk_size - 1
            };
            ranges.push((start, end));
        }
        assert_eq!(ranges, vec![(0, 24), (25, 49), (50, 74), (75, 99)]);
    }

    #[test]
    fn range_splitting_uneven() {
        let total_size: u64 = 10;
        let num_chunks: usize = 3;
        let chunk_size = total_size / num_chunks as u64;
        let mut ranges = Vec::new();
        for i in 0..num_chunks {
            let start = i as u64 * chunk_size;
            let end = if i == num_chunks - 1 {
                total_size - 1
            } else {
                (i as u64 + 1) * chunk_size - 1
            };
            ranges.push((start, end));
        }
        assert_eq!(ranges[0], (0, 2));
        assert_eq!(ranges[1], (3, 5));
        assert_eq!(ranges[2], (6, 9));
    }

    #[test]
    fn num_chunks_capped_at_total_size() {
        let total_size: u64 = 3;
        let requested_chunks: u64 = 10;
        let num_chunks = requested_chunks.min(total_size) as usize;
        assert_eq!(num_chunks, 3);
    }

    #[test]
    fn chunk_download_result_debug() {
        let result = ChunkDownloadResult {
            total_size: 42,
            data: bytes::Bytes::from("hello"),
        };
        let dbg = format!("{result:?}");
        assert!(dbg.contains("42"));
    }

    #[test]
    fn reassembly_order() {
        let mut buf = BytesMut::with_capacity(9);
        buf.put(&b"abc"[..]);
        buf.put(&b"def"[..]);
        buf.put(&b"ghi"[..]);
        assert_eq!(&buf[..], b"abcdefghi");
    }
}
