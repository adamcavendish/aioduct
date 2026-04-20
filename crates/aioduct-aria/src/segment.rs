use std::io::SeekFrom;
use std::path::Path;
use std::sync::Arc;

use aioduct::Client;
use aioduct::runtime::TokioRuntime;
use tokio::fs::OpenOptions;
use tokio::io::{AsyncSeekExt, AsyncWriteExt};

use crate::request_config::ExtraRequestConfig;

pub struct SegmentInfo {
    pub start: u64,
    pub end: u64,
}

pub fn compute_count(total: u64, split: usize, min_split_size: u64) -> usize {
    if total == 0 {
        return 1;
    }
    let max_by_size = (total / min_split_size).max(1) as usize;
    split.min(max_by_size)
}

pub fn split_range(start: u64, end: u64, count: usize) -> Vec<SegmentInfo> {
    let total = end - start + 1;
    let chunk_size = total / count as u64;
    let remainder = total % count as u64;

    let mut segments = Vec::with_capacity(count);
    let mut offset = start;

    for i in 0..count {
        let extra = if (i as u64) < remainder { 1 } else { 0 };
        let seg_end = offset + chunk_size + extra - 1;
        segments.push(SegmentInfo {
            start: offset,
            end: seg_end,
        });
        offset = seg_end + 1;
    }

    segments
}

pub async fn download_segment(
    client: &Client<TokioRuntime>,
    url: &str,
    output: &Path,
    seg: &SegmentInfo,
    extra: &Arc<ExtraRequestConfig>,
) -> Result<u64, aioduct::Error> {
    let range = format!("bytes={}-{}", seg.start, seg.end);
    let mut req = client.get(url)?;

    if let Ok(v) = range.parse::<http::HeaderValue>() {
        req = req.header(http::header::RANGE, v);
    }
    req = extra.apply_to(req);

    let resp = req.send().await?;

    let status = resp.status();
    if !status.is_success() && status.as_u16() != 206 {
        return Err(aioduct::Error::Status(status));
    }

    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(output)
        .await
        .map_err(aioduct::Error::Io)?;

    file.seek(SeekFrom::Start(seg.start))
        .await
        .map_err(aioduct::Error::Io)?;

    let mut stream = resp.into_bytes_stream();
    let mut written: u64 = 0;
    let expected = seg.end - seg.start + 1;

    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        let remaining = expected - written;
        let to_write = if chunk.len() as u64 > remaining {
            &chunk[..remaining as usize]
        } else {
            &chunk
        };
        file.write_all(to_write).await.map_err(aioduct::Error::Io)?;
        written += to_write.len() as u64;
        if written >= expected {
            break;
        }
    }

    file.flush().await.map_err(aioduct::Error::Io)?;
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_even() {
        let segs = split_range(0, 99, 4);
        assert_eq!(segs.len(), 4);
        assert_eq!(segs[0].start, 0);
        assert_eq!(segs[0].end, 24);
        assert_eq!(segs[3].start, 75);
        assert_eq!(segs[3].end, 99);
    }

    #[test]
    fn split_with_remainder() {
        let segs = split_range(0, 9, 3);
        assert_eq!(segs.len(), 3);
        assert_eq!((segs[0].end - segs[0].start + 1), 4);
        assert_eq!((segs[2].end - segs[2].start + 1), 3);
        assert_eq!(segs[2].end, 9);
    }

    #[test]
    fn compute_count_respects_min_split() {
        assert_eq!(compute_count(10_000, 8, 5_000), 2);
        assert_eq!(compute_count(10_000, 8, 1_000), 8);
        assert_eq!(compute_count(0, 4, 1_000), 1);
    }
}
