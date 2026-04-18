use http::{Method, StatusCode, Uri};

use std::fmt;
use std::sync::Arc;

type RedirectFn = dyn Fn(&Uri, &Uri, StatusCode, &Method) -> RedirectAction + Send + Sync;

#[derive(Clone)]
pub enum RedirectPolicy {
    None,
    Limited(usize),
    Custom(Arc<RedirectFn>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RedirectAction {
    Follow,
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
    pub fn none() -> Self {
        Self::None
    }

    pub fn limited(max: usize) -> Self {
        Self::Limited(max)
    }

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
