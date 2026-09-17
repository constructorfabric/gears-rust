//! Live interest in traces, held in this process.
//!
//! Registering interest costs no statement, and neither does releasing it: the
//! registry is the only thing that decides whether a completion is delivered
//! in-process, and it is also what tells the notifier whether it has any
//! reason to run at all. An instance holding no subscriptions issues no
//! notification query, which is the property that makes the mail poll free
//! when nobody is waiting.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};

use tokio::sync::watch;

use super::trace::{TraceOutcome, TraceState};
use super::types::InstanceId;

/// What an ack needs to deliver a completion in-process: who this instance is,
/// and who is waiting.
#[derive(Debug)]
pub struct Mailbox {
    instance_id: InstanceId,
    subscriptions: Arc<TraceRegistry>,
}

impl Mailbox {
    #[must_use]
    pub fn new(instance_id: InstanceId, subscriptions: Arc<TraceRegistry>) -> Self {
        Self {
            instance_id,
            subscriptions,
        }
    }

    #[must_use]
    pub fn instance_id(&self) -> &str {
        self.instance_id.as_str()
    }

    #[must_use]
    pub fn subscriptions(&self) -> &Arc<TraceRegistry> {
        &self.subscriptions
    }
}

/// Interest in one trace.
struct Interest {
    /// Which subscription this entry belongs to.
    ///
    /// The registry is keyed by the trace string and nothing requires a
    /// caller's traces to be unique, so two live subscriptions can share a
    /// key. Without an identity, the second `subscribe` would silently drop
    /// the first's sender and either one's `Drop` would evict the other's
    /// entry - so both callers would be told `None` and the completion would
    /// be claimed and discarded.
    generation: u64,
    /// The single sender for this trace's [`TraceState`]. It lives only here, so
    /// completion (or a sweep, or `close`) drops it and the subscriber's
    /// receiver closes - there is no second sender to leak. A watch rather than
    /// a queue because a subscriber wants the situation now, not every retry
    /// that led to it, and a slow subscriber must not accumulate a backlog.
    tx: watch::Sender<TraceState>,
    /// Whether this subscriber will actually read `Retrying` states. A caller
    /// that only awaits completion never sets it, so the retry reporter can skip
    /// the query entirely for an instance whose callers all do that.
    wants_retries: Arc<AtomicBool>,
}

/// Every trace this process is currently waiting on.
#[derive(Debug, Default)]
pub struct TraceRegistry {
    entries: Mutex<HashMap<String, Interest>>,
    /// Woken when the registry goes from empty to occupied.
    ///
    /// Without this the collector has no reason to look: it sleeps on its
    /// notifiers whenever nobody is waiting, and a new subscription is exactly
    /// the event that makes looking worthwhile.
    arrived: Arc<tokio::sync::Notify>,
    generations: std::sync::atomic::AtomicU64,
    /// Set once the outbox stops, so a subscription taken afterwards resolves
    /// rather than waiting for a pipeline that is no longer running.
    closed: std::sync::atomic::AtomicBool,
}

impl std::fmt::Debug for Interest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Interest")
            .field("generation", &self.generation)
            .field("receivers", &self.tx.receiver_count())
            .field("wants_retries", &self.wants_retries.load(Ordering::Relaxed))
            .finish()
    }
}

