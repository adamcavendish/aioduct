use std::future::Future;
use std::time::{Duration, Instant};

use crate::error::Error;
use crate::runtime::RuntimeCompletion;

/// One absolute budget for a fresh connection acquisition.
///
/// Each phase receives only the time left from the original budget. A new
/// value is created only when dispatch starts a distinct fresh acquisition.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ConnectionDeadline {
    started: Instant,
    timeout: Option<Duration>,
}

impl ConnectionDeadline {
    pub(crate) fn new(timeout: Option<Duration>) -> Self {
        Self {
            started: Instant::now(),
            timeout,
        }
    }

    pub(crate) fn remaining(self) -> Result<Option<Duration>, Error> {
        let Some(timeout) = self.timeout else {
            return Ok(None);
        };
        let elapsed = self.started.elapsed();
        if elapsed >= timeout {
            return Err(Error::ConnectTimeout);
        }
        Ok(Some(timeout - elapsed))
    }

    pub(crate) fn check(self) -> Result<(), Error> {
        self.remaining().map(|_| ())
    }

    pub(crate) async fn run<R, F, T>(self, future: F) -> Result<T, Error>
    where
        R: RuntimeCompletion,
        F: Future<Output = Result<T, Error>>,
    {
        crate::timeout::connect_timeout::<R, _, _>(future, self.remaining()?).await
    }

    pub(crate) async fn sleep<R>(self, duration: Duration) -> Result<(), Error>
    where
        R: RuntimeCompletion,
    {
        self.run::<R, _, _>(async move {
            R::sleep(duration).await;
            Ok(())
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deadline_without_timeout_has_no_remaining_limit() {
        let deadline = ConnectionDeadline::new(None);

        assert_eq!(deadline.remaining().unwrap(), None);
        assert!(deadline.check().is_ok());
    }

    #[test]
    fn deadline_reports_a_decreasing_remaining_budget() {
        let deadline = ConnectionDeadline::new(Some(Duration::from_millis(100)));
        let first = deadline.remaining().unwrap().unwrap();
        std::thread::sleep(Duration::from_millis(5));
        let second = deadline.remaining().unwrap().unwrap();

        assert!(second < first);
    }

    #[test]
    fn expired_deadline_is_a_connect_timeout() {
        let deadline = ConnectionDeadline::new(Some(Duration::from_millis(1)));
        std::thread::sleep(Duration::from_millis(5));

        assert!(matches!(deadline.check(), Err(Error::ConnectTimeout)));
    }

    #[cfg(feature = "tokio")]
    #[tokio::test]
    async fn sequential_phases_share_one_budget() {
        use crate::runtime::TokioRuntime;

        let deadline = ConnectionDeadline::new(Some(Duration::from_millis(200)));
        deadline
            .run::<TokioRuntime, _, _>(async {
                tokio::time::sleep(Duration::from_millis(40)).await;
                Ok(())
            })
            .await
            .unwrap();

        let result = deadline
            .run::<TokioRuntime, _, ()>(std::future::pending())
            .await;

        assert!(matches!(result, Err(Error::ConnectTimeout)));
    }
}
