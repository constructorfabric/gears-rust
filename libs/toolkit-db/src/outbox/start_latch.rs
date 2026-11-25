//! The start signal that [`OutboxBuilder::start`](super::OutboxBuilder::start)
//! gives its workers.

use std::sync::Arc;

use tokio::sync::Notify;

/// Gives each subscribed worker one startup wake, even if the worker starts
/// waiting after [`open`](Self::open).
///
/// `start()` subscribes every worker while it wires them, then opens the latch
/// once. Each worker waits on a `Notify` of its own, from
/// [`subscribe`](Self::subscribe), and `open` calls `notify_one` on each of
/// them. `notify_one` stores a permit when the worker is not waiting yet, so a
/// worker that the runtime polls only after `open` still makes its first pass
/// at once. `open` takes the latch by value, so no worker can subscribe after
/// it.
///
/// A shared `Notify` and `notify_waiters` cannot do this: `notify_waiters`
/// wakes only the tasks that are already waiting and stores nothing, so a
/// worker that was spawned but not yet polled slept until its next wake
/// (for a partition processor, its idle poke).
///
/// The latch is the startup wake only. A worker with other notifiers, such as
/// a sequencer woken by the prioritizer, can wake on those before `open`.
#[derive(Default)]
pub(super) struct StartLatch {
    /// The notifiers of the subscribed workers, one each.
    subscribers: Vec<Arc<Notify>>,
}

impl StartLatch {
    /// A notifier of one worker's own, for `WorkerBuilder::notifier`.
    ///
    /// It holds one permit once the latch is open, and never before. Give each
    /// worker its own subscription: two workers that share one notifier share
    /// its single permit.
    #[must_use]
    pub(super) fn subscribe(&mut self) -> Arc<Notify> {
        let notify = Arc::new(Notify::new());
        self.subscribers.push(Arc::clone(&notify));
        notify
    }

    /// Gives every subscribed worker its startup wake.
    ///
    /// A worker that is waiting wakes now; a worker that is not waiting yet
    /// finds its permit when it does.
    pub(super) fn open(self) {
        for notify in self.subscribers {
            notify.notify_one();
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use std::future::Future;
    use std::pin::pin;
    use std::task::{Context, Poll, Waker};

    use super::*;

    /// Polls a future once without a runtime; `true` if it is ready.
    fn ready_now<F: Future>(fut: std::pin::Pin<&mut F>) -> bool {
        matches!(
            fut.poll(&mut Context::from_waker(Waker::noop())),
            Poll::Ready(_)
        )
    }

    #[test]
    fn no_worker_starts_before_open() {
        let mut latch = StartLatch::default();
        let notify = latch.subscribe();

        assert!(!ready_now(pin!(notify.notified())));
    }

    #[test]
    fn a_worker_that_waits_only_after_open_starts_at_once() {
        let mut latch = StartLatch::default();
        let notify = latch.subscribe();

        latch.open();

        // The worker was not waiting when the latch opened (the case that a
        // shared `notify_waiters` loses): its permit is stored.
        assert!(ready_now(pin!(notify.notified())));
    }

    #[test]
    fn a_worker_already_waiting_starts_on_open() {
        let mut latch = StartLatch::default();
        let notify = latch.subscribe();
        let mut waiting = pin!(notify.notified());
        assert!(!ready_now(waiting.as_mut()));

        latch.open();

        assert!(ready_now(waiting.as_mut()));
    }

    #[test]
    fn every_subscriber_gets_one_permit_of_its_own() {
        let mut latch = StartLatch::default();
        let subscribers: Vec<_> = (0..3).map(|_| latch.subscribe()).collect();
        let mut waiting = pin!(subscribers[0].notified());
        assert!(!ready_now(waiting.as_mut()));

        latch.open();

        assert!(ready_now(waiting.as_mut()));
        for notify in subscribers.iter().skip(1) {
            assert!(ready_now(pin!(notify.notified())));
        }
        // One permit each: a worker's first pass is all the latch asks for.
        for notify in &subscribers {
            assert!(!ready_now(pin!(notify.notified())));
        }
    }
}
