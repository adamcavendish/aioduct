use std::sync::Arc;

use http::header::HeaderMap;
use http::{Method, Uri};

use super::headers;
use super::{
    MessageSignatureAsyncSigner, MessageSignatureBase, MessageSignatureConfig,
    MessageSignatureError, MessageSignatureHeaders, MessageSignatureLocalAsyncSigner,
    MessageSignatureSigner,
};

#[derive(Clone)]
pub(crate) struct AutomaticMessageSignature {
    config: MessageSignatureConfig,
    signer: AutomaticMessageSignatureSigner,
}

#[derive(Clone)]
enum AutomaticMessageSignatureSigner {
    Sync(Arc<dyn MessageSignatureSigner>),
    AsyncSend(Arc<dyn MessageSignatureAsyncSigner>),
    AsyncLocal(Arc<dyn MessageSignatureLocalAsyncSigner>),
}

pub(crate) struct PreparedAutomaticMessageSignature {
    config: MessageSignatureConfig,
    base: MessageSignatureBase,
    signer: AutomaticMessageSignatureSigner,
}

impl AutomaticMessageSignature {
    pub(crate) fn new(
        config: MessageSignatureConfig,
        signer: Arc<dyn MessageSignatureSigner>,
    ) -> Self {
        Self {
            config,
            signer: AutomaticMessageSignatureSigner::Sync(signer),
        }
    }

    pub(crate) fn new_async_send(
        config: MessageSignatureConfig,
        signer: Arc<dyn MessageSignatureAsyncSigner>,
    ) -> Self {
        Self {
            config,
            signer: AutomaticMessageSignatureSigner::AsyncSend(signer),
        }
    }

    pub(crate) fn new_async_local(
        config: MessageSignatureConfig,
        signer: Arc<dyn MessageSignatureLocalAsyncSigner>,
    ) -> Self {
        Self {
            config,
            signer: AutomaticMessageSignatureSigner::AsyncLocal(signer),
        }
    }

    pub(crate) fn prepare_headers(
        &self,
        method: &Method,
        target_uri: &Uri,
        request_target: &Uri,
        headers: &mut HeaderMap,
    ) -> Result<PreparedAutomaticMessageSignature, MessageSignatureError> {
        headers::remove_label(headers, self.config.label())?;
        let base = self
            .config
            .signature_base(method, target_uri, request_target, headers)?;
        Ok(PreparedAutomaticMessageSignature {
            config: self.config.clone(),
            base,
            signer: self.signer.clone(),
        })
    }
}

impl PreparedAutomaticMessageSignature {
    pub(crate) async fn sign_send(self) -> Result<MessageSignatureHeaders, MessageSignatureError> {
        let signature = match self.signer {
            AutomaticMessageSignatureSigner::Sync(signer) => signer.sign(self.base.as_bytes())?,
            AutomaticMessageSignatureSigner::AsyncSend(signer) => signer.sign(self.base).await?,
            AutomaticMessageSignatureSigner::AsyncLocal(_) => {
                return Err(MessageSignatureError::Signer(
                    "local async message signature signer cannot run on the send signing path"
                        .to_owned(),
                ));
            }
        };
        self.config.headers_from_signature(signature)
    }

    pub(crate) async fn sign_local(self) -> Result<MessageSignatureHeaders, MessageSignatureError> {
        let signature = match self.signer {
            AutomaticMessageSignatureSigner::Sync(signer) => signer.sign(self.base.as_bytes())?,
            AutomaticMessageSignatureSigner::AsyncSend(signer) => signer.sign(self.base).await?,
            AutomaticMessageSignatureSigner::AsyncLocal(signer) => {
                signer.sign_local(self.base).await?
            }
        };
        self.config.headers_from_signature(signature)
    }
}
