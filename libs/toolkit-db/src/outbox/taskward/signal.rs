//! A wakeup that carries what it is about.
//!
//! [`poker`](super::poker) turns a timer into a wakeup source; this turns
//! "work concerning X" into one. Both hand a worker the same `Arc<Notify>` it
//! registers as a notifier, so a task fed by either looks the same inside.
//!
//! It exists because the alternative to carrying a subject is rediscovery: a
//! bare `Notify` says only that something happened and leaves the receiver to
//! scan for what. Carrying the subject lets the receiver do exactly the work
//! it was told about.
//!
//! The subject is the caller's own type; this module knows only that subjects
//! can be compared and hashed. Two rules pair it with
//! [`Directive::Idle`](super::Directive::Idle):
//!
//! - A subject already pending is one piece of work, not two, so producers
//!   racing each other do not multiply the receiver's wakeups.
//! - A wakeup is emitted only when a subject was actually added, so a worker
//!   that returns `Idle` on an empty take is never woken to find nothing.
//!
//! The signalling is conditional by nature - a task with nothing to report
//! signals nobody, so a worker that was not told does not run. That is what
//! makes many small tasks cheaper than a few large ones rather than dearer.
//!
//! This is the many-to-one shape. Where the receiver's identity *is* the
//! subject - one long-lived task per subject - give each receiver its own
//! `Notify` and route to it, rather than sharing one set every receiver has to
//! filter.

use std::collections::HashSet;
use std::hash::Hash;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use tokio::sync::Notify;

/// Subjects a downstream worker should look at, and a nudge when one is added.
#[derive(Debug)]
pub struct Signal<T> {
    pending: Mutex<HashSet<T>>,
    notify: Arc<Notify>,
}

impl<T: Eq + Hash> Default for Signal<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T: Eq + Hash> Signal<T> {
    /// An empty signal.
    ///
    /// Returns `Self`, not `Arc<Self>`: sharing is the caller's decision, and
    /// a constructor that allocates for them rules out embedding it by value
    /// or in a `static`.
    #[must_use]
    pub fn new() -> Self {
        Self {
            pending: Mutex::new(HashSet::new()),
            notify: Arc::new(Notify::new()),
        }
    }

    /// Tell the receiver about a subject. Deduplicated: signalling the same
    /// subject twice before it is taken is one piece of work, not two, and
    /// earns one wakeup rather than two.
    pub fn signal(&self, subject: T) {
        if self.pending().insert(subject) {
            self.notify.notify_one();
        }
    }

    /// Take everything signalled so far, in no particular order.
    pub fn take(&self) -> Vec<T> {
        self.pending().drain().collect()
    }

    /// The handle a `taskward` worker registers as its notifier.
    #[must_use]
    pub fn notifier(&self) -> Arc<Notify> {
        Arc::clone(&self.notify)
    }

    /// A poisoned lock is recovered rather than propagated: the guarded set is
    /// still structurally sound, and a channel that silently swallows every
    /// subject from then on is a worse failure than one that keeps going.
    fn pending(&self) -> MutexGuard<'_, HashSet<T>> {
        self.pending.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use std::sync::atomic::{AtomicU32, Ordering};

    use super::*;

    #[test]
    fn a_signal_carries_the_subject_it_is_about() {
        let signal = Signal::new();
        assert!(signal.take().is_empty());

        signal.signal(7);
        signal.signal(9);

        let mut taken = signal.take();
        taken.sort_unstable();
        assert_eq!(taken, vec![7, 9]);
        assert!(signal.take().is_empty(), "taking empties it");
    }

    #[test]
    fn the_same_subject_twice_is_one_piece_of_work() {
        let signal = Signal::new();
        signal.signal(3);
        signal.signal(3);
        assert_eq!(signal.take(), vec![3]);
    }

    #[test]
    fn nothing_signalled_means_nothing_to_do() {
        let signal = Signal::<i64>::new();
        assert!(signal.take().is_empty());
    }

    #[test]
    fn a_subject_is_whatever_the_caller_says_it_is() {
        let signal = Signal::new();
        signal.signal("orders");
        signal.signal("orders");
        signal.signal("invoices");

        let mut taken = signal.take();
        taken.sort_unstable();
        assert_eq!(taken, vec!["invoices", "orders"]);
    }

    #[tokio::test]
    async fn a_subject_signalled_while_the_receiver_works_is_not_lost() {
        // The receiver takes, works, and only then waits again. A subject that
        // arrives inside that window has to be waiting for it when it does.
        let signal = Signal::new();
        let notifier = signal.notifier();

        signal.signal(1);
        notifier.notified().await;
        assert_eq!(signal.take(), vec![1]);

        signal.signal(2);
        notifier.notified().await;
        assert_eq!(signal.take(), vec![2]);
    }

    #[tokio::test]
    async fn a_duplicate_subject_does_not_earn_a_second_wakeup() {
        // The rule that lets a worker return Idle on an empty take: a wakeup is
        // emitted only when a subject was actually added. Counted rather than
        // timed, because a stored permit is indistinguishable from a delivered
        // one until somebody is actually waiting.
        let signal = Signal::new();
        let notifier = signal.notifier();
        let wakeups = Arc::new(AtomicU32::new(0));

        let counted = Arc::clone(&wakeups);
        let watcher = tokio::spawn(async move {
            loop {
                notifier.notified().await;
                counted.fetch_add(1, Ordering::Relaxed);
            }
        });
        tokio::task::yield_now().await; // the watcher is now waiting

        signal.signal(3);
        signal.signal(3);
        for _ in 0..4 {
            tokio::task::yield_now().await;
        }
        watcher.abort();

        assert_eq!(
            wakeups.load(Ordering::Relaxed),
            1,
            "the second signal must neither wake nor store a permit",
        );
    }
}
