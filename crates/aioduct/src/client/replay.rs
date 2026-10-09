use bytes::Bytes;
use http::header::{AUTHORIZATION, HeaderValue};
use http::{HeaderMap, Method, Uri};
use http_body::Body;

use super::request_replay::ReplayableRequestHead;

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

/// Marks a request that must not begin dispatch on a pooled connection.
///
/// Forwarded one-shot bodies use this stronger constraint because their source
/// may already be coupled to a downstream connection. A fresh upstream
/// connection can still enter the pool after the response completes.
#[derive(Clone, Copy, Debug)]
pub(crate) struct FreshConnectionRequired;

#[derive(Clone, Debug)]
pub(crate) struct AppliedCookieHeader(pub(crate) HeaderValue);

/// Replay-relevant state after preparation has finalized the wire request.
///
/// Retry policy and replay both use the method, body state, and immutable head
/// that were actually dispatched. The retained builder is not replay input.
#[derive(Clone)]
pub(crate) struct FinalizedRequestSnapshot {
    effective_uri: Uri,
    head: ReplayableRequestHead,
    body: Bytes,
    body_replayability: BodyReplayability,
    cache_state: FinalizedCacheState,
    fragment: Option<String>,
    digest_challenge: Option<crate::digest_auth::PreparedDigestChallenge>,
    applied_cookie_header: Option<HeaderValue>,
}

#[derive(Clone)]
pub(crate) struct FinalizedCacheState {
    cache_entry: Option<crate::cache::CachedResponse>,
    stale_if_error: Option<std::time::Duration>,
    request_headers: HeaderMap,
    captured_at: std::time::Instant,
}

impl FinalizedCacheState {
    pub(crate) fn new(
        cache_entry: Option<crate::cache::CachedResponse>,
        stale_if_error: Option<std::time::Duration>,
        request_headers: HeaderMap,
    ) -> Self {
        Self {
            cache_entry,
            stale_if_error,
            request_headers,
            captured_at: std::time::Instant::now(),
        }
    }

    pub(crate) fn cache_entry(&self) -> Option<crate::cache::CachedResponse> {
        self.cache_entry.clone().map(|mut entry| {
            entry.age = entry.age.saturating_add(self.captured_at.elapsed());
            entry
        })
    }

    pub(crate) fn stale_if_error(&self) -> Option<std::time::Duration> {
        self.stale_if_error
    }

    pub(crate) fn request_headers(&self) -> &HeaderMap {
        &self.request_headers
    }
}

impl FinalizedRequestSnapshot {
    pub(crate) fn capture<B>(
        request: &http::Request<B>,
        effective_uri: &Uri,
        replay_body: Option<Bytes>,
        body_replayability: BodyReplayability,
        cache_state: FinalizedCacheState,
        fragment: Option<String>,
    ) -> Option<Self>
    where
        B: Body,
    {
        let body = match body_replayability {
            BodyReplayability::Empty
                if request.body().is_end_stream()
                    && request.body().size_hint().exact() == Some(0) =>
            {
                Bytes::new()
            }
            BodyReplayability::Replayable => replay_body?,
            BodyReplayability::Empty | BodyReplayability::OneShot => return None,
        };
        let body_replayability = if body.is_empty() {
            BodyReplayability::Empty
        } else {
            BodyReplayability::Replayable
        };

        Some(Self {
            effective_uri: effective_uri.clone(),
            head: ReplayableRequestHead::capture(request),
            body,
            body_replayability,
            cache_state,
            fragment,
            digest_challenge: None,
            applied_cookie_header: request
                .extensions()
                .get::<AppliedCookieHeader>()
                .map(|applied| applied.0.clone()),
        })
    }

    pub(crate) fn effective_uri(&self) -> &Uri {
        &self.effective_uri
    }

    pub(crate) fn method(&self) -> &Method {
        self.head.method()
    }

    pub(crate) fn request_uri(&self) -> &Uri {
        self.head.uri()
    }

