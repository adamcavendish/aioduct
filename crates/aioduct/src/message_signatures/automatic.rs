use std::sync::Arc;

use http::header::HeaderMap;
use http::{Method, StatusCode, Uri};

use super::component::{MessageSignatureComponentKind, MessageSignatureComponentTarget};
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

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct ForwardResponseSignatureRequirements {
    pub(crate) has_related_request_components: bool,
    pub(crate) has_trailer_components: bool,
    pub(crate) requires_full_downstream_target_uri: bool,
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

    pub(crate) fn forward_response_requirements(
        &self,
    ) -> Result<ForwardResponseSignatureRequirements, MessageSignatureError> {
        self.config.validate_components()?;

        let mut requirements = ForwardResponseSignatureRequirements::default();
        for component in self.config.components() {
            if component.has_trailer_parameter() {
                requirements.has_trailer_components = true;
                continue;
            }

            if component.has_related_request_parameter() {
                requirements.has_related_request_components = true;
                if matches!(
                    component.target(),
                    MessageSignatureComponentTarget::Response
                ) {
                    return Err(MessageSignatureError::UnsupportedComponentParameters(
                        component.identifier()?,
                    ));
                }
                if matches!(
                    component.kind(),
                    MessageSignatureComponentKind::Scheme
                        | MessageSignatureComponentKind::Authority
                        | MessageSignatureComponentKind::TargetUri
                ) {
                    requirements.requires_full_downstream_target_uri = true;
                }
            } else if matches!(component.target(), MessageSignatureComponentTarget::Request) {
                return Err(MessageSignatureError::ComponentNotAvailable {
                    component: component.identifier()?,
                    context: "response",
                });
            }
        }

        Ok(requirements)
    }

    pub(crate) fn prepare_response_headers(
        &self,
        status: StatusCode,
        headers: &mut HeaderMap,
    ) -> Result<PreparedAutomaticMessageSignature, MessageSignatureError> {
        headers::remove_label(headers, self.config.label())?;
        let base = self.config.response_signature_base(status, headers)?;
        Ok(PreparedAutomaticMessageSignature {
            config: self.config.clone(),
            base,
            signer: self.signer.clone(),
        })
    }

    pub(crate) fn prepare_request_response_headers(
        &self,
        method: &Method,
        target_uri: &Uri,
        request_target: &Uri,
        request_headers: &HeaderMap,
        status: StatusCode,
        response_headers: &mut HeaderMap,
    ) -> Result<PreparedAutomaticMessageSignature, MessageSignatureError> {
        headers::remove_label(response_headers, self.config.label())?;
        let base = self.config.request_response_signature_base(
            method,
            target_uri,
            request_target,
            request_headers,
            status,
            response_headers,
        )?;
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
