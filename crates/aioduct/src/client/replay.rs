use http::Method;

/// Whether the bytes in a request body can be reproduced for another dispatch.
///
/// This classification deliberately says nothing about whether sending the
/// complete request again is safe. [`RequestReplayPolicy`] combines the body
/// state with request semantics and the evidence from the failed dispatch.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum BodyReplayability {
    Empty,
    Replayable,
    OneShot,
}

impl BodyReplayability {
    /// Generic forwarded bodies are one-shot unless they are already empty.
    /// Request-builder paths mark buffered bodies as replayable explicitly.
    pub(crate) fn for_forwarded_body<B>(body: &B) -> Self
    where
        B: http_body::Body + ?Sized,
    {
        if body.is_end_stream() {
            Self::Empty
        } else {
            Self::OneShot
        }
    }

    pub(crate) fn can_start_on_pooled_connection(self) -> bool {
        matches!(self, Self::Empty | Self::Replayable)
    }

    /// Once a fresh connection has already been acquired, replacing it with a
    /// pooled connection is safe only when a later dispatch failure can rebuild
    /// the request. The post-connect race path does not own Hyper's exact
    /// unsent-request recovery boundary.
    pub(crate) fn can_replace_fresh_connection(self) -> bool {
        self.can_reproduce()
    }

    pub(crate) fn can_reproduce(self) -> bool {
        matches!(self, Self::Empty | Self::Replayable)
    }
}

/// Why dispatch is considering sending a request again.
// The focused follow-up commits wire each reason into its owning dispatch path.
#[allow(dead_code)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ReplayReason {
    /// A configured retry rule selected another attempt. The caller records
    /// whether the method was authorized by the built-in or custom policy.
    Configured { method_authorized: bool },
    /// The transport failed without proving whether the peer processed the
    /// request.
    AmbiguousTransportFailure,
    /// The transport proved that the peer did not process the request.
    ProvenUnprocessed,
    /// The transport returned the exact request before serialization began.
    ExactRequestRecovered,
}

/// Private replay contract for a complete request.
///
/// State transition:
///
/// ```text
/// retained request
///   |-- exact request returned --------------------> replay permitted
///   |-- body reproducible + policy/evidence allows -> replay permitted
///   `-- body consumed or evidence is ambiguous ----> terminal outcome
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RequestReplayPolicy {
    body: BodyReplayability,
    method_is_idempotent: bool,
}

impl RequestReplayPolicy {
    pub(crate) fn new(method: &Method, body: BodyReplayability) -> Self {
        Self {
            body,
            method_is_idempotent: crate::retry::is_idempotent(method),
        }
    }

    pub(crate) fn permits(self, reason: ReplayReason) -> bool {
        if reason == ReplayReason::ExactRequestRecovered {
            return true;
        }
        if !self.body.can_reproduce() {
            return false;
        }

        match reason {
            ReplayReason::Configured { method_authorized } => method_authorized,
            ReplayReason::AmbiguousTransportFailure => self.method_is_idempotent,
            ReplayReason::ProvenUnprocessed => true,
            ReplayReason::ExactRequestRecovered => true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configured_retry_requires_body_and_method_authorization() {
        for body in [BodyReplayability::Empty, BodyReplayability::Replayable] {
            let get = RequestReplayPolicy::new(&Method::GET, body);
            let post = RequestReplayPolicy::new(&Method::POST, body);

            assert!(get.permits(ReplayReason::Configured {
                method_authorized: true,
            }));
            assert!(post.permits(ReplayReason::Configured {
                method_authorized: true,
            }));
            assert!(!get.permits(ReplayReason::Configured {
                method_authorized: false,
            }));
        }

        let streaming = RequestReplayPolicy::new(&Method::GET, BodyReplayability::OneShot);
        assert!(!streaming.permits(ReplayReason::Configured {
            method_authorized: true,
        }));
    }

    #[test]
    fn ambiguous_failures_require_idempotent_reproducible_requests() {
        for body in [BodyReplayability::Empty, BodyReplayability::Replayable] {
            assert!(
                RequestReplayPolicy::new(&Method::GET, body)
                    .permits(ReplayReason::AmbiguousTransportFailure)
            );
            assert!(
                RequestReplayPolicy::new(&Method::PUT, body)
                    .permits(ReplayReason::AmbiguousTransportFailure)
            );
            assert!(
                !RequestReplayPolicy::new(&Method::POST, body)
                    .permits(ReplayReason::AmbiguousTransportFailure)
            );
        }

        assert!(
            !RequestReplayPolicy::new(&Method::GET, BodyReplayability::OneShot)
                .permits(ReplayReason::AmbiguousTransportFailure)
        );
    }

    #[test]
    fn proven_unprocessed_still_requires_reproducible_body() {
        assert!(
            RequestReplayPolicy::new(&Method::POST, BodyReplayability::Replayable)
                .permits(ReplayReason::ProvenUnprocessed)
        );
        assert!(
            !RequestReplayPolicy::new(&Method::POST, BodyReplayability::OneShot)
                .permits(ReplayReason::ProvenUnprocessed)
        );
    }

    #[test]
    fn exact_request_recovery_preserves_one_shot_requests() {
        assert!(
            RequestReplayPolicy::new(&Method::POST, BodyReplayability::OneShot)
                .permits(ReplayReason::ExactRequestRecovered)
        );
    }
    #[test]
    fn one_shot_request_keeps_an_already_acquired_fresh_connection() {
        assert!(BodyReplayability::Empty.can_replace_fresh_connection());
        assert!(BodyReplayability::Replayable.can_replace_fresh_connection());
        assert!(!BodyReplayability::OneShot.can_replace_fresh_connection());
    }
}