    pub(crate) fn headers(&self) -> &HeaderMap {
        self.head.headers()
    }

    pub(crate) fn body_replayability(&self) -> BodyReplayability {
        self.body_replayability
    }

    pub(crate) fn body_bytes(&self) -> Bytes {
        self.body.clone()
    }

    pub(crate) fn cache_state(&self) -> &FinalizedCacheState {
        &self.cache_state
    }

    pub(crate) fn fragment(&self) -> Option<&str> {
        self.fragment.as_deref()
    }

    pub(crate) fn applied_cookie_header(&self) -> Option<&HeaderValue> {
        self.applied_cookie_header.as_ref()
    }

    pub(crate) fn stale_replay_bytes(&self) -> Option<Bytes> {
        (self.body_replayability == BodyReplayability::Replayable).then(|| self.body.clone())
    }

    pub(crate) fn with_digest_authorization(
        &self,
        value: HeaderValue,
        challenge: crate::digest_auth::PreparedDigestChallenge,
    ) -> Self {
        let mut snapshot = self.clone();
        snapshot
            .head
            .headers_mut()
            .insert(AUTHORIZATION, value.clone());
        snapshot
            .cache_state
            .request_headers
            .insert(AUTHORIZATION, value);
        snapshot.digest_challenge = Some(challenge);
        snapshot
    }

    pub(crate) fn with_request_head_from<B>(&self, request: &http::Request<B>) -> Self
    where
        B: http_body::Body,
    {
        let mut snapshot = self.clone();
        snapshot.head = ReplayableRequestHead::capture(request);
        snapshot
    }

    /// Refresh credentials only for a configured replay. Transport recovery
    /// retains the exact serialized authorization because the prior attempt is
    /// known or assumed not to have consumed its nonce count.
    pub(crate) fn refresh_digest_authorization(&mut self, digest: &crate::digest_auth::DigestAuth) {
        let Some(challenge) = self.digest_challenge.as_ref() else {
            return;
        };
        let Some(value) = digest.authorize_prepared(self.method(), self.request_uri(), challenge)
        else {
            return;
        };
        self.head.headers_mut().insert(AUTHORIZATION, value.clone());
        self.cache_state
            .request_headers
            .insert(AUTHORIZATION, value);
    }

    pub(crate) fn to_request<B>(&self, body: B) -> http::Request<B> {
        self.head.clone().into_request(body)
    }
}

pub(crate) struct FinalizedRequestState {
    method: Method,
    body: BodyReplayability,
    snapshot: Option<FinalizedRequestSnapshot>,
    pending_replay: Option<FinalizedRequestSnapshot>,
    retry_attempt: u32,
    pending_retry_eligibility: bool,
    pending_retry_reservation: Option<u32>,
    max_retries: u32,
    retry_budget: Option<crate::retry::RetryBudget>,
    retry_budget_denied: bool,
}

impl FinalizedRequestState {
    pub(crate) fn new(
        method: Method,
        body: BodyReplayability,
        max_retries: u32,
        retry_budget: Option<crate::retry::RetryBudget>,
    ) -> Self {
        Self {
            method,
            body,
            snapshot: None,
            pending_replay: None,
            retry_attempt: 0,
            pending_retry_eligibility: false,
            pending_retry_reservation: None,
            max_retries,
            retry_budget,
            retry_budget_denied: false,
        }
    }

    pub(crate) fn record_finalized(
        &mut self,
        method: Method,
        body: BodyReplayability,
        snapshot: Option<FinalizedRequestSnapshot>,
    ) {
        self.method = method;
        self.body = body;
        self.snapshot = snapshot;
    }

    pub(crate) fn method(&self) -> &Method {
        &self.method
    }

    pub(crate) fn effective_uri(&self) -> Option<&Uri> {
        self.snapshot
            .as_ref()
            .map(FinalizedRequestSnapshot::effective_uri)
    }

