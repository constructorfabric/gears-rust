// @cpt-dod:cpt-cf-settings-service-dod-consumer-activation-delivery:p1
//! The Activation Publisher's notification half: the `change_notification` of
//! the end of a request, and the delivery loop's pass over what stays
//! `awaiting`.
//!
//! The awaiting states are the delivery queue. The end of a request publishes
//! each touched `(subscriber, tenant)` pair's keys promptly; the loop
//! re-publishes every state once `redeliver_interval_seconds` has passed since
//! its last publish, until the consumer reports or a later change replaces it.
//! No state reaches a terminal status on a timer, and nothing here carries a
//! value: a notification names the tenant, the subscriber and the keys with
//! their `change_ref`.
//!
//! Everything here reaches the broker through the traits below, so the rules
//! run without one; the envelope and the size split are the broker client's
//! (`infra::broadcast`), the queries the repository's.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use event_broker_sdk::Event;
use tokio_util::sync::CancellationToken;
use toolkit_macros::domain_model;
use uuid::Uuid;

use super::{ActivationPairs, HeldKey, NotifyMetrics, SubscriberTenant};
use crate::domain::broker_sink::{BroadcastSink, NotificationResult, PublishError};
use crate::domain::error::DomainError;

/// One request's publishing as the notifier sees it: the request's own session,
/// whose skip rule — nothing is attempted once a publish of the request has
/// failed — the notifications share with the request's broadcasts. The
/// coherence half implements it for its `RequestBroadcast`; the notifier
/// depends on this trait alone.
#[async_trait]
pub trait NotificationPublisher: Send {
    /// Whether the request can publish a notification at all: a broker
    /// resolved, the activation types registered and the root tenant's id
    /// known.
    fn can_notify(&self) -> bool;

    /// Whether an earlier publish of the request failed.
    fn has_failed(&self) -> bool;

    /// Publish one `change_notification` of the request, skipped once a
    /// publish of the request has failed.
    async fn publish_notification(&mut self, event: Event) -> NotificationResult;
}

/// One key a notification names, with the change that owes a report.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotifiedChange {
    /// The setting key.
    pub key: String,
    /// The id of the change's audit record.
    pub change_ref: Uuid,
}

/// An `awaiting` state that is due for a publish, with its pair.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DueRow {
    /// The pair the state belongs to.
    pub pair: SubscriberTenant,
    /// Its key and `change_ref`.
    pub change: NotifiedChange,
}

/// A notification built for the broker, with the states it carries: the ones
/// whose `last_notified_at` the broker's acceptance stamps.
#[domain_model]
#[derive(Debug)]
pub struct BuiltNotification {
    /// The broker-native event.
    pub event: Event,
    /// The keys of its body.
    pub changes: Vec<NotifiedChange>,
}

/// How many subscriptions and `awaiting` states exist: what the gauges report.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ActivationCounts {
    /// Registered subscriptions.
    pub subscriptions: u64,
    /// Activation states with status `awaiting`.
    pub awaiting: u64,
}

/// What the notification half reads and writes in the settings database.
///
/// Every write is guarded by the `change_ref` the state was read with, so a row
/// a later change replaced meanwhile is skipped, never stamped.
#[async_trait]
pub trait NotificationStore: Send + Sync {
    /// Every `awaiting` state of the pair, in key order, through
    /// `idx_activation_awaiting`.
    ///
    /// # Errors
    /// [`DomainError`] when the read fails.
    async fn awaiting_of(
        &self,
        pair: &SubscriberTenant,
    ) -> Result<Vec<NotifiedChange>, DomainError>;

    /// Stamp `last_notified_at` now on each of the pair's states in `changes`
    /// that still names its `change_ref` and is still `awaiting`. Returns how
    /// many rows were stamped.
    ///
    /// # Errors
    /// [`DomainError`] when the statement fails.
    async fn stamp(
        &self,
        pair: &SubscriberTenant,
        changes: &[NotifiedChange],
    ) -> Result<u64, DomainError>;

