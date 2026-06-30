use std::sync::Arc;

use http::header::HeaderMap;
use http::{Method, Uri};

use super::headers;
use super::{MessageSignatureConfig, MessageSignatureError, MessageSignatureSigner};

#[derive(Clone)]
pub(crate) struct AutomaticMessageSignature {
    config: MessageSignatureConfig,
    signer: Arc<dyn MessageSignatureSigner>,
}

impl AutomaticMessageSignature {
    pub(crate) fn new(
        config: MessageSignatureConfig,
        signer: Arc<dyn MessageSignatureSigner>,
    ) -> Self {
        Self { config, signer }
    }

    pub(crate) fn sign_headers(
        &self,
        method: &Method,
        target_uri: &Uri,
        request_target: &Uri,
        headers: &mut HeaderMap,
    ) -> Result<(), MessageSignatureError> {
        headers::remove_label(headers, self.config.label())?;
        let signature_headers = self.config.sign_request(
            method,
            target_uri,
            request_target,
            headers,
            self.signer.as_ref(),
        )?;
        signature_headers.insert_into(headers)?;
        Ok(())
    }
}
