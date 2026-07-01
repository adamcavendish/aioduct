use http::header::HeaderMap;
use http::{Method, Uri};

use super::{
    MessageSignature, MessageSignatureComponent, MessageSignatureError, MessageSignatureParams,
};

/// Inputs provided to a caller-owned RFC 9421 signature verifier.
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub struct MessageSignatureVerificationInput<'a> {
    label: &'a str,
    params: &'a MessageSignatureParams,
    signature_base: &'a [u8],
    signature: &'a [u8],
}

/// Verification policy for RFC 9421 request signatures.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub struct MessageSignatureVerificationPolicy {
    required_components: Vec<MessageSignatureComponent>,
    accepted_algorithms: Vec<String>,
    accepted_key_ids: Vec<String>,
    validation_time: Option<u64>,
    max_age: Option<u64>,
    clock_skew: u64,
    require_created: bool,
}

/// Caller-owned cryptographic verifier for RFC 9421 signatures.
pub trait MessageSignatureVerifier {
    /// Verify signature bytes over a rebuilt signature base.
    fn verify(
        &self,
        input: MessageSignatureVerificationInput<'_>,
    ) -> Result<bool, MessageSignatureError>;
}

impl<F> MessageSignatureVerifier for F
where
    F: for<'a> Fn(MessageSignatureVerificationInput<'a>) -> Result<bool, MessageSignatureError>,
{
    fn verify(
        &self,
        input: MessageSignatureVerificationInput<'_>,
    ) -> Result<bool, MessageSignatureError> {
        self(input)
    }
}

impl<'a> MessageSignatureVerificationInput<'a> {
    fn new(
        label: &'a str,
        params: &'a MessageSignatureParams,
        signature_base: &'a [u8],
        signature: &'a [u8],
    ) -> Self {
        Self {
            label,
            params,
            signature_base,
            signature,
        }
    }

    /// Return the selected signature label.
    pub fn label(&self) -> &'a str {
        self.label
    }

    /// Return the parsed signature metadata parameters.
    pub fn params(&self) -> &'a MessageSignatureParams {
        self.params
    }

    /// Return the rebuilt signature base bytes.
    pub fn signature_base(&self) -> &'a [u8] {
        self.signature_base
    }

    /// Return the decoded signature bytes.
    pub fn signature(&self) -> &'a [u8] {
        self.signature
    }
}

impl MessageSignatureVerificationPolicy {
    /// Create a permissive verification policy.
    pub fn new() -> Self {
        Self::default()
    }

    /// Require one covered component to be present in the parsed signature input.
    pub fn required_component(mut self, component: MessageSignatureComponent) -> Self {
        self.required_components.push(component);
        self
    }

    /// Require covered components to be present in the parsed signature input.
    pub fn required_components_iter(
        mut self,
        components: impl IntoIterator<Item = MessageSignatureComponent>,
    ) -> Self {
        self.required_components.extend(components);
        self
    }

    /// Accept one `alg` metadata value.
    pub fn accepted_algorithm(mut self, algorithm: impl Into<String>) -> Self {
        self.accepted_algorithms.push(algorithm.into());
        self
    }

    /// Accept `alg` metadata values.
    pub fn accepted_algorithms_iter(
        mut self,
        algorithms: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        self.accepted_algorithms
            .extend(algorithms.into_iter().map(Into::into));
        self
    }

    /// Accept one `keyid` metadata value.
    pub fn accepted_key_id(mut self, key_id: impl Into<String>) -> Self {
        self.accepted_key_ids.push(key_id.into());
        self
    }

    /// Accept `keyid` metadata values.
    pub fn accepted_key_ids_iter(
        mut self,
        key_ids: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        self.accepted_key_ids
            .extend(key_ids.into_iter().map(Into::into));
        self
    }

    /// Set the UNIX timestamp used for `created`, `expires`, and max-age checks.
    pub fn validation_time(mut self, unix_time: u64) -> Self {
        self.validation_time = Some(unix_time);
        self
    }

    /// Reject signatures older than this many seconds.
    pub fn max_age(mut self, seconds: u64) -> Self {
        self.max_age = Some(seconds);
        self
    }

    /// Allow this many seconds of clock skew around timestamp comparisons.
    pub fn clock_skew(mut self, seconds: u64) -> Self {
        self.clock_skew = seconds;
        self
    }

    /// Require the signature to include the `created` metadata parameter.
    pub fn require_created(mut self) -> Self {
        self.require_created = true;
        self
    }

    /// Parse and verify one request signature selected by label.
    pub fn verify_request(
        &self,
        headers: &HeaderMap,
        label: impl AsRef<str>,
        method: &Method,
        target_uri: &Uri,
        request_target: &Uri,
        verifier: &(impl MessageSignatureVerifier + ?Sized),
    ) -> Result<(), MessageSignatureError> {
        let signature = MessageSignature::from_headers(headers, label)?;
        self.verify_parsed_request(
            &signature,
            method,
            target_uri,
            request_target,
            headers,
            verifier,
        )
    }