    /// The `awaiting` states whose `last_notified_at` is absent or older than
    /// `interval`, ordered by pair and key.
    ///
    /// # Errors
    /// [`DomainError`] when the read fails.
    async fn due(&self, interval: Duration) -> Result<Vec<DueRow>, DomainError>;

    /// Delete, in one conditional statement, every state whose subscription no
    /// longer exists, whatever its status. Returns how many were deleted.
    ///
    /// # Errors
    /// [`DomainError`] when the statement fails.
    async fn sweep_orphans(&self) -> Result<u64, DomainError>;

    /// The counts the gauges report.
    ///
    /// # Errors
    /// [`DomainError`] when a count fails.
    async fn counts(&self) -> Result<ActivationCounts, DomainError>;
}

/// The broker client as the notification half sees it.
#[async_trait]
pub trait NotificationGateway: Send + Sync {
    /// Whether the hub holds a broker now: a lookup, not a call.
    fn broker_present(&self) -> bool;

    /// The root tenant's id, once the replica's consumer task learned it.
    fn root_tenant(&self) -> Option<Uuid>;

    /// Whether the activation topic and the `change_notification` type are
    /// registered.
    fn registered(&self) -> bool;

    /// Register the activation topic and the `change_notification` type,
    /// idempotently. `true` when both are registered; a refusal is logged by
    /// the client and answered `false`. Cut short by `cancel`.
    async fn register(&self, cancel: &CancellationToken) -> bool;

    /// A publishing session for the loop's pass: `None` while no broker is in
    /// the hub or the root tenant is unknown.
    fn session(&self) -> Option<Box<dyn BroadcastSink>>;

    /// The notifications of one pair, from its states: one event or — when its
    /// keys would not fit the broker's event limit — several, each state in
    /// exactly one.
    fn notifications(
        &self,
        pair: &SubscriberTenant,
        changes: &[NotifiedChange],
        trace_parent: Option<String>,
    ) -> Vec<BuiltNotification>;
}

/// The scan lock: a single non-blocking attempt, never a wait.
#[async_trait]
pub trait ScanLock: Send + Sync {
    /// Try the lock once. `Ok(None)` when another replica holds it.
    ///
    /// # Errors
    /// [`DomainError::Unavailable`] when the lock backend cannot be reached.
    async fn try_acquire(&self) -> Result<Option<Box<dyn HeldKey>>, DomainError>;
}

/// How a tick of the delivery loop ended.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tick {
    /// No broker resolves, or the root tenant's id is unknown: the gauges were
    /// refreshed and nothing else was done.
    Idle,
    /// The activation types are not registered (refused or failed): logged,
    /// retried on the next tick.
    NotRegistered,
    /// Another replica holds the scan lock: this replica's pass ended at once.
    Skipped,
    /// The pass ran to its end.
    Complete {
        /// How many notifications the broker accepted.
        published: usize,
    },
    /// The broker disappeared during the pass: the rest was abandoned.
    BrokerLost,
    /// The broker stopped accepting during the pass — a publish timed out, or
    /// [`CONSECUTIVE_REFUSALS`] in a row were refused: the rest was abandoned,
    /// so a stalled broker costs one timeout per tick, not one per pair.
    Stalled,
    /// The stop fired during the pass.
    Cancelled,
    /// A statement of the pass failed; the states are kept.
    Failed,
}

/// How many publishes of a pass the broker may refuse in a row before the pass
/// is abandoned.
pub const CONSECUTIVE_REFUSALS: u8 = 3;

/// The notification half of the Activation Publisher.
// @cpt-dod:cpt-cf-settings-service-dod-consumer-activation-observability:p1
#[domain_model]
pub struct ActivationNotifier {
    store: Arc<dyn NotificationStore>,
    gateway: Arc<dyn NotificationGateway>,
    scan: Arc<dyn ScanLock>,
    metrics: Arc<dyn NotifyMetrics>,
    redeliver_interval: Duration,
}

