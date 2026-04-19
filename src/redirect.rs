use http::{Method, StatusCode, Uri};

use std::fmt;
use std::sync::Arc;

type RedirectFn = dyn Fn(&Uri, &Uri, StatusCode, &Method) -> RedirectAction + Send + Sync;

/// Controls how the client handles HTTP redirects.
#[derive(Clone)]
pub enum RedirectPolicy {
    /// Never follow redirects.
    None,
    /// Follow up to N redirects.
    Limited(usize),
    /// Use a custom redirect decision function.
    Custom(Arc<RedirectFn>),
}

/// Decision returned by a redirect policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RedirectAction {
    /// Follow the redirect.
    Follow,
    /// Stop and return the redirect response.
    Stop,
}

impl fmt::Debug for RedirectPolicy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::None => write!(f, "RedirectPolicy::None"),
            Self::Limited(n) => write!(f, "RedirectPolicy::Limited({n})"),
            Self::Custom(_) => write!(f, "RedirectPolicy::Custom(...)"),
        }
    }
}

impl Default for RedirectPolicy {
    fn default() -> Self {
        Self::Limited(10)
    }
}

impl RedirectPolicy {
    /// Create a policy that never follows redirects.
    pub fn none() -> Self {
        Self::None
    }

    /// Create a policy that follows up to `max` redirects.
    pub fn limited(max: usize) -> Self {
        Self::Limited(max)
    }

    /// Create a policy using a custom decision function.
    pub fn custom<F>(f: F) -> Self
    where
        F: Fn(&Uri, &Uri, StatusCode, &Method) -> RedirectAction + Send + Sync + 'static,
    {
        Self::Custom(Arc::new(f))
    }

    pub(crate) fn max_redirects(&self) -> usize {
        match self {
            Self::None => 0,
            Self::Limited(n) => *n,
            Self::Custom(_) => usize::MAX,
        }
    }

    pub(crate) fn check(
        &self,
        current: &Uri,
        next: &Uri,
        status: StatusCode,
        method: &Method,
    ) -> RedirectAction {
        match self {
            Self::None => RedirectAction::Stop,
            Self::Limited(_) => RedirectAction::Follow,
            Self::Custom(f) => f(current, next, status, method),
        }
    }
}
