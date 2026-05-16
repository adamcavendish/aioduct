use std::cell::RefCell;
use std::marker::PhantomData;
use std::rc::Rc;

use bytes::{BufMut, BytesMut};
use http::HeaderValue;
use http::header::{ACCEPT_RANGES, CONTENT_LENGTH, RANGE};

use super::ChunkDownloadResult;
use crate::client::HttpEngineLocal;
use crate::error::Error;
use crate::runtime::{Connector, RuntimeLocal};

type ChunkResults = Rc<RefCell<Vec<Option<Result<bytes::Bytes, Error>>>>>;

/// Parallel range-request downloader for `!Send` runtimes.
pub struct ChunkDownloadLocal<R: RuntimeLocal, C: Connector + Clone> {
    client: HttpEngineLocal<R, C>,
    url: String,
    chunks: usize,
    _runtime: PhantomData<(R, C)>,
}

impl<R: RuntimeLocal, C: Connector + Clone> std::fmt::Debug for ChunkDownloadLocal<R, C> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChunkDownloadLocal")
            .field("url", &self.url)
            .finish()
    }
}

impl<R: RuntimeLocal, C: Connector + Clone> ChunkDownloadLocal<R, C> {
    pub(crate) fn new(client: HttpEngineLocal<R, C>, url: String) -> Self {
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

        let head_resp = client
            .request_local(http::Method::HEAD, &url)?
            .send()
            .await?;

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
                let resp = client.get_local(&url)?.send().await?;
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

        let results: ChunkResults = Rc::new(RefCell::new((0..num_chunks).map(|_| None).collect()));
        let done_count = Rc::new(std::cell::Cell::new(0usize));

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
            let results = Rc::clone(&results);
            let done_count = Rc::clone(&done_count);

            R::spawn_local(async move {
                let result: Result<bytes::Bytes, Error> = async {
                    let range_header = HeaderValue::from_str(&range_value)
                        .map_err(|e| Error::Other(Box::new(e)))?;
                    let resp = client
                        .get_local(&url)?
                        .header(RANGE, range_header)
                        .send()
                        .await?;

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

                results.borrow_mut()[i] = Some(result);
                done_count.set(done_count.get() + 1);
            });
        }

        loop {
            if done_count.get() == num_chunks {
                break;
            }
            R::sleep(std::time::Duration::from_millis(1)).await;
        }

        let chunk_data = Rc::try_unwrap(results)
            .map_err(|_| Error::Other("failed to unwrap results".into()))?
            .into_inner();

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

#[cfg(all(test, feature = "compio"))]
mod tests {
    use super::*;
    use crate::runtime::compio_rt::{CompioRuntime, TcpConnector};

    fn test_client() -> HttpEngineLocal<CompioRuntime, TcpConnector> {
        HttpEngineLocal::new_local(TcpConnector)
    }

    #[test]
    fn debug_format() {
        let client = test_client();
        let dl = ChunkDownloadLocal::<CompioRuntime, TcpConnector>::new(
            client,
            "http://example.com/file.bin".into(),
        );
        let dbg = format!("{:?}", dl);
        assert!(dbg.contains("ChunkDownloadLocal"));
        assert!(dbg.contains("http://example.com/file.bin"));
    }

    #[test]
    fn chunks_sets_value() {
        let client = test_client();
        let dl = ChunkDownloadLocal::<CompioRuntime, TcpConnector>::new(
            client,
            "http://example.com/file.bin".into(),
        )
        .chunks(8);
        assert_eq!(dl.chunks, 8);
    }

    #[test]
    fn chunks_clamps_to_one() {
        let client = test_client();
        let dl = ChunkDownloadLocal::<CompioRuntime, TcpConnector>::new(
            client,
            "http://example.com/file.bin".into(),
        )
        .chunks(0);
        assert_eq!(dl.chunks, 1);
    }

    #[test]
    fn default_chunks_is_four() {
        let client = test_client();
        let dl = ChunkDownloadLocal::<CompioRuntime, TcpConnector>::new(
            client,
            "http://example.com/file.bin".into(),
        );
        assert_eq!(dl.chunks, 4);
    }
}