impl TraceRegistry {
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            entries: Mutex::new(HashMap::new()),
            arrived: Arc::new(tokio::sync::Notify::new()),
            generations: std::sync::atomic::AtomicU64::new(0),
            closed: std::sync::atomic::AtomicBool::new(false),
        })
    }

    /// Take the entries lock, recovering from and logging poison instead of
    /// silently going dead.
    ///
    /// A panic while the lock is held leaves the interest map itself usable, so
    /// recovering the guard keeps the registry answering; and a registry that
    /// has seen a poisoning is worth a log line rather than a quiet turn to
    /// no-op. Every access goes through here so no site swallows poison.
    fn entries(&self) -> std::sync::MutexGuard<'_, HashMap<String, Interest>> {
        self.entries.lock().unwrap_or_else(|poisoned| {
            tracing::warn!("outbox subscription registry mutex poisoned; recovering");
            poisoned.into_inner()
        })
    }

    /// Register interest in a trace, before the enqueuing transaction commits.
    ///
    /// A completion cannot precede that commit, so there is no race: the
    /// registration is in place before anything could deliver to it.
    pub fn subscribe(self: &Arc<Self>, trace: &str) -> TraceSubscription {
        let (tx, rx) = watch::channel(TraceState::InFlight);
        let wants_retries = Arc::new(AtomicBool::new(false));
        let generation = self.generations.fetch_add(1, Ordering::Relaxed);
        // A subscription taken after the outbox stopped keeps no entry, so its
        // sender drops here and awaiting it yields `None` at once rather than
        // waiting on a pipeline that will never run. The `closed` check is read
        // under the entries lock so it serializes against `close()`, which sets
        // it and drains under the same lock - otherwise a subscribe that raced a
        // close could insert into the map close() already cleared.
        {
            let mut entries = self.entries();
            if !self.closed.load(Ordering::Acquire) {
                entries.insert(
                    trace.to_owned(),
                    Interest {
                        generation,
                        tx,
                        wants_retries: Arc::clone(&wants_retries),
                    },
                );
            }
            // If closed, `tx` is dropped here, so `rx` sees the channel closed
            // and the subscription resolves to `None` at once.
        }
        // Tell the collector there is now a reason to look.
        self.arrived.notify_one();
        TraceSubscription {
            trace: trace.to_owned(),
            generation,
            registry: Arc::downgrade(self),
            rx,
            wants_retries,
        }
    }

    /// The wakeup to hand the collector, fired when a subscription is taken.
    #[must_use]
    pub fn arrivals(&self) -> Arc<tokio::sync::Notify> {
        Arc::clone(&self.arrived)
    }

    /// Whether anything in this process is waiting.
    ///
    /// The notifier's gate: false means there is provably no mail worth
    /// looking for, so no query runs.
    #[must_use]
    pub fn is_idle(&self) -> bool {
        self.entries().is_empty()
    }

    /// How many traces this process is waiting on.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries().len()
    }

    /// Hand a completion to whoever is waiting for it.
    ///
    /// Returns whether anyone was: `false` means the mail has been claimed and
    /// has nowhere to go, which is the case when the guard was dropped or the
    /// process restarted, and the mail is then correctly discarded rather than
    /// retained.
    pub fn deliver(&self, outcome: TraceOutcome) -> bool {
        let mut entries = self.entries();
        let Some(interest) = entries.get(&outcome.trace) else {
            return false;
        };
        // Publish the terminal state, then remove the entry so the only sender
        // drops and the receiver closes once it has observed `Completed`. A
        // second delivery finds no entry and returns false, so a completion is
        // announced at most once.
        let trace = outcome.trace.clone();
        let sent = interest.tx.send(TraceState::Completed(outcome)).is_ok();
        entries.remove(&trace);
        sent
    }

    /// Whether any live subscription will actually read `Retrying` states.
    ///
    /// The retry reporter's gate. A caller that only awaits completion never
    /// sets this, so an instance full of such callers issues no retry query at
    /// all - the feature costs nothing until it is used.
    #[must_use]
    pub fn wants_retry_reports(&self) -> bool {
        self.entries()
            .values()
            .any(|interest| interest.wants_retries.load(Ordering::Relaxed))
    }

    /// Report that this trace is stuck retrying an entity, to whoever is
    /// watching it.
    ///
    /// Returns whether the state changed. Reporting the same retry twice is
    /// harmless - the receiver sees a change only when the value differs.
    pub fn publish_retry(&self, trace: &str, retrying: TraceState) -> bool {
        let entries = self.entries();
        let Some(interest) = entries.get(trace) else {
            return false;
        };
        if !interest.wants_retries.load(Ordering::Relaxed) {
            return false;
        }
        interest.tx.send_if_modified(|current| {
            if *current == retrying {
                false
            } else {
                *current = retrying;
                true
            }
        })
    }

    /// Move every trace outside `still_retrying` back to `InFlight`, because
    /// those batches are moving again.
    pub fn clear_retries_except(&self, still_retrying: &HashSet<String>) {
        // Collect the senders to clear under the lock, then release it before
        // writing: `send_if_modified` takes a watch write lock and wakes wakers,
        // and `deliver` contends for this same mutex on every completion.
        let to_clear: Vec<watch::Sender<TraceState>> = {
            let entries = self.entries();
            entries
                .iter()
                .filter(|(trace, _)| !still_retrying.contains(*trace))
                .map(|(_, interest)| interest.tx.clone())
                .collect()
        };
        for tx in to_clear {
            tx.send_if_modified(|current| {
                if matches!(current, TraceState::Retrying { .. }) {
                    *current = TraceState::InFlight;
                    true
                } else {
                    false
                }
            });
        }
    }

    /// Release one subscription's entry, and only its own.
    fn release(&self, trace: &str, generation: u64) {
        let mut entries = self.entries();
        if entries
            .get(trace)
            .is_some_and(|interest| interest.generation == generation)
        {
            entries.remove(trace);
        }
    }

    /// Stop answering, and resolve everyone still waiting.
    ///
    /// Dropping every sender is what makes an awaiting subscription yield
    /// `None` - the documented signal that this process can no longer answer
    /// and the caller should ask [`Outbox::trace_status`](super::Outbox::trace_status)
    /// instead. Without this, a caller waiting when the outbox stops waits for
    /// ever, because it necessarily holds the `Arc` that owns this registry.
    pub fn close(&self) {
        // Set closed and drain under the entries lock, so a concurrent
        // `subscribe` (which checks closed under the same lock) cannot insert an
        // interest this drain has already passed.
        let mut entries = self.entries();
        self.closed
            .store(true, std::sync::atomic::Ordering::Release);
        entries.clear();
    }
}