    pub(crate) fn body(&self) -> BodyReplayability {
        self.snapshot
            .as_ref()
            .map(FinalizedRequestSnapshot::body_replayability)
            .unwrap_or(self.body)
    }

    pub(crate) fn has_replay_snapshot(&self) -> bool {
        self.snapshot.is_some()
    }

    pub(crate) fn clear_replay_snapshot(&mut self) {
        self.snapshot = None;
    }

    pub(crate) fn take_pending_replay(&mut self) -> Option<FinalizedRequestSnapshot> {
        let replay = self.pending_replay.take();
        if replay.is_some() {
            self.commit_retry_reservation();
        }
        replay
    }

    /// The zero-based wire-attempt index that has most recently started.
    pub(crate) fn retry_attempt(&self) -> u32 {
        self.retry_attempt
    }

    /// Reserve one configured retry before dispatching it.
    ///
    /// Digest authentication and the outer retry loop share this state so all
    /// extra wire attempts honor the same count and token budget.
    pub(crate) fn try_start_retry(&mut self) -> Option<u32> {
        self.retry_budget_denied = false;
        debug_assert!(
            !self.pending_retry_eligibility,
            "retry eligibility must be committed or released before reserving an attempt"
        );
        debug_assert!(
            self.pending_retry_reservation.is_none(),
            "a retry reservation must be committed or cancelled before reserving another"
        );
        if self.pending_retry_eligibility || self.pending_retry_reservation.is_some() {
            return None;
        }
        if self.retry_attempt >= self.max_retries {
            return None;
        }
        if let Some(budget) = &self.retry_budget
            && !budget.try_withdraw()
        {
            self.retry_budget_denied = true;
            return None;
        }

        self.retry_attempt += 1;
        self.pending_retry_reservation = Some(self.retry_attempt);
        Some(self.retry_attempt)
    }

    /// Reserve a configured retry and queue the exact finalized request for
    /// the next execute call. The retained builder is deliberately ignored.
    pub(crate) fn try_start_configured_retry(&mut self) -> Option<u32> {
        let snapshot = self.snapshot.as_ref()?;
        let snapshot = snapshot.clone();
        let attempt = self.try_start_retry()?;
        self.pending_replay = Some(snapshot);
        Some(attempt)
    }

    pub(crate) fn retry_budget_denied(&self) -> bool {
        self.retry_budget_denied
    }

    pub(crate) fn commit_retry_reservation(&mut self) {
        self.pending_retry_reservation = None;
    }

    #[cfg(test)]
    pub(crate) fn cancel_retry_reservation(&mut self) {
        let Some(reserved_attempt) = self.pending_retry_reservation.take() else {
            return;
        };
        debug_assert_eq!(reserved_attempt, self.retry_attempt);
        self.retry_attempt -= 1;
        if let Some(budget) = &self.retry_budget {
            budget.refund();
        }
        self.retry_budget_denied = false;
    }
}

/// Holds retry-budget eligibility while a response is drained for replay.
///
/// The logical attempt and observer-visible retry state are committed only
/// after draining succeeds. Dropping an uncommitted permit restores the budget.
pub(crate) struct RetryEligibilityPermit<'a> {
    state: &'a std::sync::Mutex<FinalizedRequestState>,
    committed: bool,
}

impl<'a> RetryEligibilityPermit<'a> {
    pub(crate) fn try_new(state: &'a std::sync::Mutex<FinalizedRequestState>) -> Option<Self> {
        let mut finalized = state.lock().unwrap_or_else(|error| error.into_inner());
        finalized.retry_budget_denied = false;
        if finalized.pending_retry_eligibility
            || finalized.pending_retry_reservation.is_some()
            || finalized.retry_attempt >= finalized.max_retries
        {
            return None;
        }
        if let Some(budget) = &finalized.retry_budget
            && !budget.try_withdraw()
        {
            finalized.retry_budget_denied = true;
            return None;
        }
        finalized.pending_retry_eligibility = true;
        Some(Self {
            state,
            committed: false,
        })
    }

