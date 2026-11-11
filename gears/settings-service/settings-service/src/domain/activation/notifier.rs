// @cpt-dod:cpt-cf-settings-service-dod-consumer-activation-delivery:p1
// @cpt-dod:cpt-cf-settings-service-dod-consumer-activation-report-timeout:p1
//! The Activation Publisher's notification half: the `change_notification` of
//! the end of a request, and the delivery loop's pass over what stays
//! `awaiting`.
//!
//! The awaiting states are the delivery queue. A state is due when it is
//! `awaiting` and was never published or last published at least
//! `redeliver_interval_seconds` ago — the delivery loop forgiving a tenth of
//! the interval ([`tick_age`]), so what its previous tick published is due at
//! its next. The end of a request publishes each touched
//! `(subscriber, tenant)` pair's due keys promptly; the loop re-publishes
//! them every interval, until the instance reports, a later change
//! replaces the state, or the report timeout calls it silent. A notification
//! carries each key once, whichever instances' states are due, and a
//! publication the broker accepted stamps and counts one delivery on exactly
//! the due states it bundled: after `K` of them a due state is called silent
//! rather than published again. A subscription silent for a day is forgotten
//! with its states. No state reaches `succeeded` or `failed` without a report,
//! and nothing here carries a value: a notification names the tenant, the
//! subscriber and the keys with their `change_ref`.
//!
//! Everything here reaches the broker through the traits below, so the rules
//! run without one; the envelope and the size split are the broker client's
//! (`infra::broadcast`), the queries the repository's.

use std::collections::HashSet;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use async_trait::async_trait;
use event_broker_sdk::EventV1;
use tokio_util::sync::CancellationToken;
use toolkit_macros::domain_model;
use tracing::Instrument as _;
use uuid::Uuid;

use super::{ActivationPairs, HeldKey, InstanceKey, NotifyMetrics, SubscriberTenant};
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
    async fn publish_notification(&mut self, event: EventV1) -> NotificationResult;
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
    /// The consumer instance whose state it is.
    pub instance_id: Uuid,
    /// Its key and `change_ref`.
    pub change: NotifiedChange,
    /// The publications of that `change_ref` the broker accepted for it so
    /// far: the report timeout is counted in them.
    pub deliveries: u32,
}

impl DueRow {
    /// The identity of the state.
    #[must_use]
    pub fn instance_key(&self) -> super::InstanceKey {
        super::InstanceKey {
            subscriber: self.pair.subscriber.clone(),
            instance_id: self.instance_id,
            key: self.change.key.clone(),
            tenant: self.pair.tenant.clone(),
        }
    }
}

/// A notification built for the broker, with the states it carries: the ones
/// whose `last_notified_at` the broker's acceptance stamps.
#[domain_model]
#[derive(Debug)]
pub struct BuiltNotification {
    /// The broker-native event.
    pub event: EventV1,
    /// The keys of its body.
    pub changes: Vec<NotifiedChange>,
}

/// What the gauges report: the subscriptions, the `awaiting` and the `silent`
/// states, and the subscriptions with `silent_since` set.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ActivationCounts {
    /// Registered subscriptions, one per instance, key and tenant.
    pub subscriptions: u64,
    /// Activation states with status `awaiting`.
    pub awaiting: u64,
    /// Activation states with status `silent`.
    pub silent: u64,
    /// Subscriptions whose `silent_since` is set.
    pub silent_subscriptions: u64,
}

/// What a call of silence that matched its state came to, judged in its own
/// transaction.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Silenced {
    /// The call turned its subscriber's account of the key at the tenant into
    /// `incomplete`: before it, none of the subscriber's other states there
    /// was `silent` or `failed`.
    pub incomplete: bool,
}

/// `SILENT_FORGET_SECONDS`: how long a subscription stays silent before the
/// delivery loop forgets it — one day, a constant, not configuration.
pub const SILENT_FORGET_SECONDS: u64 = 86_400;

/// [`SILENT_FORGET_SECONDS`] as a span.
pub const SILENT_FORGET: Duration = Duration::from_secs(SILENT_FORGET_SECONDS);

/// The default of `activation_report_timeout_seconds`: ten minutes.
pub const DEFAULT_REPORT_TIMEOUT_SECONDS: u64 = 600;