/// What one pass came to for one pair's notification.
#[domain_model]
enum Sent {
    Accepted,
    Refused,
    Lost,
    Stalled,
    Cancelled,
}

/// What the refusals of one pass amount to so far.
#[domain_model]
#[derive(Default)]
struct Refusals {
    /// Refused publishes since the last accepted one.
    consecutive: u8,
    /// Whether the pass already logged a refusal at `warn`.
    warned: bool,
}

impl ActivationNotifier {
    /// A notifier over the store, the broker client's view and the scan lock;
    /// `redeliver_interval` is `redeliver_interval_seconds`.
    #[must_use]
    pub fn new(
        store: Arc<dyn NotificationStore>,
        gateway: Arc<dyn NotificationGateway>,
        scan: Arc<dyn ScanLock>,
        metrics: Arc<dyn NotifyMetrics>,
        redeliver_interval: Duration,
    ) -> Self {
        Self {
            store,
            gateway,
            scan,
            metrics,
            redeliver_interval,
        }
    }

    /// The loop's interval: `redeliver_interval_seconds`.
    #[must_use]
    pub const fn interval(&self) -> Duration {
        self.redeliver_interval
    }

    /// Whether the activation types are registered: the end-of-request hook
    /// publishes no notification before.
    #[must_use]
    pub fn registered(&self) -> bool {
        self.gateway.registered()
    }

    /// The notification half of the end-of-request hook: per pair the request
    /// touched, in first-touch order, publish the pair's `awaiting` keys on the
    /// request's own session, so the skip rule is shared with its broadcasts.
    /// Never fails the request: whatever is not published stays `awaiting` for
    /// the delivery loop.
    pub async fn publish_request(
        &self,
        request: &mut dyn NotificationPublisher,
        pairs: &ActivationPairs,
        trace_parent: Option<&str>,
    ) {
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-publish:p1:inst-ca-publish-1
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-publish:p1:inst-ca-publish-2
        if pairs.is_empty() {
            return;
        }
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-publish:p1:inst-ca-publish-2
        if !request.can_notify() {
            tracing::debug!(
                pairs = pairs.pairs().len(),
                "change_notification not published; the delivery loop covers the awaiting states"
            );
            return;
        }
        if request.has_failed() {
            tracing::debug!(
                "change_notification not published after a refused publish of the request"
            );
            return;
        }
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-publish:p1:inst-ca-publish-5
        for pair in pairs.pairs() {
            if !self.publish_pair(request, pair, trace_parent).await {
                break;
            }
        }
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-publish:p1:inst-ca-publish-5
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-publish:p1:inst-ca-publish-12
        // The request answers its caller with its own outcomes, whatever the
        // notifications' were; this hook returns nothing to it.
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-publish:p1:inst-ca-publish-12
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-publish:p1:inst-ca-publish-1
    }

    /// One pair of the request: `false` when the request's remaining publishes
    /// are skipped.
    async fn publish_pair(
        &self,
        request: &mut dyn NotificationPublisher,
        pair: &SubscriberTenant,
        trace_parent: Option<&str>,
    ) -> bool {
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-publish:p1:inst-ca-publish-6
        let awaiting = match self.store.awaiting_of(pair).await {
            Ok(rows) => rows,
            Err(err) => {
                tracing::warn!(
                    subscriber = %pair.subscriber,
                    tenant = %pair.tenant,
                    %err,
                    "the awaiting states of a pair could not be read; the delivery loop publishes them"
                );
                return true;
            }
        };
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-publish:p1:inst-ca-publish-6
        if awaiting.is_empty() {
            return true;
        }
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-publish:p1:inst-ca-publish-7
        let built = self
            .gateway
            .notifications(pair, &awaiting, trace_parent.map(str::to_owned));
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-publish:p1:inst-ca-publish-7
        for notification in built {
            match request.publish_notification(notification.event).await {
                // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-publish:p1:inst-ca-publish-10
                NotificationResult::Accepted => {
                    self.stamp(pair, &notification.changes).await;
                }
                // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-publish:p1:inst-ca-publish-10
                NotificationResult::Skipped => return false,
                // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-publish:p1:inst-ca-publish-11
                NotificationResult::Failed(reason) => {
                    tracing::warn!(
                        subscriber = %pair.subscriber,
                        tenant = %pair.tenant,
                        %reason,
                        "change_notification dropped; the delivery loop publishes the awaiting states"
                    );
                    return false;
                } // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-publish:p1:inst-ca-publish-11
            }
        }
        true
    }

