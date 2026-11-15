//! Clock and Sleeper ports.
//!
//! Time is injected so retry, rate-limit and progress logic is deterministic under test.
//! Instants are [`tokio::time::Instant`], so tests drive everything with tokio's paused time.

use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use thiserror::Error;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

/// A boxed, sendable future: keeps [`Sleeper`] usable as a trait object.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Returned when a wait was ended by a cancelled [`CancellationToken`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("operation cancelled")]
pub struct Cancelled;

/// Source of monotonic time.
pub trait Clock: Send + Sync + 'static {
    /// The current monotonic instant.
    fn now(&self) -> Instant;
}

/// Suspends the current task for a duration.
pub trait Sleeper: Send + Sync + 'static {
    /// Completes after `duration` has elapsed on this sleeper's time base.
    fn sleep(&self, duration: Duration) -> BoxFuture<'_, ()>;
}

/// The production [`Clock`]: tokio's clock (honours `tokio::time::pause`).
#[derive(Debug, Clone, Copy, Default)]
pub struct TokioClock;

impl Clock for TokioClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
}

/// The production [`Sleeper`]: `tokio::time::sleep`.
#[derive(Debug, Clone, Copy, Default)]
pub struct TokioSleeper;

impl Sleeper for TokioSleeper {
    fn sleep(&self, duration: Duration) -> BoxFuture<'_, ()> {
        Box::pin(tokio::time::sleep(duration))
    }
}

/// Sleeps for `duration` unless `cancel` fires first.
///
/// # Errors
///
/// Returns [`Cancelled`] when the token is cancelled before or during the wait.
pub async fn sleep_or_cancel(
    sleeper: &dyn Sleeper,
    duration: Duration,
    cancel: &CancellationToken,
) -> Result<(), Cancelled> {
    if cancel.is_cancelled() {
        return Err(Cancelled);
    }
    tokio::select! {
        biased;
        () = cancel.cancelled() => Err(Cancelled),
        () = sleeper.sleep(duration) => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn sleep_or_cancel_completes_and_cancels() {
        let cancel = CancellationToken::new();
        let start = TokioClock.now();
        sleep_or_cancel(&TokioSleeper, Duration::from_secs(5), &cancel)
            .await
            .unwrap();
        assert_eq!(start.elapsed(), Duration::from_secs(5));

        let child = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(1)).await;
            child.cancel();
        });
        let before = TokioClock.now();
        let result = sleep_or_cancel(&TokioSleeper, Duration::from_secs(60), &cancel).await;
        assert_eq!(result, Err(Cancelled));
        assert_eq!(before.elapsed(), Duration::from_secs(1));
        // Already cancelled: no wait at all.
        let result = sleep_or_cancel(&TokioSleeper, Duration::from_secs(60), &cancel).await;
        assert_eq!(result, Err(Cancelled));
    }
}