/// `K`: the accepted deliveries the report timeout amounts to at the delivery
/// loop's interval, `ceil(report_timeout / interval)` — `20` at the defaults.
#[must_use]
pub fn deliveries_within(report_timeout: Duration, interval: Duration) -> u32 {
    // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-lifecycle:p1:inst-ca-lifecycle-21
    let deliveries = report_timeout
        .as_nanos()
        .div_ceil(interval.as_nanos().max(1));
    u32::try_from(deliveries).unwrap_or(u32::MAX)
    // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-lifecycle:p1:inst-ca-lifecycle-21
}

/// The share of `redeliver_interval_seconds` the delivery loop forgives a
/// stamp's age by: a tenth.
const TICK_SLACK_DIVISOR: u32 = 10;

/// The age at which the delivery loop's pass takes a stamped state as due:
/// the interval less a tenth of it.
///
/// A pass stamps what it published after its due read, and the next tick
/// comes one interval after this one, so at the next read the state the
/// previous tick published is younger than the interval by the time from read
/// to stamp and by the jitter of the timer and of the statements before the
/// read. Taken exactly, such a state would be due at every second tick only,
/// and its `K` deliveries — the report timeout — would take twice their
/// intervals. The slack absorbs that and no more: another replica's pass or
/// the end of a request counts a state's next delivery no sooner than nine
/// tenths of an interval after its last, and the end of a request, which is
/// not a tick, reads its due states with the exact interval.
#[must_use]
pub fn tick_age(interval: Duration) -> Duration {
    interval.saturating_sub(interval / TICK_SLACK_DIVISOR)
}

/// What the notification half reads and writes in the settings database.
///
/// Every write is guarded by the `change_ref` the state was read with, so a row
/// a later change replaced meanwhile is skipped, never stamped or silenced.
#[async_trait]
pub trait NotificationStore: Send + Sync {
    /// The pair's due states — `awaiting`, with `last_notified_at` absent or
    /// at least `interval` old — of every instance of the subscriber, ordered
    /// by key and instance, through `idx_activation_awaiting`.
    ///
    /// # Errors
    /// [`DomainError`] when the read fails.
    async fn due_of(
        &self,
        pair: &SubscriberTenant,
        interval: Duration,
    ) -> Result<Vec<DueRow>, DomainError>;

    /// A publication the broker accepted, on exactly the due states it
    /// bundled: in one guarded statement, stamp `last_notified_at` now and
    /// count one delivery on each of `rows` still naming its `change_ref`,
    /// still `awaiting` and still at the `deliveries` it was read due with,
    /// and on no other row — a row another publisher stamped since the read is
    /// no longer due, and its delivery is not counted twice. Returns how many
    /// were stamped.
    ///
    /// # Errors
    /// [`DomainError`] when the statement fails.
    async fn stamp_due(&self, rows: &[DueRow]) -> Result<u64, DomainError>;

    /// The due states of every pair — `awaiting`, with `last_notified_at`
    /// absent or at least `interval` old — ordered by pair, key and instance.
    ///
    /// # Errors
    /// [`DomainError`] when the read fails.
    async fn due(&self, interval: Duration) -> Result<Vec<DueRow>, DomainError>;

    /// Call one due state silent, in one transaction and in this order: the
    /// state to `silent` with `silent_since` now, guarded on its `change_ref`
    /// and `awaiting`; only when that matched, the subscription's
    /// `silent_since` unless already set — its row held until the commit
    /// either way, so a concurrent clear waits for the call — and one
    /// `activation` audit record
    /// with outcome `silent` naming the instance. `None` when the guard
    /// matched nothing — a report or a change came first — and nothing else
    /// was written.
    ///
    /// # Errors
    /// [`DomainError`] when a statement or the transaction fails; nothing of
    /// the call is kept.
    async fn silence(&self, row: &DueRow) -> Result<Option<Silenced>, DomainError>;

    /// The subscriptions whose `silent_since` is older than `age`, oldest
    /// silence first, through `idx_sub_silent`.
    ///
    /// # Errors
    /// [`DomainError`] when the read fails.
    async fn silent_past(&self, age: Duration) -> Result<Vec<InstanceKey>, DomainError>;

