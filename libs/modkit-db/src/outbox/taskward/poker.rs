use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

/// Spawn a background task that periodically pokes a [`Notify`].
///
/// Returns `(Arc<Notify>, JoinHandle<()>)`. The spawned task calls
/// `notify_one()` every `interval` and exits when `cancel` is cancelled.
#[must_use]
pub fn poker(
    interval: Duration,
    cancel: CancellationToken,
) -> (Arc<Notify>, tokio::task::JoinHandle<()>) {
    let notify = Arc::new(Notify::new());
    let n = notify.clone();
    let handle = tokio::spawn(async move {
        loop {
            tokio::select! {
                () = cancel.cancelled() => break,
                () = tokio::time::sleep(interval) => { n.notify_one(); }
            }
        }
    });
    (notify, handle)
}

#[cfg(test)]
#[path = "poker_tests.rs"]
mod tests;