/// One batch's live state, and the means of following it.
///
/// A single channel of [`TraceState`]: the batch is `InFlight`, occasionally
/// `Retrying` while a handler is stuck on one entity, and finally `Completed`.
/// [`completion`](Self::completion) awaits just the result; [`next`](Self::next)
/// yields every state change for a caller that also cares about retries.
///
/// Either resolves to `None` when this process can no longer answer - the outbox
/// was stopped, or this registry outlived the caller. The durable answer is
/// always available from [`Outbox::trace_status`](super::Outbox::trace_status),
/// which reads the trace row rather than this process's memory.
///
/// Dropping the subscription releases it, with no statement issued.
pub struct TraceSubscription {
    trace: String,
    generation: u64,
    registry: Weak<TraceRegistry>,
    rx: watch::Receiver<TraceState>,
    /// Shared with the registry entry; set the first time the caller asks for a
    /// state change, so the retry reporter only runs for callers that read it.
    wants_retries: Arc<AtomicBool>,
}

impl TraceSubscription {
    /// The trace being followed.
    #[must_use]
    pub fn trace(&self) -> &str {
        &self.trace
    }

    /// Await the batch's outcome, ignoring intermediate retry states.
    ///
    /// Resolves to `None` if this process can no longer answer, in which case
    /// the durable answer is [`Outbox::trace_status`](super::Outbox::trace_status).
    /// Cancel-safe: it waits on [`watch::Receiver::changed`].
    pub async fn completion(mut self) -> Option<TraceOutcome> {
        loop {
            if let TraceState::Completed(outcome) = &*self.rx.borrow_and_update() {
                return Some(outcome.clone());
            }
            if self.rx.changed().await.is_err() {
                return None;
            }
        }
    }

    /// The next state change, or `None` once the batch has completed or this
    /// process can no longer answer.
    ///
    /// Asking for state changes is what makes this instance look for retries, so
    /// a caller that only uses [`completion`](Self::completion) pays nothing.
    /// Cancel-safe: it waits on [`watch::Receiver::changed`].
    ///
    /// ```ignore
    /// let mut sub = outbox.subscribe("import-1")?;
    /// while let Some(state) = sub.next().await {
    ///     match state {
    ///         TraceState::Retrying { attempts, .. } => tracing::warn!(attempts, "stuck"),
    ///         TraceState::Completed(outcome)        => { handle(outcome); break; }
    ///         TraceState::InFlight                  => {}
    ///     }
    /// }
    /// ```
    pub async fn next(&mut self) -> Option<TraceState> {
        self.wants_retries.store(true, Ordering::Relaxed);
        match self.rx.changed().await {
            Ok(()) => Some(self.rx.borrow_and_update().clone()),
            Err(_) => None,
        }
    }
}

