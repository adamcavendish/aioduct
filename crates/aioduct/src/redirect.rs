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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_limited_10() {
        match RedirectPolicy::default() {
            RedirectPolicy::Limited(n) => assert_eq!(n, 10),
            _ => panic!("expected Limited"),
        }
    }

    #[test]
    fn none_max_redirects_zero() {
        assert_eq!(RedirectPolicy::none().max_redirects(), 0);
    }

    #[test]
    fn limited_max_redirects() {
        assert_eq!(RedirectPolicy::limited(5).max_redirects(), 5);
    }

    #[test]
    fn custom_max_redirects_is_max() {
        let policy = RedirectPolicy::custom(|_, _, _, _| RedirectAction::Follow);
        assert_eq!(policy.max_redirects(), usize::MAX);
    }

    #[test]
    fn none_check_always_stops() {
        let from: Uri = "http://a.com".parse().unwrap();
        let to: Uri = "http://b.com".parse().unwrap();
        let action =
            RedirectPolicy::none().check(&from, &to, StatusCode::MOVED_PERMANENTLY, &Method::GET);
        assert_eq!(action, RedirectAction::Stop);
    }

    #[test]
    fn limited_check_always_follows() {
        let from: Uri = "http://a.com".parse().unwrap();
        let to: Uri = "http://b.com".parse().unwrap();
        let action = RedirectPolicy::limited(5).check(&from, &to, StatusCode::FOUND, &Method::GET);
        assert_eq!(action, RedirectAction::Follow);
    }

    #[test]
    fn custom_check_delegates() {
        let policy = RedirectPolicy::custom(|_, _, status, _| {
            if status == StatusCode::MOVED_PERMANENTLY {
                RedirectAction::Follow
            } else {
                RedirectAction::Stop
            }
        });
        let from: Uri = "http://a.com".parse().unwrap();
        let to: Uri = "http://b.com".parse().unwrap();
        assert_eq!(
            policy.check(&from, &to, StatusCode::MOVED_PERMANENTLY, &Method::GET),
            RedirectAction::Follow
        );
        assert_eq!(
            policy.check(&from, &to, StatusCode::FOUND, &Method::GET),
            RedirectAction::Stop
        );
    }

    #[test]
    fn debug_formatting() {
        assert_eq!(
            format!("{:?}", RedirectPolicy::None),
            "RedirectPolicy::None"
        );
        assert_eq!(
            format!("{:?}", RedirectPolicy::Limited(3)),
            "RedirectPolicy::Limited(3)"
        );
        let custom = RedirectPolicy::custom(|_, _, _, _| RedirectAction::Follow);
        assert_eq!(format!("{custom:?}"), "RedirectPolicy::Custom(...)");
    }
}