    /// Stamp what the broker accepted. A failed stamp only makes the loop
    /// re-publish the states at its next tick: logged, never raised.
    async fn stamp(&self, pair: &SubscriberTenant, changes: &[NotifiedChange]) {
        if let Err(err) = self.store.stamp(pair, changes).await {
            tracing::warn!(
                subscriber = %pair.subscriber,
                tenant = %pair.tenant,
                %err,
                "a published notification could not be stamped; it is re-published at the next due tick"
            );
        }
    }

    /// One tick of the delivery loop, after the interval: refresh the gauges,
    /// then — with a broker, the root tenant's id and the registered activation
    /// types — run one pass.
    pub async fn tick(&self, cancel: &CancellationToken) -> Tick {
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-lifecycle:p1:inst-ca-lifecycle-8
        self.refresh_gauges().await;
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-lifecycle:p1:inst-ca-lifecycle-8
        if let Some(idle) = self.idle() {
            return idle;
        }
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-lifecycle:p1:inst-ca-lifecycle-11
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-lifecycle:p1:inst-ca-lifecycle-12
        if !self.gateway.registered() && !self.gateway.register(cancel).await {
            // The client logged the reason at `warn`; the next tick registers
            // again, and publishes and the scan stay off meanwhile.
            return Tick::NotRegistered;
        }
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-lifecycle:p1:inst-ca-lifecycle-12
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-lifecycle:p1:inst-ca-lifecycle-11
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-lifecycle:p1:inst-ca-lifecycle-13
        let outcome = self.deliver(cancel).await;
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-lifecycle:p1:inst-ca-lifecycle-13
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-lifecycle:p1:inst-ca-lifecycle-14
        if outcome == Tick::BrokerLost {
            tracing::warn!(
                "the broker was lost during a delivery pass; every awaiting state is kept"
            );
        }
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-lifecycle:p1:inst-ca-lifecycle-14
        outcome
    }

    /// `Some(Tick::Idle)` when the tick has nothing to do: no broker resolves,
    /// or the root tenant's id is not yet known.
    fn idle(&self) -> Option<Tick> {
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-lifecycle:p1:inst-ca-lifecycle-9
        if !self.gateway.broker_present() {
            tracing::debug!("no broker resolves; the delivery loop idles");
            return Some(Tick::Idle);
        }
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-lifecycle:p1:inst-ca-lifecycle-9
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-lifecycle:p1:inst-ca-lifecycle-10
        if self.gateway.root_tenant().is_none() {
            tracing::debug!("the root tenant's id is not yet known; the delivery loop idles");
            return Some(Tick::Idle);
        }
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-lifecycle:p1:inst-ca-lifecycle-10
        None
    }

    /// Set the two gauges from the counts, whether or not a broker resolves.
    async fn refresh_gauges(&self) {
        match self.store.counts().await {
            Ok(counts) => {
                self.metrics.subscriptions(counts.subscriptions);
                self.metrics.awaiting(counts.awaiting);
            }
            Err(err) => tracing::warn!(%err, "the activation gauges could not be refreshed"),
        }
    }

