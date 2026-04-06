use std::time::Duration;

use tokio_util::sync::CancellationToken;

/// Scheduling directive returned by worker actions, carrying an optional
/// typed payload `P`.
///
/// All workers use the same directive enum regardless of notification mode.
/// The default `P = ()` preserves backward compatibility.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Directive<P = ()> {
    /// More work available — re-execute immediately.
    Proceed(P),
    /// No work — wait for any configured notifier (or cancellation if none).
    Idle(P),
    /// Soft sleep — listen for notifiers, wake early if notified before duration elapses.
    Sleep(Duration, P),
}

impl<P> Directive<P> {
    /// Borrow the payload.
    pub fn payload(&self) -> &P {
        match self {
            Self::Proceed(p) | Self::Idle(p) | Self::Sleep(_, p) => p,
        }
    }

    /// Transform the payload.
    pub fn map<Q>(self, f: impl FnOnce(P) -> Q) -> Directive<Q> {
        match self {
            Self::Proceed(p) => Directive::Proceed(f(p)),
            Self::Idle(p) => Directive::Idle(f(p)),
            Self::Sleep(d, p) => Directive::Sleep(d, f(p)),
        }
    }

    /// Strip the payload, keeping only the scheduling signal.
    pub fn strip(&self) -> Directive<()> {
        match self {
            Self::Proceed(_) => Directive::Proceed(()),
            Self::Idle(_) => Directive::Idle(()),
            Self::Sleep(d, _) => Directive::Sleep(*d, ()),
        }
    }
}

/// Convenience constructors for the no-payload case.
impl Directive<()> {
    /// `Proceed` with no payload.
    #[must_use]
    pub fn proceed() -> Self {
        Self::Proceed(())
    }

    /// `Idle` with no payload.
    #[must_use]
    pub fn idle() -> Self {
        Self::Idle(())
    }

    /// `Sleep` with no payload.
    #[must_use]
    pub fn sleep(d: Duration) -> Self {
        Self::Sleep(d, ())
    }
}

// Directive<()> is Copy since () is Copy.
impl Copy for Directive<()> {}

/// Trait for worker action logic. The worker loop calls `execute()` repeatedly,
/// using the returned directive to decide when to call again.
///
/// # Associated Types
///
/// - `Payload` — typed data attached to the directive on success. Use `()`
///   for workers with no meaningful report data.
/// - `Error` — must be `Display + Send`. Errors are absorbed by the bulkhead
///   with escalating backoff; the worker never exits on error.
pub trait WorkerAction: Send {
    type Payload: Send + Sync + 'static;
    type Error: std::fmt::Display + Send;

    /// Execute one unit of work.
    fn execute(
        &mut self,
        cancel: &CancellationToken,
    ) -> impl std::future::Future<Output = Result<Directive<Self::Payload>, Self::Error>> + Send;
}

#[cfg(test)]
#[path = "action_tests.rs"]
mod tests;