    pub(crate) fn commit(mut self) -> (u32, u32) {
        let mut finalized = self.state.lock().unwrap_or_else(|error| error.into_inner());
        debug_assert!(finalized.pending_retry_eligibility);
        debug_assert!(finalized.pending_retry_reservation.is_none());
        finalized.pending_retry_eligibility = false;
        finalized.retry_attempt += 1;
        let attempt = finalized.retry_attempt;
        let max_retries = finalized.max_retries;
        finalized.pending_retry_reservation = Some(attempt);
        self.committed = true;
        (attempt, max_retries)
    }
}

impl Drop for RetryEligibilityPermit<'_> {
    fn drop(&mut self) {
        if self.committed {
            return;
        }
        let mut finalized = self.state.lock().unwrap_or_else(|error| error.into_inner());
        if finalized.pending_retry_eligibility {
            finalized.pending_retry_eligibility = false;
            if let Some(budget) = &finalized.retry_budget {
                budget.refund();
            }
            finalized.retry_budget_denied = false;
        }
    }
}

impl Drop for FinalizedRequestState {
    fn drop(&mut self) {
        if self.pending_retry_eligibility
            && let Some(budget) = &self.retry_budget
        {
            budget.refund();
        }
        if self.pending_retry_reservation.take().is_some()
            && let Some(budget) = &self.retry_budget
        {
            budget.refund();
        }
    }
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