    /// One pass of the deliver flow, under the scan lock.
    async fn deliver(&self, cancel: &CancellationToken) -> Tick {
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-deliver:p1:inst-ca-deliver-1
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-deliver:p1:inst-ca-deliver-2
        let held = match self.scan.try_acquire().await {
            Ok(Some(held)) => held,
            Ok(None) => {
                // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-deliver:p1:inst-ca-deliver-3
                tracing::debug!("another replica holds the scan lock; this tick is skipped");
                return Tick::Skipped;
                // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-deliver:p1:inst-ca-deliver-3
            }
            Err(err) => {
                tracing::warn!(%err, "the scan lock could not be attempted; this tick is skipped");
                return Tick::Failed;
            }
        };
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-deliver:p1:inst-ca-deliver-2
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-deliver:p1:inst-ca-deliver-1
        let outcome = self.pass(cancel).await;
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-deliver:p1:inst-ca-deliver-14
        held.release().await;
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-deliver:p1:inst-ca-deliver-14
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-deliver:p1:inst-ca-deliver-15
        outcome
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-deliver:p1:inst-ca-deliver-15
    }

    /// The body of a pass: the orphan sweep, the due states per pair.
    async fn pass(&self, cancel: &CancellationToken) -> Tick {
        if !self.sweep().await {
            return Tick::Failed;
        }
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-deliver:p1:inst-ca-deliver-5
        let due = match self.store.due(self.redeliver_interval).await {
            Ok(rows) => rows,
            Err(err) => {
                tracing::warn!(%err, "the due states could not be read; the pass is abandoned");
                return Tick::Failed;
            }
        };
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-deliver:p1:inst-ca-deliver-5
        let Some(session) = self.gateway.session() else {
            return Tick::BrokerLost;
        };
        let mut published = 0;
        let mut refusals = Refusals::default();
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-deliver:p1:inst-ca-deliver-6
        for (pair, changes) in group_by_pair(due) {
            match self
                .republish_pair(session.as_ref(), &pair, &changes, &mut refusals, cancel)
                .await
            {
                Ok(accepted) => published += accepted,
                Err(ended) => return ended,
            }
        }
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-deliver:p1:inst-ca-deliver-6
        Tick::Complete { published }
    }

    /// The orphan sweep: `false` when its statement failed and the pass is
    /// abandoned.
    async fn sweep(&self) -> bool {
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-deliver:p1:inst-ca-deliver-4
        // @cpt-begin:cpt-cf-settings-service-state-consumer-activation-row:p1:inst-ca-row-6
        match self.store.sweep_orphans().await {
            Ok(0) => true,
            Ok(swept) => {
                tracing::info!(swept, "activation states of retired subscriptions swept");
                true
            }
            Err(err) => {
                tracing::warn!(%err, "the orphan sweep failed; the pass is abandoned");
                false
            }
        }
        // @cpt-end:cpt-cf-settings-service-state-consumer-activation-row:p1:inst-ca-row-6
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-deliver:p1:inst-ca-deliver-4
    }

    /// One pair's notifications of the pass: how many the broker accepted, or
    /// the tick's end when the broker is lost or the stop fired.
    async fn republish_pair(
        &self,
        session: &dyn BroadcastSink,
        pair: &SubscriberTenant,
        changes: &[NotifiedChange],
        refusals: &mut Refusals,
        cancel: &CancellationToken,
    ) -> Result<usize, Tick> {
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-deliver:p1:inst-ca-deliver-7
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-deliver:p1:inst-ca-deliver-8
        let built = self.gateway.notifications(pair, changes, None);
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-deliver:p1:inst-ca-deliver-8
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-deliver:p1:inst-ca-deliver-7
        let mut accepted = 0;
        for notification in built {
            match self
                .republish(session, pair, notification, refusals, cancel)
                .await
            {
                Sent::Accepted => accepted += 1,
                Sent::Refused => {}
                Sent::Lost => return Err(Tick::BrokerLost),
                Sent::Stalled => return Err(Tick::Stalled),
                Sent::Cancelled => return Err(Tick::Cancelled),
            }
        }
        Ok(accepted)
    }

