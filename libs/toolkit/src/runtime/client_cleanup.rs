use std::{fmt, future::Future, pin::Pin};

use tokio_util::sync::CancellationToken;

/// The completion operation owned by [`ClientCleanup`].
pub type ClientCleanupFuture = Pin<Box<dyn Future<Output = Result<(), ClientCleanupError>> + Send>>;

/// A failure during client cleanup.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum ClientCleanupError {
    /// A client worker fails during cleanup.
    #[error("client cleanup worker failed")]
    WorkerFailed,
    /// The completion operation reports a deadline, or the `OoP` bootstrap times out while waiting
    /// for cleanup. When the bootstrap times out, it requests cancellation. Workers may still be
    /// running when the call returns.
    #[error("client cleanup deadline exceeded")]
    DeadlineExceeded,
}

/// Owns one asynchronous client completion operation.
///
/// Await [`finish`](Self::finish) to observe completion. Dropping an unfinished
/// owner requests cancellation through its abort token; it does not join workers.
pub struct ClientCleanup {
    completion: ClientCleanupFuture,
    abort: CancellationToken,
    armed: bool,
}

impl ClientCleanup {
    /// Stores the completion operation without polling it or spawning work.
    #[must_use]
    pub fn new(completion: ClientCleanupFuture, abort: CancellationToken) -> Self {
        Self {
            completion,
            abort,
            armed: true,
        }
    }

    /// Creates an owner whose completion succeeds immediately.
    #[must_use]
    pub fn empty() -> Self {
        Self::new(Box::pin(async { Ok(()) }), CancellationToken::new())
    }

    /// Drives the supplied operation to completion, then disarms abort cancellation.
    ///
    /// # Cancellation safety
    ///
    /// Dropping this future before completion, even before its first poll, cancels
    /// the abort token and drops the completion operation. Cancellation requests
    /// termination; it does not prove that workers have joined.
    ///
    /// # Errors
    ///
    /// Returns the supplied completion error unchanged. An error also disarms abort.
    pub async fn finish(mut self) -> Result<(), ClientCleanupError> {
        let result = self.completion.as_mut().await;
        self.armed = false;
        result
    }
}

impl fmt::Debug for ClientCleanup {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClientCleanup")
            .field("armed", &self.armed)
            .finish_non_exhaustive()
    }
}

impl Drop for ClientCleanup {
    fn drop(&mut self) {
        if self.armed {
            self.abort.cancel();
        }
    }
}
