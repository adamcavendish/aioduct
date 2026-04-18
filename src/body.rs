use bytes::Bytes;
use http_body_util::BodyExt;

use crate::error::{HyperBody, Result};

pub struct BodyStream {
    body: HyperBody,
    done: bool,
}

impl BodyStream {
    pub(crate) fn new(body: HyperBody) -> Self {
        Self { body, done: false }
    }

    pub async fn next(&mut self) -> Option<Result<Bytes>> {
        if self.done {
            return None;
        }

        loop {
            match self.body.frame().await {
                Some(Ok(frame)) => {
                    if let Ok(data) = frame.into_data() {
                        return Some(Ok(data));
                    }
                }
                Some(Err(e)) => {
                    self.done = true;
                    return Some(Err(e));
                }
                None => {
                    self.done = true;
                    return None;
                }
            }
        }
    }
}
