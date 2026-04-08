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
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    #[tokio::test]
    async fn periodic_notification() {
        // Use a very short interval so the poker fires quickly.
        // Wait for the first poke via notified() — no timeout needed.
        let cancel = CancellationToken::new();
        let (notify, handle) = poker(Duration::from_millis(1), cancel.clone());

        // Block until at least one poke arrives
        notify.notified().await;

        cancel.cancel();
        drop(handle.await);
    }

    #[tokio::test]
    async fn cancellation_stops_poker() {
        // Prove: after cancel + join, no more pokes arrive.
        // We use a counter to detect pokes instead of timeouts.
        let cancel = CancellationToken::new();
        let (notify, handle) = poker(Duration::from_millis(1), cancel.clone());

        // Wait for at least one poke
        notify.notified().await;

        // Cancel and join — poker task is fully stopped
        cancel.cancel();
        drop(handle.await);

        // Spawn a watcher that counts any further pokes.
        // If the poker is truly stopped, the counter stays at 0.
        let counter = Arc::new(AtomicU32::new(0));
        let counter_c = Arc::clone(&counter);
        let notify_c = Arc::clone(&notify);
        let watcher_cancel = CancellationToken::new();
        let watcher_cancel_c = watcher_cancel.clone();
        let watcher = tokio::spawn(async move {
            loop {
                tokio::select! {
                    () = watcher_cancel_c.cancelled() => break,
                    () = notify_c.notified() => {
                        counter_c.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }
        });

        // Give the watcher a chance to observe any spurious pokes
        tokio::task::yield_now().await;
        tokio::task::yield_now().await;

        watcher_cancel.cancel();
        drop(watcher.await);

        assert_eq!(
            counter.load(Ordering::Relaxed),
            0,
            "no pokes should arrive after cancel + join"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn stored_permit_semantics() {
        // Prove: a poke that fires while nobody is awaiting creates a
        // stored permit that the next notified() consumes immediately.
        let cancel = CancellationToken::new();
        let (notify, handle) = poker(Duration::from_millis(1), cancel.clone());

        // Yield to let the poker fire at least once (stores a permit)
        tokio::task::yield_now().await;
        tokio::task::yield_now().await;

        // This should resolve immediately from the stored permit
        notify.notified().await;

        cancel.cancel();
        drop(handle.await);
    }
}