impl Drop for TraceSubscription {
    fn drop(&mut self) {
        if let Some(registry) = self.registry.upgrade() {
            registry.release(&self.trace, self.generation);
        }
    }
}

/// A running callback watch over one trace, returned by
/// [`Outbox::watch_trace`](super::Outbox::watch_trace) and
/// [`Outbox::watch_trace_events`](super::Outbox::watch_trace_events).
///
/// The callback runs on a spawned task, not inline in a worker, so a slow
/// handler cannot stall the pipeline. Dropping the guard aborts that task and
/// releases the subscription; keep it alive for as long as the callback matters.
#[must_use = "dropping the guard immediately stops the watch"]
pub struct TraceWatch {
    handle: tokio::task::JoinHandle<()>,
}

impl TraceWatch {
    pub(super) fn new(handle: tokio::task::JoinHandle<()>) -> Self {
        Self { handle }
    }
}

impl Drop for TraceWatch {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

impl std::fmt::Debug for TraceWatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TraceWatch").finish()
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    fn outcome(trace: &str) -> TraceOutcome {
        TraceOutcome {
            trace: trace.to_owned(),
            entities: 2,
            failures: 0,
            attempts: 0,
            completed_at: chrono::Utc::now(),
        }
    }

    fn retrying(attempts: i64) -> TraceState {
        TraceState::Retrying {
            entities: 4,
            pending: 3,
            failures: 0,
            attempts,
            last_error: Some("upstream refused".to_owned()),
            retrying_since: chrono::Utc::now(),
        }
    }

    /// Spawn a task that follows a subscription's state changes into a channel,
    /// stopping after `Completed`. Modelling the real callback path, this is
    /// also what arms the retry query (the first `next()` sets `wants_retries`).
    fn spawn_watcher(
        mut sub: TraceSubscription,
    ) -> tokio::sync::mpsc::UnboundedReceiver<TraceState> {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        tokio::spawn(async move {
            while let Some(state) = sub.next().await {
                let done = matches!(state, TraceState::Completed(_));
                if tx.send(state).is_err() || done {
                    break;
                }
            }
        });
        rx
    }

    async fn arm_retries(registry: &Arc<TraceRegistry>) {
        while !registry.wants_retry_reports() {
            tokio::task::yield_now().await;
        }
    }

    #[tokio::test]
    async fn a_subscription_receives_its_completion() {
        let registry = TraceRegistry::new();
        let sub = registry.subscribe("t1");
        assert_eq!(registry.len(), 1);
        assert!(!registry.is_idle());

        assert!(registry.deliver(outcome("t1")));
        let received = sub.completion().await.expect("delivered");
        assert_eq!(received.trace, "t1");
        assert_eq!(received.entities, 2);
        assert!(received.is_clean());
    }

    #[tokio::test]
    async fn a_completion_is_delivered_at_most_once() {
        let registry = TraceRegistry::new();
        let sub = registry.subscribe("t1");

        assert!(registry.deliver(outcome("t1")));
        assert!(
            !registry.deliver(outcome("t1")),
            "a second delivery must find nothing to send"
        );
        assert!(sub.completion().await.is_some());
    }

    #[test]
    fn dropping_a_subscription_releases_it() {
        let registry = TraceRegistry::new();
        let sub = registry.subscribe("t1");
        assert_eq!(registry.len(), 1);
        drop(sub);
        assert_eq!(registry.len(), 0);
        assert!(registry.is_idle());
    }

    #[test]
    fn mail_for_a_released_subscription_has_nowhere_to_go() {
        let registry = TraceRegistry::new();
        drop(registry.subscribe("t1"));
        assert!(
            !registry.deliver(outcome("t1")),
            "the caller stamps and discards it"
        );
    }

    #[test]
    fn mail_for_a_trace_nobody_asked_about_is_not_delivered() {
        let registry = TraceRegistry::new();
        assert!(!registry.deliver(outcome("never-subscribed")));
    }