    /// Forget one silent instance in one transaction: its state, then its
    /// subscription, each statement conditional on its `silent_since` still
    /// being older than `age`. `true` when the subscription was deleted; a
    /// subscription a report or a registration cleared meanwhile is kept. No
    /// audit record is written or altered.
    ///
    /// # Errors
    /// [`DomainError`] when a statement or the transaction fails.
    async fn forget_silent(
        &self,
        instance: &InstanceKey,
        age: Duration,
    ) -> Result<bool, DomainError>;

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
    /// refreshed and the pass ran its forgetting and orphan sweep only.
    Idle,
    /// The activation types are not registered (refused or failed): logged,
    /// retried on the next tick; the pass ran its forgetting and orphan sweep
    /// only.
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

impl Tick {
    /// How the tick ended, as the `end` attribute of its span.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::NotRegistered => "not_registered",
            Self::Skipped => "skipped",
            Self::Complete { .. } => "complete",
            Self::BrokerLost => "broker_lost",
            Self::Stalled => "stalled",
            Self::Cancelled => "cancelled",
            Self::Failed => "failed",
        }
    }
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
    /// `K`: the accepted deliveries after which a due state is called silent.
    silent_after: u32,
    /// The pair at which this replica's last pass ended before its end — a
    /// stall, the broker lost, the stop — which the next pass of this replica
    /// starts after ([`in_pass_order`]); `None` once a pass ran to its end.
    resume_after: Mutex<Option<SubscriberTenant>>,
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
    /// `redeliver_interval` is `redeliver_interval_seconds`. The report timeout
    /// is the default `activation_report_timeout_seconds` until
    /// [`Self::with_report_timeout`] sets the configured one.
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
            silent_after: deliveries_within(
                Duration::from_secs(DEFAULT_REPORT_TIMEOUT_SECONDS),
                redeliver_interval,
            ),
            resume_after: Mutex::new(None),
        }
    }

    /// The report timeout, `activation_report_timeout_seconds`, converted once
    /// to `K = ceil(report_timeout / redeliver_interval)` accepted deliveries,
    /// which every pass uses.
    #[must_use]
    pub fn with_report_timeout(mut self, report_timeout: Duration) -> Self {
        self.silent_after = deliveries_within(report_timeout, self.redeliver_interval);
        self
    }

    /// The loop's interval: `redeliver_interval_seconds`.
    #[must_use]
    pub const fn interval(&self) -> Duration {
        self.redeliver_interval
    }

    /// `K`: the accepted deliveries after which a due state is called silent.
    #[must_use]
    pub const fn silent_after(&self) -> u32 {
        self.silent_after
    }

    /// Whether the activation types are registered: the end-of-request hook
    /// publishes no notification before.
    #[must_use]
    pub fn registered(&self) -> bool {
        self.gateway.registered()
    }

    /// The notification half of the end-of-request hook: per pair the request
    /// touched, in first-touch order, publish the keys of the pair's due
    /// states on the request's own session, so the skip rule is shared with
    /// its broadcasts. Never fails the request: whatever is not published
    /// stays `awaiting` for the delivery loop.
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
        // Every due key of the pair, not only those this request changed; the
        // rows the request wrote are due, their stamp cleared.
        let due = match self.store.due_of(pair, self.redeliver_interval).await {
            Ok(rows) => rows,
            Err(err) => {
                tracing::warn!(
                    subscriber = %pair.subscriber,
                    tenant = %pair.tenant,
                    %err,
                    "the due states of a pair could not be read; the delivery loop publishes them"
                );
                return true;
            }
        };
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-publish:p1:inst-ca-publish-6
        if due.is_empty() {
            return true;
        }
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-publish:p1:inst-ca-publish-7
        let built =
            self.gateway
                .notifications(pair, &keys_once(&due), trace_parent.map(str::to_owned));
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-publish:p1:inst-ca-publish-7
        for notification in built {
            let bundled = bundled(&due, &notification.changes);
            match request.publish_notification(notification.event).await {
                // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-publish:p1:inst-ca-publish-10
                NotificationResult::Accepted => {
                    self.stamp(pair, &bundled).await;
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

    /// Stamp and count what the broker accepted, on the due rows it bundled
    /// only. A failed stamp only makes the loop re-publish the states at its
    /// next tick, the delivery uncounted: logged, never raised.
    async fn stamp(&self, pair: &SubscriberTenant, rows: &[DueRow]) {
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-deliver:p1:inst-ca-deliver-28
        if let Err(err) = self.store.stamp_due(rows).await {
            tracing::warn!(
                subscriber = %pair.subscriber,
                tenant = %pair.tenant,
                %err,
                "a published notification could not be stamped; it is re-published at the next due tick"
            );
        }
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-deliver:p1:inst-ca-deliver-28
    }

    /// One tick of the delivery loop, after the interval: refresh the gauges,
    /// then run one pass whether or not a broker resolves — its forgetting and
    /// orphan sweep on every tick, its call of silence and its publishing only
    /// with a broker, the root tenant's id and the registered activation
    /// types.
    ///
    /// The tick runs inside an `activation.deliver` span, a root of its own,
    /// whose only attribute is the tick's `end`; each pair it re-publishes
    /// runs in a `change_notification.republish` child span, which its
    /// notifications carry as `trace_parent`.
    pub async fn tick(&self, cancel: &CancellationToken) -> Tick {
        let span = tracing::info_span!(
            parent: None,
            "activation.deliver",
            end = tracing::field::Empty,
        );
        let tick = self.ticked(cancel).instrument(span.clone()).await;
        span.record("end", tick.as_str());
        tick
    }

    async fn ticked(&self, cancel: &CancellationToken) -> Tick {
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-lifecycle:p1:inst-ca-lifecycle-8
        self.refresh_gauges().await;
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-lifecycle:p1:inst-ca-lifecycle-8
        let ready = self.readiness(cancel).await;
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-lifecycle:p1:inst-ca-lifecycle-13
        let outcome = self.deliver(ready.is_ok(), cancel).await;
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-lifecycle:p1:inst-ca-lifecycle-13
        match ready {
            Ok(()) => {
                // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-lifecycle:p1:inst-ca-lifecycle-14
                if outcome == Tick::BrokerLost {
                    tracing::warn!(
                        "the broker was lost during a delivery pass; every awaiting state is kept"
                    );
                }
                // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-lifecycle:p1:inst-ca-lifecycle-14
                outcome
            }
            // The pass ran its forgetting and sweep only: the tick reports why
            // it went no further, unless that part failed or was stopped.
            Err(not_ready) => match outcome {
                Tick::Failed | Tick::Cancelled => outcome,
                _ => not_ready,
            },
        }
    }

    /// Whether the pass's broker-dependent part may run: `Err` with the tick's
    /// end when no broker resolves, the root tenant's id is not yet known, or
    /// the activation types are not — and could not now be — registered.
    async fn readiness(&self, cancel: &CancellationToken) -> Result<(), Tick> {
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-lifecycle:p1:inst-ca-lifecycle-9
        if !self.gateway.broker_present() {
            tracing::debug!("no broker resolves; the delivery pass forgets and sweeps only");
            return Err(Tick::Idle);
        }
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-lifecycle:p1:inst-ca-lifecycle-9
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-lifecycle:p1:inst-ca-lifecycle-10
        if self.gateway.root_tenant().is_none() {
            tracing::debug!(
                "the root tenant's id is not yet known; the delivery pass forgets and sweeps only"
            );
            return Err(Tick::Idle);
        }
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-lifecycle:p1:inst-ca-lifecycle-10
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-lifecycle:p1:inst-ca-lifecycle-11
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-lifecycle:p1:inst-ca-lifecycle-12
        if !self.gateway.registered() && !self.gateway.register(cancel).await {
            // The client logged the reason at `warn`; the next tick registers
            // again, and publishes and the call of silence stay off meanwhile.
            return Err(Tick::NotRegistered);
        }
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-lifecycle:p1:inst-ca-lifecycle-12
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-lifecycle:p1:inst-ca-lifecycle-11
        Ok(())
    }

    /// Set the four gauges from the counts, whether or not a broker resolves.
    async fn refresh_gauges(&self) {
        match self.store.counts().await {
            Ok(counts) => {
                self.metrics.subscriptions(counts.subscriptions);
                self.metrics.awaiting(counts.awaiting);
                self.metrics.silent(counts.silent);
                self.metrics
                    .silent_subscriptions(counts.silent_subscriptions);
            }
            Err(err) => tracing::warn!(%err, "the activation gauges could not be refreshed"),
        }
    }

    /// One pass of the deliver flow, under the scan lock; `broker` tells it
    /// whether its broker-dependent part may run.
    async fn deliver(&self, broker: bool, cancel: &CancellationToken) -> Tick {
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
        let outcome = self.pass(broker, cancel).await;
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-deliver:p1:inst-ca-deliver-14
        held.release().await;
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-deliver:p1:inst-ca-deliver-14
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-deliver:p1:inst-ca-deliver-15
        outcome
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-deliver:p1:inst-ca-deliver-15
    }

    /// The body of a pass: the forgetting and the orphan sweep on every tick;
    /// with a broker, the call of silence and the due states per pair.
    async fn pass(&self, broker: bool, cancel: &CancellationToken) -> Tick {
        if let Err(ended) = self.forget(cancel).await {
            return ended;
        }
        if !self.sweep().await {
            return Tick::Failed;
        }
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-deliver:p1:inst-ca-deliver-20
        if !broker {
            // No publication can be accepted, so no delivery is counted and
            // no instance is called silent.
            return Tick::Idle;
        }
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-deliver:p1:inst-ca-deliver-20
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-deliver:p1:inst-ca-deliver-5
        // The tick's age, not the exact interval: what the previous tick
        // published is due at this one.
        let due = match self.store.due(tick_age(self.redeliver_interval)).await {
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
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-deliver:p1:inst-ca-deliver-21
        // @cpt-begin:cpt-cf-settings-service-state-consumer-activation-row:p1:inst-ca-row-7
        let (silenced, due): (Vec<DueRow>, Vec<DueRow>) = due
            .into_iter()
            .partition(|row| row.deliveries >= self.silent_after);
        // @cpt-end:cpt-cf-settings-service-state-consumer-activation-row:p1:inst-ca-row-7
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-deliver:p1:inst-ca-deliver-21
        if let Err(ended) = self.call_silence(&silenced, cancel).await {
            return ended;
        }
        let mut published = 0;
        let mut refusals = Refusals::default();
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-deliver:p1:inst-ca-deliver-6
        let resume_after = self.resume_point();
        for (pair, rows) in in_pass_order(group_by_pair(due), resume_after.as_ref()) {
            match self
                .republish_pair(session.as_ref(), &pair, &rows, &mut refusals, cancel)
                .instrument(republish_span(&pair))
                .await
            {
                Ok(accepted) => published += accepted,
                Err(ended) => {
                    // The pairs after this one were not attempted: the next
                    // pass starts with them, so a pair the broker keeps
                    // refusing, or one whose publish keeps timing out, ends
                    // each pass after the others had their turn.
                    self.set_resume_point(Some(pair));
                    return ended;
                }
            }
        }
        self.set_resume_point(None);
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-deliver:p1:inst-ca-deliver-6
        Tick::Complete { published }
    }

    /// The pair this replica's last pass ended early at, if it did.
    fn resume_point(&self) -> Option<SubscriberTenant> {
        self.resume_after
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Remember where this pass ended: `None` when it ran to its end.
    fn set_resume_point(&self, pair: Option<SubscriberTenant>) {
        *self
            .resume_after
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = pair;
    }

    /// The forgetting: each subscription silent for longer than
    /// [`SILENT_FORGET_SECONDS`] goes, with its states, one transaction per
    /// subscription, each statement conditional on its `silent_since`. Takes
    /// no key's mutation lock and writes no audit record. `Err` with the
    /// tick's end when a statement failed or the stop fired.
    async fn forget(&self, cancel: &CancellationToken) -> Result<(), Tick> {
        let silent = self
            .store
            .silent_past(SILENT_FORGET)
            .await
            .map_err(|err| abandoned(&err, "the silent subscriptions could not be read"))?;
        for instance in silent {
            stopped(cancel)?;
            let forgotten = self
                .store
                .forget_silent(&instance, SILENT_FORGET)
                .await
                .map_err(|err| abandoned(&err, "a silent subscription could not be forgotten"))?;
            // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-deliver:p1:inst-ca-deliver-19
            if forgotten {
                self.metrics.forgotten();
            }
            forgetting_line(&instance, forgotten);
            // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-deliver:p1:inst-ca-deliver-19
        }
        Ok(())
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

    /// Call silent, one transaction each, the due states that had `K`
    /// accepted deliveries without a report; none of them is published, the
    /// call matched or not. Takes no key's mutation lock. `Err` with the
    /// tick's end when a call failed or the stop fired.
    async fn call_silence(&self, rows: &[DueRow], cancel: &CancellationToken) -> Result<(), Tick> {
        for row in rows {
            stopped(cancel)?;
            let called = self
                .store
                .silence(row)
                .await
                .map_err(|err| abandoned(&err, "a call of silence failed"))?;
            // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-deliver:p1:inst-ca-deliver-27
            // Counted after the commit, the transition as the call's
            // transaction judged it.
            if let Some(silenced) = called {
                self.metrics.silenced();
                if silenced.incomplete {
                    self.metrics.incomplete();
                }
            }
            silence_line(row, called.is_some());
            // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-deliver:p1:inst-ca-deliver-27
        }
        Ok(())
    }

    /// One pair's notifications of the pass: how many the broker accepted, or
    /// the tick's end when the broker is lost or the stop fired.
    async fn republish_pair(
        &self,
        session: &dyn BroadcastSink,
        pair: &SubscriberTenant,
        rows: &[DueRow],
        refusals: &mut Refusals,
        cancel: &CancellationToken,
    ) -> Result<usize, Tick> {
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-deliver:p1:inst-ca-deliver-7
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-deliver:p1:inst-ca-deliver-8
        // `trace_parent` from the current span: the pair's republish span in
        // the tick's trace. The publishing request's trace is not kept with
        // the state, so a re-publication links to the pass that sent it.
        let built = self.gateway.notifications(
            pair,
            &keys_once(rows),
            toolkit_trace_context::current_traceparent(),
        );
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-deliver:p1:inst-ca-deliver-8
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-deliver:p1:inst-ca-deliver-7
        let mut accepted = 0;
        for notification in built {
            let bundled = bundled(rows, &notification.changes);
            match self
                .republish(
                    session,
                    pair,
                    notification.event,
                    &bundled,
                    refusals,
                    cancel,
                )
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
        event: EventV1,
        bundled: &[DueRow],
        refusals: &mut Refusals,
        cancel: &CancellationToken,
    ) -> Sent {
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-deliver:p1:inst-ca-deliver-9
        let published = tokio::select! {
            biased;
            () = cancel.cancelled() => None,
            out = session.publish(event) => Some(out),
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
                self.stamp(pair, bundled).await;
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
    /// else leaves the states due at the next tick. No delivery is counted.
    /// Only the first refusal of a pass is logged at `warn` — a missing
    /// `produce` grant refuses every pair alike — the rest at `debug`.
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

/// `Err(Tick::Cancelled)` once the stop fired: the pass is abandoned between
/// two of its transactions, every `awaiting` state kept.
/// The span one pair's re-publication runs in: identifiers only, never a key,
/// a `change_ref` or a value.
fn republish_span(pair: &SubscriberTenant) -> tracing::Span {
    tracing::info_span!(
        "change_notification.republish",
        subscriber = %pair.subscriber,
        tenant = %pair.tenant,
    )
}

fn stopped(cancel: &CancellationToken) -> Result<(), Tick> {
    if cancel.is_cancelled() {
        tracing::info!("the delivery pass was abandoned by the stop");
        return Err(Tick::Cancelled);
    }
    Ok(())
}

/// A statement of the pass failed: logged at `warn`, the pass abandoned.
fn abandoned(err: &DomainError, what: &str) -> Tick {
    tracing::warn!(%err, "{what}; the pass is abandoned");
    Tick::Failed
}

/// What one forgetting came to, in the log: identifiers only.
fn forgetting_line(instance: &InstanceKey, forgotten: bool) {
    if forgotten {
        tracing::info!(
            subscriber = %instance.subscriber,
            instance_id = %instance.instance_id,
            key = %instance.key,
            tenant = %instance.tenant,
            "a consumer instance silent for a day was forgotten with its states"
        );
    } else {
        tracing::debug!(
            subscriber = %instance.subscriber,
            instance_id = %instance.instance_id,
            "a silent subscription re-entered before it was forgotten; it is kept"
        );
    }
}

/// What one call of silence came to, in the log: identifiers only.
fn silence_line(row: &DueRow, called: bool) {
    if called {
        tracing::info!(
            subscriber = %row.pair.subscriber,
            instance_id = %row.instance_id,
            key = %row.change.key,
            tenant = %row.pair.tenant,
            change_ref = %row.change.change_ref,
            deliveries = row.deliveries,
            "a consumer instance was called silent"
        );
    } else {
        tracing::debug!(
            subscriber = %row.pair.subscriber,
            instance_id = %row.instance_id,
            "a call of silence lost to a report or a change; nothing was written"
        );
    }
}

/// The keys a pair's due rows put in its bundle: each key once, however many
/// instances hold a due row of it — every row of one subscriber at one key and
/// tenant names the same `change_ref` — in the rows' key order.
fn keys_once(rows: &[DueRow]) -> Vec<NotifiedChange> {
    // @cpt-begin:cpt-cf-settings-service-algo-consumer-activation-bundle:p1:inst-ca-bundle-6
    let mut seen = HashSet::new();
    rows.iter()
        .filter(|row| seen.insert(row.change.key.as_str()))
        .map(|row| row.change.clone())
        .collect()
    // @cpt-end:cpt-cf-settings-service-algo-consumer-activation-bundle:p1:inst-ca-bundle-6
}

/// The due rows one notification bundled: those whose key travels in it with
/// the row's own `change_ref`. A row of a key that travelled for another
/// `change_ref` is not among them, so it is neither stamped nor counted.
fn bundled(rows: &[DueRow], changes: &[NotifiedChange]) -> Vec<DueRow> {
    let carried: HashSet<(&str, Uuid)> = changes
        .iter()
        .map(|c| (c.key.as_str(), c.change_ref))
        .collect();
    rows.iter()
        .filter(|row| carried.contains(&(row.change.key.as_str(), row.change.change_ref)))
        .cloned()
        .collect()
}

/// The due rows grouped per pair, in the order the store returned them (by
/// pair, key and instance), each pair once. The pass takes the pairs in
/// [`in_pass_order`].
fn group_by_pair(due: Vec<DueRow>) -> Vec<(SubscriberTenant, Vec<DueRow>)> {
    let mut groups: Vec<(SubscriberTenant, Vec<DueRow>)> = Vec::new();
    for row in due {
        match groups.last_mut() {
            Some((pair, rows)) if *pair == row.pair => rows.push(row),
            _ => groups.push((row.pair.clone(), vec![row])),
        }
    }
    groups
}

/// The order in which a pass attempts its pairs: by subscriber, then tenant,
/// compared as strings — the notifier's own order, not the database
/// collation's, so the rotation below is a rotation of one total order —
/// starting after `resume_after`, the pair the replica's previous pass ended
/// at, and wrapping around to the pairs up to it.
///
/// A pass ends at the first timed-out publish or at the
/// [`CONSECUTIVE_REFUSALS`]th refusal in a row; the due read returns the
/// same pairs in the same order while their states stay unstamped, so a pass
/// always starting from the first pair would let a pair the broker keeps
/// refusing end every pass before the pairs after it. Starting after the end
/// of the previous pass gives each pair its turn: the pairs that ended it are
/// attempted last. The pairs' set changes between passes, so the start is the
/// first pair ordered after `resume_after`, whether or not that pair is still
/// due.
fn in_pass_order(
    mut groups: Vec<(SubscriberTenant, Vec<DueRow>)>,
    resume_after: Option<&SubscriberTenant>,
) -> Vec<(SubscriberTenant, Vec<DueRow>)> {
    fn order(pair: &SubscriberTenant) -> (&str, &str) {
        (pair.subscriber.as_str(), pair.tenant.as_str())
    }
    groups.sort_by(|(a, _), (b, _)| order(a).cmp(&order(b)));
    if let Some(after) = resume_after {
        let start = groups.partition_point(|(pair, _)| order(pair) <= order(after));
        groups.rotate_left(start);
    }
    groups
}

#[cfg(test)]
#[path = "notifier_tests.rs"]
mod notifier_tests;