    pub(crate) fn verify_parsed_request(
        &self,
        signature: &MessageSignature,
        method: &Method,
        target_uri: &Uri,
        request_target: &Uri,
        headers: &HeaderMap,
        verifier: &(impl MessageSignatureVerifier + ?Sized),
    ) -> Result<(), MessageSignatureError> {
        self.validate_policy(signature)?;
        let base = signature.signature_base(method, target_uri, request_target, headers)?;
        if verifier.verify(MessageSignatureVerificationInput::new(
            signature.label(),
            signature.params(),
            base.as_bytes(),
            signature.signature(),
        ))? {
            Ok(())
        } else {
            Err(MessageSignatureError::VerificationFailed)
        }
    }

    fn validate_policy(&self, signature: &MessageSignature) -> Result<(), MessageSignatureError> {
        self.validate_required_components(signature)?;
        self.validate_algorithm(signature.params())?;
        self.validate_key_id(signature.params())?;
        self.validate_timestamps(signature.params())?;
        Ok(())
    }

    fn validate_required_components(
        &self,
        signature: &MessageSignature,
    ) -> Result<(), MessageSignatureError> {
        for required in &self.required_components {
            let required_key = required.comparison_key();
            if signature
                .components()
                .iter()
                .all(|component| component.comparison_key() != required_key)
            {
                return Err(MessageSignatureError::MissingRequiredComponent(
                    required.identifier()?,
                ));
            }
        }
        Ok(())
    }

    fn validate_algorithm(
        &self,
        params: &MessageSignatureParams,
    ) -> Result<(), MessageSignatureError> {
        if self.accepted_algorithms.is_empty() {
            return Ok(());
        }
        let Some(algorithm) = params.algorithm() else {
            return Err(MessageSignatureError::UnacceptableAlgorithm(None));
        };
        if self
            .accepted_algorithms
            .iter()
            .any(|accepted| accepted == algorithm)
        {
            Ok(())
        } else {
            Err(MessageSignatureError::UnacceptableAlgorithm(Some(
                algorithm.to_owned(),
            )))
        }
    }

    fn validate_key_id(
        &self,
        params: &MessageSignatureParams,
    ) -> Result<(), MessageSignatureError> {
        if self.accepted_key_ids.is_empty() {
            return Ok(());
        }
        let Some(key_id) = params.key_id() else {
            return Err(MessageSignatureError::UnknownKeyId(None));
        };
        if self
            .accepted_key_ids
            .iter()
            .any(|accepted| accepted == key_id)
        {
            Ok(())
        } else {
            Err(MessageSignatureError::UnknownKeyId(Some(key_id.to_owned())))
        }
    }

    fn validate_timestamps(
        &self,
        params: &MessageSignatureParams,
    ) -> Result<(), MessageSignatureError> {
        if self.require_created && params.created().is_none() {
            return Err(MessageSignatureError::MissingSignatureParameter("created"));
        }
        if self.max_age.is_some() && self.validation_time.is_none() {
            return Err(MessageSignatureError::MissingValidationTime);
        }
        if self.validation_time.is_none()
            && (params.created().is_some() || params.expires().is_some())
        {
            return Err(MessageSignatureError::MissingValidationTime);
        }

        let Some(now) = self.validation_time else {
            return Ok(());
        };

        if let Some(created) = params.created() {
            if created > now.saturating_add(self.clock_skew) {
                return Err(MessageSignatureError::SignatureCreatedInFuture { created, now });
            }
            if let Some(max_age) = self.max_age {
                let latest = created
                    .saturating_add(max_age)
                    .saturating_add(self.clock_skew);
                if now > latest {
                    return Err(MessageSignatureError::SignatureTooOld {
                        created,
                        now,
                        max_age,
                    });
                }
            }
        } else if self.max_age.is_some() {
            return Err(MessageSignatureError::MissingSignatureParameter("created"));
        }

        if let Some(expires) = params.expires()
            && now > expires.saturating_add(self.clock_skew)
        {
            return Err(MessageSignatureError::SignatureExpired { expires, now });
        }

        Ok(())
    }
}

impl MessageSignature {
    /// Verify this parsed signature against a request and policy.
    pub fn verify_request(
        &self,
        policy: &MessageSignatureVerificationPolicy,
        method: &Method,
        target_uri: &Uri,
        request_target: &Uri,
        headers: &HeaderMap,
        verifier: &(impl MessageSignatureVerifier + ?Sized),
    ) -> Result<(), MessageSignatureError> {
        policy.verify_parsed_request(self, method, target_uri, request_target, headers, verifier)
    }
}