    #[test]
    fn only_awaiting_completion_arms_no_retry_query() {
        let registry = TraceRegistry::new();
        let _sub = registry.subscribe("t1");
        assert!(
            !registry.wants_retry_reports(),
            "a caller that only awaits completion pays for no retry query"
        );
    }

    #[tokio::test]
    async fn following_state_changes_arms_the_retry_query() {
        let registry = TraceRegistry::new();
        let _rx = spawn_watcher(registry.subscribe("t1"));
        arm_retries(&registry).await;
        assert!(registry.wants_retry_reports());
    }

    #[tokio::test]
    async fn a_retry_reaches_a_watcher_clears_and_the_watch_closes_at_completion() {
        let registry = TraceRegistry::new();
        let mut rx = spawn_watcher(registry.subscribe("t1"));
        arm_retries(&registry).await;

        // A retry reaches the watcher.
        assert!(registry.publish_retry("t1", retrying(3)));
        assert!(
            matches!(
                rx.recv().await,
                Some(TraceState::Retrying { attempts: 3, .. })
            ),
            "the retry reaches the watcher"
        );

        // The batch moves again: the retry is cleared back to InFlight.
        registry.clear_retries_except(&HashSet::new());
        assert!(
            matches!(rx.recv().await, Some(TraceState::InFlight)),
            "a batch that moved again is no longer retrying"
        );

        // Completion is the terminal state, and the channel closes after it -
        // the watcher sees Completed then None and stops (no leaked task).
        assert!(registry.deliver(outcome("t1")));
        assert!(matches!(rx.recv().await, Some(TraceState::Completed(_))));
        assert!(
            rx.recv().await.is_none(),
            "the watch closes at completion instead of waiting for ever"
        );
    }

    #[tokio::test]
    async fn a_retry_for_an_unwatched_trace_goes_nowhere() {
        let registry = TraceRegistry::new();
        let _sub = registry.subscribe("t1");
        assert!(
            !registry.publish_retry("t1", retrying(3)),
            "subscribed, but not following state changes"
        );
        assert!(!registry.publish_retry("never-subscribed", retrying(3)));
    }

    #[tokio::test]
    async fn two_subscriptions_on_one_trace_do_not_destroy_each_other() {
        // `trace_status` documents duplicate traces as legal, and the registry
        // is keyed by the trace string, so dropping the older must not evict the
        // newer one's entry.
        let registry = TraceRegistry::new();
        let first = registry.subscribe("t1");
        let second = registry.subscribe("t1");

        drop(first);
        assert_eq!(
            registry.len(),
            1,
            "the newer subscription is still registered"
        );
        assert!(
            registry.deliver(outcome("t1")),
            "and its completion still has somewhere to go"
        );
        assert!(second.completion().await.is_some());
    }

    #[test]
    fn dropping_the_newer_subscription_leaves_no_stale_entry() {
        let registry = TraceRegistry::new();
        let first = registry.subscribe("t1");
        let second = registry.subscribe("t1");
        drop(second);
        assert_eq!(
            registry.len(),
            0,
            "the entry it owned is gone, and it did not resurrect the older one"
        );
        drop(first);
        assert_eq!(registry.len(), 0);
    }

    #[tokio::test]
    async fn closing_resolves_everyone_still_waiting() {
        // What `stop()` relies on: dropping the senders is the documented
        // signal that this process can no longer answer.
        let registry = TraceRegistry::new();
        let waiting = registry.subscribe("t1");
        registry.close();
        assert!(
            waiting.completion().await.is_none(),
            "a caller waiting when the outbox stops is told so, rather than waiting for ever"
        );
    }

    #[tokio::test]
    async fn subscribing_after_close_resolves_at_once() {
        let registry = TraceRegistry::new();
        registry.close();
        assert!(
            registry.subscribe("t1").completion().await.is_none(),
            "there is no pipeline left to answer, so do not pretend to wait for one"
        );
        assert!(registry.is_idle(), "and nothing is left registered");
    }

    #[tokio::test]
    async fn awaiting_yields_none_once_the_registry_is_gone() {
        let registry = TraceRegistry::new();
        let sub = registry.subscribe("t1");
        drop(registry);
        assert!(
            sub.completion().await.is_none(),
            "this process can no longer answer; the trace row still can"
        );
    }
}