    /// Publish one notification of the pass and settle what it came to.
    async fn republish(
        &self,
        session: &dyn BroadcastSink,
        pair: &SubscriberTenant,
        notification: BuiltNotification,
        refusals: &mut Refusals,
        cancel: &CancellationToken,
    ) -> Sent {
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-deliver:p1:inst-ca-deliver-9
        let published = tokio::select! {
            biased;
            () = cancel.cancelled() => None,
            out = session.publish(notification.event) => Some(out),
        };
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-deliver:p1:inst-ca-deliver-9
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-deliver:p1:inst-ca-deliver-13
        let Some(published) = published else {
            // The publish in flight is abandoned and every awaiting state is
            // kept; the lock is released by the caller.
            tracing::info!("the delivery pass was abandoned by the stop");
            return Sent::Cancelled;
        };
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-deliver:p1:inst-ca-deliver-13
        match published {
            Ok(()) => {
                refusals.consecutive = 0;
                // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-deliver:p1:inst-ca-deliver-10
                self.stamp(pair, &notification.changes).await;
                // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-deliver:p1:inst-ca-deliver-10
                // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-deliver:p1:inst-ca-deliver-16
                self.metrics.redelivery();
                // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-deliver:p1:inst-ca-deliver-16
                Sent::Accepted
            }
            Err(reason) => self.refused(pair, &reason, refusals),
        }
    }

    /// A refused or timed-out publish of the pass: the broker gone ends the
    /// pass as lost; a publish that timed out, or the
    /// [`CONSECUTIVE_REFUSALS`]th refusal in a row, ends it as stalled; anything
    /// else leaves the states due at the next tick. Only the first refusal of a
    /// pass is logged at `warn` — a missing `produce` grant refuses every pair
    /// alike — the rest at `debug`.
    fn refused(
        &self,
        pair: &SubscriberTenant,
        reason: &PublishError,
        refusals: &mut Refusals,
    ) -> Sent {
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-deliver:p1:inst-ca-deliver-12
        if !self.gateway.broker_present() {
            return Sent::Lost;
        }
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-deliver:p1:inst-ca-deliver-12
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-deliver:p1:inst-ca-deliver-11
        if refusals.warned {
            tracing::debug!(
                subscriber = %pair.subscriber,
                tenant = %pair.tenant,
                %reason,
                "change_notification refused; the states stay awaiting until the next due tick"
            );
        } else {
            refusals.warned = true;
            tracing::warn!(
                subscriber = %pair.subscriber,
                tenant = %pair.tenant,
                %reason,
                "change_notification refused; the states stay awaiting until the next due tick"
            );
        }
        refusals.consecutive = refusals.consecutive.saturating_add(1);
        if matches!(reason, PublishError::TimedOut) || refusals.consecutive >= CONSECUTIVE_REFUSALS
        {
            return Sent::Stalled;
        }
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-deliver:p1:inst-ca-deliver-11
        Sent::Refused
    }
}

/// The due states grouped per pair, in the order the store returned them (by
/// pair and key), each pair once.
fn group_by_pair(due: Vec<DueRow>) -> Vec<(SubscriberTenant, Vec<NotifiedChange>)> {
    let mut groups: Vec<(SubscriberTenant, Vec<NotifiedChange>)> = Vec::new();
    for row in due {
        match groups.last_mut() {
            Some((pair, changes)) if *pair == row.pair => changes.push(row.change),
            _ => groups.push((row.pair, vec![row.change])),
        }
    }
    groups
}

#[cfg(test)]
#[path = "notifier_tests.rs"]
mod notifier_tests;