    pub(crate) fn can_start_on_pooled_connection(
        self,
        supports_unsent_request_recovery: bool,
    ) -> bool {
        self != Self::OneShot || supports_unsent_request_recovery
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
    /// HTTP/3 instructed the client to use an earlier HTTP version. Normal
    /// method and body replay safety still applies.
    #[cfg(all(feature = "http3", feature = "rustls"))]
    VersionFallback,
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

/// Single-use budget for transport-triggered request replay.
///
/// Dispatch has several pooled and fresh-connection branches. Claiming one
/// shared budget keeps all of them subject to the same one-replay invariant.
#[derive(Debug, Default)]
pub(crate) struct StaleReplayBudget {
    claimed: bool,
}

impl StaleReplayBudget {
    pub(crate) fn claim(&mut self, policy: RequestReplayPolicy, reason: ReplayReason) -> bool {
        if self.claimed || !policy.permits(reason) {
            return false;
        }
        self.claimed = true;
        true
    }
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
            #[cfg(all(feature = "http3", feature = "rustls"))]
            ReplayReason::VersionFallback => self.method_is_idempotent,
            ReplayReason::ExactRequestRecovered => true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pool::ProtocolHint;
    use http::Version;

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
    fn configured_retry_queues_the_exact_finalized_snapshot() {
        let effective_uri: Uri = "https://example.test/final?x=1".parse().unwrap();
        let mut request = http::Request::builder()
            .method(Method::POST)
            .uri("/final?x=1")
            .version(Version::HTTP_2)
            .header("x-finalized", "yes")
            .body(http_body_util::Full::new(Bytes::new()))
            .unwrap();
        request.extensions_mut().insert(42_u32);
        request.extensions_mut().insert(ProtocolHint::H2c);
        let snapshot = FinalizedRequestSnapshot::capture(
            &request,
            &effective_uri,
            None,
            BodyReplayability::Empty,
            FinalizedCacheState::new(None, None, HeaderMap::new()),
            None,
        )
        .unwrap();
        let mut state = FinalizedRequestState::new(Method::GET, BodyReplayability::Empty, 1, None);

        assert!(!state.has_replay_snapshot());
        state.record_finalized(Method::POST, BodyReplayability::Empty, Some(snapshot));
        assert_eq!(state.try_start_configured_retry(), Some(1));
        let replay = state.take_pending_replay().unwrap();
        let replayed = replay.to_request(());

        assert_eq!(state.method(), Method::POST);
        assert_eq!(state.body(), BodyReplayability::Empty);
        assert_eq!(replay.effective_uri(), &effective_uri);
        assert_eq!(replayed.method(), Method::POST);
        assert_eq!(replayed.uri(), "/final?x=1");
        assert_eq!(replayed.version(), Version::HTTP_2);
        assert_eq!(replayed.headers()["x-finalized"], "yes");
        assert!(replayed.extensions().get::<u32>().is_none());
        assert_eq!(
            replayed.extensions().get::<ProtocolHint>(),
            Some(&ProtocolHint::H2c)
        );
        assert!(state.take_pending_replay().is_none());
    }

    #[test]
    fn one_shot_request_has_no_finalized_snapshot() {
        let request = http::Request::new(http_body_util::Full::new(Bytes::from_static(b"body")));
        let snapshot = FinalizedRequestSnapshot::capture(
            &request,
            &Uri::from_static("http://example.test/"),
            None,
            BodyReplayability::OneShot,
            FinalizedCacheState::new(None, None, HeaderMap::new()),
            None,
        );

        assert!(snapshot.is_none());
    }

    #[test]
    fn finalized_state_shares_retry_count_and_token_budget() {
        let budget = crate::retry::RetryBudget::new(2, 0);
        let mut state = FinalizedRequestState::new(
            Method::GET,
            BodyReplayability::Empty,
            2,
            Some(budget.clone()),
        );

        assert_eq!(state.try_start_retry(), Some(1));
        state.commit_retry_reservation();
        assert_eq!(state.try_start_retry(), Some(2));
        state.commit_retry_reservation();
        assert_eq!(state.try_start_retry(), None);
        assert_eq!(state.retry_attempt(), 2);
        assert_eq!(budget.available(), 0);
        assert!(!state.retry_budget_denied());
    }

    #[test]
    fn finalized_state_does_not_advance_when_retry_budget_is_empty() {
        let budget = crate::retry::RetryBudget::new(0, 0);
        let mut state =
            FinalizedRequestState::new(Method::GET, BodyReplayability::Empty, 2, Some(budget));

        assert_eq!(state.try_start_retry(), None);
        assert_eq!(state.retry_attempt(), 0);
        assert!(state.retry_budget_denied());
    }

    #[test]
    fn cancelling_an_undispatched_retry_refunds_exactly_one_budget_token() {
        let budget = crate::retry::RetryBudget::new(1, 0);
        let mut state = FinalizedRequestState::new(
            Method::GET,
            BodyReplayability::Empty,
            1,
            Some(budget.clone()),
        );

        assert_eq!(state.try_start_retry(), Some(1));
        assert_eq!(budget.available(), 0);
        state.cancel_retry_reservation();

        assert_eq!(state.retry_attempt(), 0);
        assert_eq!(budget.available(), 1);
    }

    #[test]
    fn cancelling_a_dispatched_retry_does_not_refund_or_rewind_it() {
        let budget = crate::retry::RetryBudget::new(1, 0);
        let mut state = FinalizedRequestState::new(
            Method::GET,
            BodyReplayability::Empty,
            1,
            Some(budget.clone()),
        );

        assert_eq!(state.try_start_retry(), Some(1));
        state.commit_retry_reservation();
        state.cancel_retry_reservation();

        assert_eq!(state.retry_attempt(), 1);
        assert_eq!(budget.available(), 0);
    }

    #[test]
    fn dropping_an_undispatched_retry_refunds_its_budget_token() {
        let budget = crate::retry::RetryBudget::new(1, 0);
        {
            let mut state = FinalizedRequestState::new(
                Method::GET,
                BodyReplayability::Empty,
                1,
                Some(budget.clone()),
            );
            assert_eq!(state.try_start_retry(), Some(1));
            assert_eq!(budget.available(), 0);
        }

        assert_eq!(budget.available(), 1);
    }

    #[test]
    fn dropping_retry_eligibility_does_not_advance_or_spend_the_retry() {
        let budget = crate::retry::RetryBudget::new(1, 0);
        let state = std::sync::Mutex::new(FinalizedRequestState::new(
            Method::GET,
            BodyReplayability::Empty,
            1,
            Some(budget.clone()),
        ));

        let permit = RetryEligibilityPermit::try_new(&state).unwrap();
        assert_eq!(budget.available(), 0);
        drop(permit);

        let state = state.lock().unwrap();
        assert_eq!(state.retry_attempt(), 0);
        assert_eq!(budget.available(), 1);
        assert!(!state.retry_budget_denied());
    }

    #[test]
    fn committing_retry_eligibility_creates_the_retry_reservation() {
        let budget = crate::retry::RetryBudget::new(1, 0);
        let state = std::sync::Mutex::new(FinalizedRequestState::new(
            Method::GET,
            BodyReplayability::Empty,
            2,
            Some(budget.clone()),
        ));

        let permit = RetryEligibilityPermit::try_new(&state).unwrap();
        assert_eq!(permit.commit(), (1, 2));

        let mut state = state.lock().unwrap();
        assert_eq!(state.retry_attempt(), 1);
        assert_eq!(budget.available(), 0);
        state.commit_retry_reservation();
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

    #[cfg(all(feature = "http3", feature = "rustls"))]
    #[test]
    fn version_fallback_requires_idempotent_reproducible_request() {
        assert!(
            RequestReplayPolicy::new(&Method::GET, BodyReplayability::Empty)
                .permits(ReplayReason::VersionFallback)
        );
        assert!(
            RequestReplayPolicy::new(&Method::PUT, BodyReplayability::Replayable)
                .permits(ReplayReason::VersionFallback)
        );
        assert!(
            !RequestReplayPolicy::new(&Method::POST, BodyReplayability::Replayable)
                .permits(ReplayReason::VersionFallback)
        );
        assert!(
            !RequestReplayPolicy::new(&Method::GET, BodyReplayability::OneShot)
                .permits(ReplayReason::VersionFallback)
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
    fn stale_replay_budget_allows_only_one_transport_replay() {
        let policy = RequestReplayPolicy::new(&Method::POST, BodyReplayability::Replayable);
        let mut budget = StaleReplayBudget::default();

        assert!(budget.claim(policy, ReplayReason::ProvenUnprocessed));
        assert!(!budget.claim(policy, ReplayReason::ProvenUnprocessed));
    }

    #[test]
    fn stale_replay_budget_does_not_claim_for_an_unsafe_request() {
        let policy = RequestReplayPolicy::new(&Method::POST, BodyReplayability::OneShot);
        let mut budget = StaleReplayBudget::default();

        assert!(!budget.claim(policy, ReplayReason::ProvenUnprocessed));
        assert!(!budget.claim(policy, ReplayReason::AmbiguousTransportFailure));
        assert!(budget.claim(
            RequestReplayPolicy::new(&Method::POST, BodyReplayability::Replayable),
            ReplayReason::ProvenUnprocessed,
        ));
    }

    #[test]
    fn one_shot_request_keeps_an_already_acquired_fresh_connection() {
        assert!(BodyReplayability::Empty.can_replace_fresh_connection());
        assert!(BodyReplayability::Replayable.can_replace_fresh_connection());
        assert!(!BodyReplayability::OneShot.can_replace_fresh_connection());
    }

    #[test]
    fn one_shot_pool_reuse_requires_exact_recovery_support() {
        assert!(BodyReplayability::Empty.can_start_on_pooled_connection(false));
        assert!(BodyReplayability::Replayable.can_start_on_pooled_connection(false));
        assert!(BodyReplayability::OneShot.can_start_on_pooled_connection(true));
        assert!(!BodyReplayability::OneShot.can_start_on_pooled_connection(false));
    }
}
