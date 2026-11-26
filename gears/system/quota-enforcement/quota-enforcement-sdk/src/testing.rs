//! Test doubles for the plugin contract. Enabled with the `test-util` feature.
//!
//! [`InMemoryStorage`] implements every method of
//! [`QuotaEnforcementStoragePluginV1`] with simple in-memory semantics, so the
//! gear's bootstrap and readiness paths run against a complete contract
//! (foundation `DoD`, "Workspace and Crate Skeletons").
//!
//! The double knows only the default rows of the configuration tables
//! (contention timeout, active-lease cap, idempotency retention): the contract
//! has no method that writes a per-tenant or per-metric row, so behaviour under
//! such a row is tested against the reference plugin.
//!
//! The double accepts an injected failure through `fail_with`, which makes
//! every later call return that error until `clear_failure`. Singleton
//! coordination is not a plugin contract of this gear: it is consumed from the
//! platform `cluster` gear (ADR-0006), so no coordination double exists here.

#![allow(
    clippy::expect_used,
    clippy::missing_panics_doc,
    reason = "test support: fixtures are built from constant, well-formed inputs"
)]

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use gts::GtsTypeId;
use parking_lot::Mutex;
use serde_json::Value;
use time::OffsetDateTime;
use toolkit_gts::gts_id;
use toolkit_security::{AccessScope, ScopeFilter, ScopeValue, pep_properties};
use uuid::Uuid;

use crate::bulk::{
    BulkCreateEnvelope, BulkCreated, BulkCreatedItem, BulkDeactivateEnvelope, BulkDeactivated,
    BulkDeactivatedItem, BulkRecord, BulkUpdateEnvelope, BulkUpdated, BulkUpdatedItem,
};
use crate::engine::{
    EngineError, EvaluationBudget, EvaluationContext, EvaluationFailure, EvaluationQuota,
    QuotaScopeTier,
};
use crate::models::{
    ActiveQuotaCounts, Actor, ApplicableQuotas, AppliedMutation, AttributionDigest, BatchRecord,
    BootstrapBundle, CapPatch, ChargeReturn, ConfigDefaults, DeactivateOutcome, DebitPlan,
    Decision, DecisionResult, EnforcementMode, EvaluatedDebit, EvaluatedLease, EventId,
    ExpiredLease, HeldAllocationCharges, HeldEntry, IdempotencyRecord, IdempotencyScope,
    IdempotencyWrite, LeaseHold, LeaseState, LeaseToken, MetricId, MutationResult,
    NotificationEvent, NotificationEventKind, NotificationScope, OperationType, PageRequest,
    PageResult, PartialIdempotencyWrite, PeriodId, PeriodType, PeriodWindow, PolicyDraft, PolicyId,
    PolicyScope, PolicyUpdate, PolicyVersion, PolicyVersionMeta, PolicyVersionState, Quota,
    QuotaDraft, QuotaFilter, QuotaId, QuotaPatch, QuotaSnapshot, QuotaSource, QuotaStatus,
    QuotaType, Retention, RollbackTarget, SubjectRef, TenantId, TransitionOutcome,
    ValidityWindowPatch,
};
use crate::notifications::{
    DeliveryOutcome, DispatchError, NotificationDeliveryHandle, NotificationDeliveryV1, QuotaEvent,
    QuotaNotificationSinkV1,
};
use crate::storage_plugin::{
    CONTRACT_MAJOR, EvaluatedBatch, EvaluatedMutation, QuotaEnforcementStoragePluginV1,
    StorageError,
};
use crate::thresholds::threshold_crossings;

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// A fixed tenant for tests.
#[must_use]
pub fn test_tenant() -> TenantId {
    TenantId::new(Uuid::from_u128(0x7e57_0000_0000_0000_0000_0000_0000_0001))
}

/// A metric type id derived from the quota metric base.
#[must_use]
pub fn test_metric() -> MetricId {
    MetricId::parse(gts_id!(
        "cf.core.quota.metric.v1~cf.genai.llm_gateway.ai_tokens_input.v1~"
    ))
    .expect("well-formed metric id")
}

/// A subject of a user subject type derived from the quota subject base.
#[must_use]
pub fn test_subject(subject_id: &str) -> SubjectRef {
    SubjectRef {
        subject_type: GtsTypeId::new(gts_id!("cf.core.quota.subj.v1~acme.core.quota.user.v1~")),
        subject_id: subject_id.to_owned(),
    }
}

/// A consumption Quota draft with a bounded cap.
#[must_use]
pub fn quota_draft(subject: SubjectRef, cap: Option<u64>) -> QuotaDraft {
    QuotaDraft {
        tenant_id: test_tenant(),
        subject,
        metric: test_metric(),
        quota_type: QuotaType::Allocation,
        period: None,
        enforcement_mode: EnforcementMode::Hard,
        cap,
        notification_thresholds: Vec::new(),
        validity_window: None,
        fail_open_hint: false,
        metadata: serde_json::Map::new(),
        source: QuotaSource::Licensing,
    }
}

/// A consumption Quota draft: per-period accounting with the given period,
/// cap, and notification thresholds.
#[must_use]
pub fn consumption_quota_draft(
    subject: SubjectRef,
    cap: Option<u64>,
    period: PeriodType,
    thresholds: Vec<u8>,
) -> QuotaDraft {
    QuotaDraft {
        quota_type: QuotaType::Consumption,
        period: Some(period),
        notification_thresholds: thresholds,
        ..quota_draft(subject, cap)
    }
}

// ---------------------------------------------------------------------------
// Storage double
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct LeaseRow {
    tenant_id: TenantId,
    metric: MetricId,
    /// The attribution digest the acquisition was admitted under, which the
    /// charge of its commit takes.
    authorized: AttributionDigest,
    /// What the acquisition asked for; the holds of every chain add up to it.
    reserved_amount: u64,
    holds: Vec<HeldRow>,
    state: LeaseState,
    expires_at: OffsetDateTime,
}

/// One hold, and whether its capacity has already been given back.
///
/// An expired lease is released the moment its TTL passes (I4), but its
/// capacity sits in the counter until someone returns it. Whoever touches the
/// counter first does that and marks the hold, so the sweeper that arrives
/// later moves nothing a second time.
#[derive(Clone)]
struct HeldRow {
    hold: LeaseHold,
    /// `(chain, position)` of the plan entry this hold applied.
    slot: (u32, u32),
    returned: bool,
}

/// One `(Quota, period)` counter row. Consumption Quotas accumulate here;
/// allocation Quotas have no periods and use `in_flight` instead.
#[derive(Clone)]
struct PeriodRow {
    quota_id: QuotaId,
    window: PeriodWindow,
    consumed: u64,
    /// Highest threshold emitted for this row. Reset by construction at every
    /// rollover, because the successor row is a new row (I13).
    marker: Option<u8>,
    settled: bool,
}

/// The public view of a period row, for assertions about materialization and
/// settlement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeriodRowView {
    /// Identifier of the row.
    pub period_id: PeriodId,
    /// Bounds of the period.
    pub window: PeriodWindow,
    /// Counter value.
    pub consumed: u64,
    /// Highest threshold emitted for this row.
    pub marker: Option<u8>,
    /// Whether the rollover event has been emitted for it.
    pub settled: bool,
}

/// One counter movement of a committed debit, kept so a return can give back
/// exactly what was applied, against the period it was attributed to (I5).
#[derive(Clone)]
struct AppliedEntry {
    quota_id: QuotaId,
    period_id: Option<PeriodId>,
    amount: u64,
    /// `(chain, position)` of the plan entry this movement applied.
    slot: (u32, u32),
}

/// The charge of an allowed debit or a lease commit (ADR-0011): the record of
/// that operation and what each of its entries charged and has had returned.
#[derive(Clone)]
struct ChargeRow {
    /// Names this charge, apart from a later one under the same key.
    id: Uuid,
    /// Its `expires_at` is the end of the return window.
    record: IdempotencyRecord,
    authorized: AttributionDigest,
    metric: MetricId,
    /// Kept at least until then by the last return or rollback.
    kept_until: Option<OffsetDateTime>,
    entries: Vec<ChargeEntry>,
}

/// The record of one applied return, deleted with its charge.
#[derive(Clone)]
struct ReturnRow {
    charge_id: Uuid,
    charge: IdempotencyScope,
    amount: u64,
}

#[derive(Clone)]
struct ChargeEntry {
    charged: AppliedEntry,
    returned: u64,
}

#[derive(Clone)]
struct LogEntry {
    at: OffsetDateTime,
    actor: Actor,
    operation: &'static str,
    quota_ids: Vec<QuotaId>,
}

#[derive(Clone, Default)]
struct StorageState {
    installed_major: Option<u32>,
    /// The largest page `read_quotas` serves, when it serves less than asked.
    page_cap: Option<u32>,
    /// `read_quotas` answers a first page with no items and a cursor.
    empty_first_quota_page: bool,
    bootstrapped: Option<BootstrapBundle>,
    bootstrap_calls: usize,
    defaults: Option<ConfigDefaults>,
    quotas: BTreeMap<QuotaId, Quota>,
    /// In-flight amounts of allocation Quotas.
    in_flight: BTreeMap<QuotaId, u64>,
    /// Highest threshold emitted per allocation Quota, which has no period row
    /// to hold the marker.
    allocation_markers: BTreeMap<QuotaId, u8>,
    /// Every period row ever materialized, settled ones included.
    periods: BTreeMap<PeriodId, PeriodRow>,
    /// The row each consumption Quota is currently accumulating into.
    current_period: BTreeMap<QuotaId, PeriodId>,
    leases: BTreeMap<LeaseToken, LeaseRow>,
    idempotency: HashMap<IdempotencyScope, IdempotencyRecord>,
    /// Charges, keyed by the scope of their debit or commit.
    charges: HashMap<IdempotencyScope, ChargeRow>,
    /// Return records, keyed by the scope of the return's key.
    returns: HashMap<IdempotencyScope, ReturnRow>,
    policies: BTreeMap<PolicyId, Vec<PolicyVersion>>,
    events: Vec<NotificationEvent>,
    policy_audit: Vec<PolicyTransitionAudit>,
    log: Vec<LogEntry>,
    /// Every `AccessScope` a scoped call received, in call order.
    scopes: Vec<AccessScope>,
    failure: Option<StorageError>,
    /// Test-controlled clock. `None` reads the wall clock.
    now_override: Option<OffsetDateTime>,
    /// When set, [`QuotaEnforcementStoragePluginV1::lookup_idempotency`]
    /// answers `None` even for a record that exists.
    hide_lookups: bool,
    /// How many of the next lookups answer `None`.
    hidden_lookups: usize,
}

/// What one atomic envelope decided: the policy its items selected, their
/// decisions in submission order, and whether the batch committed. A batch that
/// did not commit leaves the counters exactly as it found them.
struct BatchRun {
    /// The policy each item selected.
    policies: Vec<PolicyVersion>,
    decisions: Vec<Decision>,
    /// The movements of each item.
    entries: Vec<Vec<AppliedEntry>>,
    committed: bool,
}

/// Committed transition audit, separate from immutable version creation fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyTransitionAudit {
    /// Stable policy identifier.
    pub policy_id: PolicyId,
    /// Target version of the transition.
    pub version: u32,
    /// Transition kind.
    pub kind: &'static str,
    /// The actor of the transition.
    pub actor: String,
    /// Transition-specific operator comment.
    pub comment: Option<String>,
}

/// Complete in-memory [`QuotaEnforcementStoragePluginV1`].
pub struct InMemoryStorage {
    state: Mutex<StorageState>,
    /// The notification pipeline, apart from the transactional state: it is
    /// not rolled back with a mutation.
    delivery: Arc<Mutex<DeliveryState>>,
    /// Held for a whole drain, like the real pipeline's partition lease: one
    /// drain at a time, and a stop waits for the delivery in flight.
    drain: Arc<tokio::sync::Mutex<()>>,
}

/// The double's notification pipeline: the dispatcher's callback while
/// started, how far delivery has come through the committed events, the
/// failed attempts of the head event, and what was dead-lettered.
#[derive(Default)]
struct DeliveryState {
    callback: Option<Arc<dyn NotificationDeliveryV1>>,
    started: bool,
    next: usize,
    attempts: u16,
    dead_letters: Vec<(NotificationEvent, String)>,
}

/// What one [`InMemoryStorage::drain_notifications`] call did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DrainReport {
    /// Events acknowledged.
    pub delivered: usize,
    /// Events dead-lettered.
    pub rejected: usize,
    /// Whether the head event asked to be retried; draining stopped there.
    pub retry_pending: bool,
}

/// The double's pipeline handle: stopping it drops the callback, then waits
/// for a drain in progress to finish its delivery in flight.
struct DeliveryHandle {
    delivery: Arc<Mutex<DeliveryState>>,
    drain: Arc<tokio::sync::Mutex<()>>,
}

#[async_trait]
impl NotificationDeliveryHandle for DeliveryHandle {
    async fn stop(self: Box<Self>) {
        // A drain checks the callback before each event, so it ends after the
        // one in flight; taking its lock waits for that.
        self.delivery.lock().callback = None;
        let _drained = self.drain.lock().await;
    }
}

/// A notification sink that records what it received and answers from a
/// script: each call takes the next scripted outcome, and `Ok` once the
/// script is spent.
pub struct RecordingSink {
    id: String,
    script: Mutex<std::collections::VecDeque<Result<(), DispatchError>>>,
    received: Mutex<Vec<QuotaEvent>>,
}

impl RecordingSink {
    /// A sink that takes every event.
    #[must_use]
    pub fn new(id: &str) -> Self {
        Self {
            id: id.to_owned(),
            script: Mutex::new(std::collections::VecDeque::new()),
            received: Mutex::new(Vec::new()),
        }
    }

    /// A sink that answers `outcomes` in order, then takes every event.
    #[must_use]
    pub fn answering(id: &str, outcomes: Vec<Result<(), DispatchError>>) -> Self {
        let sink = Self::new(id);
        *sink.script.lock() = outcomes.into();
        sink
    }

    /// Every event received so far.
    #[must_use]
    pub fn received(&self) -> Vec<QuotaEvent> {
        self.received.lock().clone()
    }
}

#[async_trait]
impl QuotaNotificationSinkV1 for RecordingSink {
    fn id(&self) -> &str {
        &self.id
    }

    async fn dispatch(&self, event: QuotaEvent) -> Result<(), DispatchError> {
        self.received.lock().push(event);
        self.script.lock().pop_front().unwrap_or(Ok(()))
    }
}

impl Default for InMemoryStorage {
    fn default() -> Self {
        Self::new()
    }
}

/// The Quota value one scope filter constrains.
#[derive(Debug, Clone, Copy)]
enum ScopedValue {
    /// The owning tenant (`owner_tenant_id`).
    Tenant,
    /// The Quota id (`id`).
    Quota,
}

impl ScopedValue {
    fn of(self, quota: &Quota) -> ScopeValue {
        match self {
            Self::Tenant => ScopeValue::Uuid(quota.tenant_id.as_uuid()),
            Self::Quota => ScopeValue::Uuid(quota.id.as_uuid()),
        }
    }
}

/// A scope the double has checked it can evaluate: allow-all, deny-all, and
/// equality or membership filters on the owner tenant and the Quota id, as an
/// OR of constraints and an AND of their filters.
///
/// It is built before any row is read, so an unsupported scope is refused
/// whether or not a row would have been checked against it: no test passes
/// by skipping a scope the double cannot evaluate, on an empty store or a page
/// past the last match included. Hierarchy-aware scopes are exercised against
/// the SQL plugin.
#[derive(Debug)]
struct InMemoryScope<'a> {
    unconstrained: bool,
    constraints: Vec<Vec<(ScopedValue, &'a ScopeFilter)>>,
}

impl<'a> InMemoryScope<'a> {
    /// Check every filter of `scope` once.
    ///
    /// # Errors
    ///
    /// [`StorageError::Internal`] for a hierarchical filter or a property
    /// other than the owner tenant and the Quota id.
    fn of(scope: &'a AccessScope) -> Result<Self, StorageError> {
        let mut constraints = Vec::with_capacity(scope.constraints().len());
        for constraint in scope.constraints() {
            let mut filters = Vec::with_capacity(constraint.filters().len());
            for filter in constraint.filters() {
                if !filter.is_representable_in_memory() {
                    return Err(StorageError::Internal(format!(
                        "the in-memory double cannot evaluate a hierarchical scope filter on `{}`",
                        filter.property()
                    )));
                }
                let value = match filter.property() {
                    pep_properties::OWNER_TENANT_ID => ScopedValue::Tenant,
                    pep_properties::RESOURCE_ID => ScopedValue::Quota,
                    other => {
                        return Err(StorageError::Internal(format!(
                            "the in-memory double cannot evaluate scope property `{other}`"
                        )));
                    }
                };
                filters.push((value, filter));
            }
            constraints.push(filters);
        }
        Ok(Self {
            unconstrained: scope.is_unconstrained(),
            constraints,
        })
    }

    /// Whether the scope admits `quota`.
    fn admits(&self, quota: &Quota) -> bool {
        self.unconstrained
            || self.constraints.iter().any(|filters| {
                filters
                    .iter()
                    .all(|(value, filter)| filter.values().contains(&value.of(quota)))
            })
    }
}

impl InMemoryStorage {
    /// Inspect committed policy transitions in tests.
    #[must_use]
    pub fn policy_audit(&self) -> Vec<PolicyTransitionAudit> {
        self.state.lock().policy_audit.clone()
    }

    /// A backend whose installed schema major equals [`CONTRACT_MAJOR`].
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: Mutex::new(StorageState {
                installed_major: Some(CONTRACT_MAJOR),
                ..StorageState::default()
            }),
            delivery: Arc::new(Mutex::new(DeliveryState::default())),
            drain: Arc::default(),
        }
    }

    /// The same storage after a process restart: a new plugin instance over
    /// what this one committed, delivery included as far as it came, with no
    /// pipeline started. The two diverge from here on.
    #[must_use]
    pub fn restarted(&self) -> Self {
        let delivery = self.delivery.lock();
        Self {
            state: Mutex::new(self.state.lock().clone()),
            delivery: Arc::new(Mutex::new(DeliveryState {
                callback: None,
                started: false,
                next: delivery.next,
                attempts: delivery.attempts,
                dead_letters: delivery.dead_letters.clone(),
            })),
            drain: Arc::default(),
        }
    }

    /// Hand every committed event not yet delivered to the started pipeline's
    /// callback, in order, as the real pipeline's processor would: `Delivered`
    /// moves on, `Reject` dead-letters the event and moves on, `Retry` counts
    /// a failed attempt and stops at that event. Does nothing before
    /// [`QuotaEnforcementStoragePluginV1::start_notification_delivery`] or after
    /// the handle stopped. Concurrent drains run one after the other, so each
    /// event is delivered once, in order.
    pub async fn drain_notifications(&self) -> DrainReport {
        let _draining = self.drain.lock().await;
        let mut report = DrainReport::default();
        loop {
            let (callback, event, attempts) = {
                let delivery = self.delivery.lock();
                let Some(callback) = delivery.callback.clone() else {
                    return report;
                };
                let events = &self.state.lock().events;
                let Some(event) = events.get(delivery.next).cloned() else {
                    return report;
                };
                (callback, event, delivery.attempts)
            };
            let outcome = callback
                .deliver(event.clone(), attempts, Duration::from_secs(28))
                .await;
            let mut delivery = self.delivery.lock();
            match outcome {
                DeliveryOutcome::Delivered => {
                    delivery.next += 1;
                    delivery.attempts = 0;
                    report.delivered += 1;
                }
                DeliveryOutcome::Reject(reason) => {
                    delivery.dead_letters.push((event, reason));
                    delivery.next += 1;
                    delivery.attempts = 0;
                    report.rejected += 1;
                }
                DeliveryOutcome::Retry => {
                    delivery.attempts = delivery.attempts.saturating_add(1);
                    report.retry_pending = true;
                    return report;
                }
            }
        }
    }

    /// Events the pipeline dead-lettered, with the reason.
    #[must_use]
    pub fn dead_letters(&self) -> Vec<(NotificationEvent, String)> {
        self.delivery.lock().dead_letters.clone()
    }

    /// A backend that reports another installed schema major (I12 tests).
    #[must_use]
    pub fn with_installed_schema_major(major: u32) -> Self {
        let this = Self::new();
        this.state.lock().installed_major = Some(major);
        this
    }

    /// Serve at most `max` Quotas per `read_quotas` page, as a plugin with a
    /// smaller page than the caller asked for may.
    pub fn cap_pages(&self, max: u32) {
        self.state.lock().page_cap = Some(max);
    }

    /// Answer the first `read_quotas` page with no items and a cursor to the
    /// rest, as a plugin that filters after paging may.
    pub fn empty_first_quota_page(&self) {
        self.state.lock().empty_first_quota_page = true;
    }

    /// Every later call fails with `err` until [`Self::clear_failure`].
    pub fn fail_with(&self, err: StorageError) {
        self.state.lock().failure = Some(err);
    }

    /// Stop the injected failure.
    pub fn clear_failure(&self) {
        self.state.lock().failure = None;
    }

    /// Number of `bootstrap` calls, failures included.
    #[must_use]
    pub fn bootstrap_calls(&self) -> usize {
        self.state.lock().bootstrap_calls
    }

    /// The bundle of the last successful `bootstrap`.
    #[must_use]
    pub fn bootstrapped_bundle(&self) -> Option<BootstrapBundle> {
        self.state.lock().bootstrapped.clone()
    }

    /// The seeded default rows, once `bootstrap` succeeded.
    #[must_use]
    pub fn seeded_defaults(&self) -> Option<ConfigDefaults> {
        self.state.lock().defaults
    }

    /// The actor of every operation-log entry, oldest first.
    #[must_use]
    pub fn operation_log_actors(&self) -> Vec<Actor> {
        self.state
            .lock()
            .log
            .iter()
            .map(|entry| entry.actor.clone())
            .collect()
    }

    /// Every `AccessScope` a scoped call received, in call order.
    #[must_use]
    pub fn scopes(&self) -> Vec<AccessScope> {
        self.state.lock().scopes.clone()
    }

    /// Every event enqueued so far, in order.
    #[must_use]
    pub fn events(&self) -> Vec<NotificationEvent> {
        self.state.lock().events.clone()
    }

    /// A stored Quota.
    #[must_use]
    pub fn quota(&self, id: QuotaId) -> Option<Quota> {
        self.state.lock().quotas.get(&id).cloned()
    }

    /// Consumed or in-flight amount of a Quota: the current period's counter
    /// for a consumption Quota, the in-flight amount otherwise.
    #[must_use]
    pub fn consumed(&self, id: QuotaId) -> u64 {
        let st = self.state.lock();
        Self::counter_value(&st, id)
    }

    /// Every period row of a Quota, oldest first, for assertions about
    /// materialization, settlement, and the threshold marker.
    #[must_use]
    pub fn period_rows(&self, id: QuotaId) -> Vec<PeriodRowView> {
        let st = self.state.lock();
        let mut rows: Vec<PeriodRowView> = st
            .periods
            .iter()
            .filter(|(_, row)| row.quota_id == id)
            .map(|(period_id, row)| PeriodRowView {
                period_id: *period_id,
                window: row.window,
                consumed: row.consumed,
                marker: row.marker,
                settled: row.settled,
            })
            .collect();
        rows.sort_by_key(|row| row.window.start);
        rows
    }

    /// Number of operation-log rows written so far.
    #[must_use]
    pub fn operation_log_len(&self) -> usize {
        self.state.lock().log.len()
    }

    /// The operations and Quotas the log recorded, in order.
    #[must_use]
    pub fn operation_log(&self) -> Vec<(&'static str, Vec<QuotaId>)> {
        self.state
            .lock()
            .log
            .iter()
            .map(|entry| (entry.operation, entry.quota_ids.clone()))
            .collect()
    }

    /// Make the read-only replay lookup miss while the records themselves stay
    /// in place.
    ///
    /// This is the race window a caller cannot otherwise reach: another writer
    /// commits the record between the caller's lookup and its transaction, so
    /// the pre-check misses and the mutating primitive reports `NoOp` from
    /// inside its own row locks.
    pub fn hide_idempotency_lookups(&self) {
        self.state.lock().hide_lookups = true;
    }

    /// Make only the next `count` replay lookups miss: the window in which a
    /// record is committed after one lookup and before the next.
    pub fn hide_next_idempotency_lookups(&self, count: usize) {
        self.state.lock().hidden_lookups = count;
    }

    /// Drive the double's clock. `None` restores the wall clock.
    ///
    /// Period boundaries and retention deadlines are wall-clock instants, so
    /// tests move this rather than Tokio's paused time.
    pub fn set_now(&self, now: Option<OffsetDateTime>) {
        self.state.lock().now_override = now;
    }

    /// State of a lease, if it exists.
    #[must_use]
    pub fn lease_state(&self, token: LeaseToken) -> Option<LeaseState> {
        self.state.lock().leases.get(&token).map(|l| l.state)
    }

    /// Move every active lease's expiry into the past (I4 tests).
    pub fn expire_leases(&self) {
        let past = OffsetDateTime::now_utc() - Duration::from_secs(1);
        for lease in self.state.lock().leases.values_mut() {
            lease.expires_at = past;
        }
    }

    fn check(st: &StorageState) -> Result<(), StorageError> {
        match &st.failure {
            Some(err) => Err(err.clone()),
            None => Ok(()),
        }
    }

    /// Run `work` against a staged copy of the whole state, publishing it only
    /// when the closure succeeds.
    ///
    /// A real backend rolls back counters, period rows, outbox events, log
    /// rows, and idempotency records together. Staging the entire state gives
    /// the double the same all-or-nothing behaviour without a restore list that
    /// could fall out of step with the collections it is meant to cover.
    fn transact<T>(
        &self,
        work: impl FnOnce(&mut StorageState) -> Result<T, StorageError>,
    ) -> Result<T, StorageError> {
        let mut committed = self.state.lock();
        Self::check(&committed)?;
        let mut staged = committed.clone();
        let outcome = work(&mut staged)?;
        *committed = staged;
        Ok(outcome)
    }

    /// The stored outcome of a bulk envelope under `write`, or the payload
    /// mismatch when its key carried other items.
    fn bulk_replayed<T: serde::de::DeserializeOwned>(
        st: &StorageState,
        write: &IdempotencyWrite,
    ) -> Result<Option<T>, StorageError> {
        let Some(blob) = Self::replayed(st, write)? else {
            return Ok(None);
        };
        serde_json::from_value::<BulkRecord<T>>(blob)
            .map(|record| Some(record.outcome))
            .map_err(|e| StorageError::Internal(e.to_string()))
    }

    /// Record a committed bulk outcome under its envelope key.
    fn remember_bulk(
        st: &mut StorageState,
        write: &IdempotencyWrite,
        outcome: &impl serde::Serialize,
    ) -> Result<(), StorageError> {
        let blob = Self::blob(&BulkRecord::new(outcome))?;
        Self::remember(st, write, blob, None);
        Ok(())
    }

    /// `id` names a Quota of `tenant_id`; the double keeps no scopes, so the
    /// envelope tenant is the visibility rule.
    fn visible(st: &StorageState, tenant_id: TenantId, id: QuotaId) -> Result<(), StorageError> {
        if st
            .quotas
            .get(&id)
            .is_none_or(|quota| quota.tenant_id != tenant_id)
        {
            return Err(StorageError::QuotaNotFound { id });
        }
        Ok(())
    }

    /// A replay answers only when every target is still visible; the first
    /// that is not is not found, at its item's index.
    fn replay_visible(
        st: &StorageState,
        tenant_id: TenantId,
        ids: &[QuotaId],
    ) -> Result<(), StorageError> {
        for (index, id) in ids.iter().enumerate() {
            Self::visible(st, tenant_id, *id).map_err(|error| error.at_item(index))?;
        }
        Ok(())
    }

    /// Insert a new Quota and enqueue its events, as `create_quota` does.
    fn create_in(
        st: &mut StorageState,
        actor: &Actor,
        draft: QuotaDraft,
        events: &[NotificationEvent],
    ) -> QuotaId {
        let now = OffsetDateTime::now_utc();
        let id = QuotaId::generate();
        st.quotas.insert(
            id,
            Quota {
                id,
                tenant_id: draft.tenant_id,
                subject: draft.subject,
                metric: draft.metric,
                quota_type: draft.quota_type,
                period: draft.period,
                enforcement_mode: draft.enforcement_mode,
                cap: draft.cap,
                notification_thresholds: draft.notification_thresholds,
                validity_window: draft.validity_window,
                fail_open_hint: draft.fail_open_hint,
                metadata: draft.metadata,
                source: draft.source,
                status: QuotaStatus::Active,
                record_version: 1,
                created_at: now,
                updated_at: now,
            },
        );
        let events: Vec<NotificationEvent> = events
            .iter()
            .cloned()
            .map(|mut event| {
                if event.quota_id.is_none() {
                    event.quota_id = Some(id);
                }
                event
            })
            .collect();
        Self::push_events(st, actor, &events);
        id
    }

    /// Apply a patch under the I6 and I14 guards, as `update_quota` does.
    fn update_in(
        st: &mut StorageState,
        actor: &Actor,
        quota_id: QuotaId,
        patch: QuotaPatch,
        events: &[NotificationEvent],
    ) -> Result<Quota, StorageError> {
        Self::active_quota(st, quota_id)?;
        let consumed = Self::counter_value(st, quota_id);
        let quota = st
            .quotas
            .get_mut(&quota_id)
            .ok_or(StorageError::QuotaNotFound { id: quota_id })?;
        // I6 and I14 are decided on the merged row before anything changes.
        let merged_cap = match patch.cap {
            Some(CapPatch::Bounded(new_cap)) => Some(new_cap),
            Some(CapPatch::Unbounded) => None,
            None => quota.cap,
        };
        if let Some(new_cap) = merged_cap
            && patch.cap.is_some()
            && new_cap < consumed
        {
            return Err(StorageError::CapBelowConsumed { new_cap, consumed });
        }
        let merged_thresholds_present = patch
            .notification_thresholds
            .as_ref()
            .map_or(!quota.notification_thresholds.is_empty(), |t| !t.is_empty());
        if merged_cap.is_none() && merged_thresholds_present {
            return Err(StorageError::ThresholdsRequireBoundedCap);
        }
        if patch.cap.is_some() {
            quota.cap = merged_cap;
        }
        if let Some(thresholds) = patch.notification_thresholds {
            quota.notification_thresholds = thresholds;
        }
        if let Some(window) = patch.validity_window {
            quota.validity_window = match window {
                ValidityWindowPatch::Clear => None,
                ValidityWindowPatch::Set(w) => Some(w),
            };
        }
        if let Some(metadata) = patch.metadata {
            quota.metadata = metadata;
        }
        if let Some(mode) = patch.enforcement_mode {
            quota.enforcement_mode = mode;
        }
        if let Some(hint) = patch.fail_open_hint {
            quota.fail_open_hint = hint;
        }
        quota.record_version += 1;
        quota.updated_at = OffsetDateTime::now_utc();
        let updated = quota.clone();
        Self::push_events(st, actor, events);
        Ok(updated)
    }

    /// Deactivate a Quota and resolve its live leases, as `deactivate_quota`
    /// does.
    fn deactivate_in(
        st: &mut StorageState,
        actor: &Actor,
        quota_id: QuotaId,
        events: &[NotificationEvent],
    ) -> Result<DeactivateOutcome, StorageError> {
        Self::active_quota(st, quota_id)?;
        let now = OffsetDateTime::now_utc();
        let quota = st
            .quotas
            .get_mut(&quota_id)
            .ok_or(StorageError::QuotaNotFound { id: quota_id })?;
        quota.status = QuotaStatus::Deactivated;
        quota.record_version += 1;
        quota.updated_at = now;
        // Resolve live leases and return their held capacity; expired leases
        // have already been released (I4) and are left to the sweeper.
        let mut resolved = Vec::new();
        let mut returned: Vec<(LeaseToken, TenantId, SubjectRef, LeaseHold)> = Vec::new();
        let subject = st.quotas.get(&quota_id).map(|quota| quota.subject.clone());
        for (token, lease) in &mut st.leases {
            if lease.state != LeaseState::Active
                || lease.expires_at <= now
                || !lease.holds.iter().any(|h| h.hold.quota_id == quota_id)
            {
                continue;
            }
            lease.state = LeaseState::ResolvedByDeactivation;
            resolved.push(*token);
            let tenant_id = lease.tenant_id;
            for held in &mut lease.holds {
                if held.returned {
                    continue;
                }
                held.returned = true;
                if let Some(subject) = subject.clone() {
                    returned.push((*token, tenant_id, subject, held.hold.clone()));
                }
            }
        }
        for (_, _, _, hold) in &returned {
            Self::lower_counter(
                st,
                &AppliedEntry {
                    quota_id: hold.quota_id,
                    period_id: hold.period_id,
                    amount: hold.held_amount,
                    slot: (0, 0),
                },
            );
        }
        // One event per resolved lease, built here because only the
        // transaction knows which leases it resolved (I11).
        let mut seen = BTreeSet::new();
        for (token, tenant_id, subject, hold) in &returned {
            if !seen.insert(*token) {
                continue;
            }
            let held: u64 = returned
                .iter()
                .filter(|(other, _, _, _)| other == token)
                .map(|(_, _, _, hold)| hold.held_amount)
                .sum();
            let _ = hold;
            let event = NotificationEvent {
                event_id: EventId::generate(),
                kind: NotificationEventKind::LeaseResolvedByDeactivation,
                actor: actor.clone(),
                scope: NotificationScope::Tenant {
                    tenant_id: *tenant_id,
                },
                quota_id: Some(quota_id),
                policy_id: None,
                subject: Some(subject.clone()),
                payload: serde_json::json!({
                    "lease_token": token,
                    "held_amount": held,
                    "quota_id": quota_id,
                }),
                emitted_at: now,
            };
            st.events.push(event);
        }
        Self::push_events(st, actor, events);
        Ok(DeactivateOutcome {
            resolved_leases: resolved,
        })
    }

    fn record_scope(&self, scope: &AccessScope) {
        self.state.lock().scopes.push(scope.clone());
    }

    /// The transaction clock: a test-set instant, or the wall clock.
    fn now(st: &StorageState) -> OffsetDateTime {
        st.now_override.unwrap_or_else(OffsetDateTime::now_utc)
    }

    /// Enqueue caller events and write the accepted mutation's operation-log
    /// row, as a real plugin does in the same transaction.
    fn push_events_for(
        st: &mut StorageState,
        actor: &Actor,
        events: &[NotificationEvent],
        operation: &'static str,
        quota_ids: Vec<QuotaId>,
    ) -> Vec<EventId> {
        st.events.extend_from_slice(events);
        let at = Self::now(st);
        st.log.push(LogEntry {
            at,
            actor: actor.clone(),
            operation,
            quota_ids,
        });
        events.iter().map(|e| e.event_id).collect()
    }

    fn push_events(
        st: &mut StorageState,
        actor: &Actor,
        events: &[NotificationEvent],
    ) -> Vec<EventId> {
        Self::push_events_for(st, actor, events, "mutation", Vec::new())
    }

    fn event(
        st: &StorageState,
        actor: &Actor,
        kind: NotificationEventKind,
        quota: &Quota,
        payload: Value,
    ) -> NotificationEvent {
        NotificationEvent {
            event_id: EventId::generate(),
            kind,
            actor: actor.clone(),
            scope: NotificationScope::Tenant {
                tenant_id: quota.tenant_id,
            },
            quota_id: Some(quota.id),
            policy_id: None,
            subject: Some(quota.subject.clone()),
            payload,
            emitted_at: Self::now(st),
        }
    }

    /// The row a consumption Quota accumulates into at `now`, materializing it
    /// when the Quota has none or its current row has elapsed.
    ///
    /// Materialization only creates the successor row. Settling the row it
    /// succeeds, and emitting that row's rollover event, is the caller's.
    fn ensure_current_row(
        st: &mut StorageState,
        quota_id: QuotaId,
        now: OffsetDateTime,
    ) -> PeriodId {
        let period_type = st
            .quotas
            .get(&quota_id)
            .and_then(|quota| quota.period)
            .unwrap_or(PeriodType::OneTime);
        if let Some(current) = st.current_period.get(&quota_id)
            && st
                .periods
                .get(current)
                .is_some_and(|row| row.window.contains(now))
        {
            return *current;
        }
        let period_id = PeriodId::generate();
        st.periods.insert(
            period_id,
            PeriodRow {
                quota_id,
                window: period_type.window_containing(now),
                consumed: 0,
                marker: None,
                settled: false,
            },
        );
        st.current_period.insert(quota_id, period_id);
        period_id
    }

    /// Settle every elapsed, unsettled row of `quota_id`, emitting one
    /// rollover event each. Only a mutation against the current period calls
    /// this, so a dry run never enqueues an event.
    fn settle_elapsed_rows(
        st: &mut StorageState,
        actor: &Actor,
        quota_id: QuotaId,
        now: OffsetDateTime,
    ) {
        let closing: Vec<PeriodId> = st
            .periods
            .iter()
            .filter(|(_, row)| {
                row.quota_id == quota_id && !row.settled && row.window.has_elapsed(now)
            })
            .map(|(id, _)| *id)
            .collect();
        for period_id in closing {
            let Some(row) = st.periods.get_mut(&period_id) else {
                continue;
            };
            row.settled = true;
            let (consumed, boundary) = (row.consumed, row.window.end);
            let Some(quota) = st.quotas.get(&quota_id).cloned() else {
                continue;
            };
            let event = Self::event(
                st,
                actor,
                NotificationEventKind::PeriodRollover,
                &quota,
                serde_json::json!({
                    "closing_period_id": period_id,
                    "closing_consumed": consumed,
                    "closing_cap": quota.cap,
                    "new_period_boundary": boundary,
                }),
            );
            st.events.push(event);
        }
    }

    /// The counter a Quota currently reads: the in-flight amount of an
    /// allocation Quota, or the consumed amount of the current period row,
    /// in both cases less the expired holds nobody has returned yet (I4).
    ///
    /// The stored value and the correction are read from one borrow of the
    /// state, so no concurrent return can be observed by half of this.
    fn counter_value(st: &StorageState, quota_id: QuotaId) -> u64 {
        let now = Self::now(st);
        let (stored, period_id) = if st
            .quotas
            .get(&quota_id)
            .is_some_and(|quota| quota.quota_type == QuotaType::Consumption)
        {
            let current = st.current_period.get(&quota_id).copied().filter(|id| {
                st.periods
                    .get(id)
                    .is_some_and(|row| !row.window.has_elapsed(now))
            });
            let stored = current
                .and_then(|id| st.periods.get(&id))
                .map_or(0, |row| row.consumed);
            (stored, current)
        } else {
            (st.in_flight.get(&quota_id).copied().unwrap_or(0), None)
        };
        stored.saturating_sub(Self::unreturned_expired(st, quota_id, period_id, now))
    }

    /// What expired, unreturned holds still occupy on one counter row.
    fn unreturned_expired(
        st: &StorageState,
        quota_id: QuotaId,
        period_id: Option<PeriodId>,
        now: OffsetDateTime,
    ) -> u64 {
        st.leases
            .values()
            .filter(|lease| lease.state == LeaseState::Active && lease.expires_at <= now)
            .flat_map(|lease| &lease.holds)
            .filter(|held| {
                !held.returned && held.hold.quota_id == quota_id && held.hold.period_id == period_id
            })
            .map(|held| held.hold.held_amount)
            .sum()
    }

    /// Give back every expired hold on `quota_id` that nobody has returned yet,
    /// each against the period it was acquired on (I5), and mark it returned.
    ///
    /// Every writer runs this before its own arithmetic, so the stored counter
    /// equals the logical one from there on and a later sweep moves nothing.
    fn return_expired_holds(st: &mut StorageState, quota_id: QuotaId, now: OffsetDateTime) {
        let mut returned = Vec::new();
        for lease in st.leases.values_mut() {
            if lease.state != LeaseState::Active || lease.expires_at > now {
                continue;
            }
            for held in &mut lease.holds {
                if held.returned || held.hold.quota_id != quota_id {
                    continue;
                }
                held.returned = true;
                returned.push(AppliedEntry {
                    quota_id,
                    period_id: held.hold.period_id,
                    amount: held.hold.held_amount,
                    slot: held.slot,
                });
            }
        }
        for entry in &returned {
            Self::lower_counter(st, entry);
        }
    }

    /// Raise a counter and emit the thresholds the move crossed.
    ///
    /// Emission is upward-only and silent inside the settlement window: a row
    /// past its boundary is being closed, and ADR-0004 keeps adjusted and
    /// threshold events out of that window.
    fn debit_counter(
        st: &mut StorageState,
        actor: &Actor,
        quota_id: QuotaId,
        amount: u64,
        now: OffsetDateTime,
    ) -> Result<AppliedEntry, StorageError> {
        let quota = Self::active_quota(st, quota_id)?.clone();
        // This writer holds the row, so it is the one that reconciles the
        // expired holds still sitting in it (I4) before adding its own.
        Self::return_expired_holds(st, quota_id, now);
        let consumption = quota.quota_type == QuotaType::Consumption;
        let period_id = consumption.then(|| Self::ensure_current_row(st, quota_id, now));
        let (pre, post, silent) = if let Some(period_id) = period_id {
            let row = st
                .periods
                .get_mut(&period_id)
                .ok_or_else(|| StorageError::Internal("period row vanished".to_owned()))?;
            let pre = row.consumed;
            let post = pre
                .checked_add(amount)
                .ok_or_else(|| StorageError::Internal("counter overflow".to_owned()))?;
            row.consumed = post;
            (pre, post, row.window.has_elapsed(now))
        } else {
            let counter = st.in_flight.entry(quota_id).or_insert(0);
            let pre = *counter;
            let post = pre
                .checked_add(amount)
                .ok_or_else(|| StorageError::Internal("counter overflow".to_owned()))?;
            *counter = post;
            (pre, post, false)
        };
        let marker = match period_id {
            Some(id) => st.periods.get(&id).and_then(|row| row.marker),
            None => st.allocation_markers.get(&quota_id).copied(),
        };
        if let Some(crossing) = threshold_crossings(
            quota_id,
            pre,
            post,
            quota.cap,
            &quota.notification_thresholds,
            marker,
        ) {
            let highest = crossing.highest_crossed_threshold;
            match period_id {
                Some(id) => {
                    if let Some(row) = st.periods.get_mut(&id) {
                        row.marker = Some(highest);
                    }
                }
                None => {
                    st.allocation_markers.insert(quota_id, highest);
                }
            }
            if !silent {
                let event = Self::event(
                    st,
                    actor,
                    NotificationEventKind::ThresholdCrossed,
                    &quota,
                    serde_json::json!({
                        "crossed_thresholds": crossing.crossed_thresholds,
                        "highest_crossed_threshold": highest,
                        "consumed": post,
                        "cap": quota.cap,
                    }),
                );
                st.events.push(event);
            }
        }
        Ok(AppliedEntry {
            quota_id,
            period_id,
            amount,
            slot: (0, 0),
        })
    }

    /// Lower a counter, flooring at zero. Downward moves never emit a
    /// threshold: the marker only advances, so a threshold crossed once stays
    /// crossed until the period rolls over.
    ///
    /// Like every writer this first returns the row's expired holds, so a
    /// credit cannot floor away capacity that a later sweep still owes back.
    fn credit_counter(st: &mut StorageState, entry: &AppliedEntry, now: OffsetDateTime) {
        Self::return_expired_holds(st, entry.quota_id, now);
        Self::lower_counter(st, entry);
    }

    /// The raw downward move, with no expired-hold reconciliation: used by
    /// [`Self::return_expired_holds`] itself and by the callers that have
    /// already run it.
    fn lower_counter(st: &mut StorageState, entry: &AppliedEntry) {
        if let Some(period_id) = entry.period_id {
            if let Some(row) = st.periods.get_mut(&period_id) {
                row.consumed = row.consumed.saturating_sub(entry.amount);
            }
        } else {
            let counter = st.in_flight.entry(entry.quota_id).or_insert(0);
            *counter = counter.saturating_sub(entry.amount);
        }
    }

    /// What an earlier transaction recorded under this key, if any. A replay
    /// carrying a different payload is I2, not a second mutation.
    fn replayed(
        st: &StorageState,
        write: &IdempotencyWrite,
    ) -> Result<Option<Value>, StorageError> {
        let Some(existing) = Self::stored(st, &write.scope) else {
            return Ok(None);
        };
        if existing.payload_hash != write.payload_hash {
            return Err(StorageError::IdempotencyPayloadMismatch);
        }
        Ok(Some(existing.decision_blob.clone()))
    }

    /// The record under `scope`: the live charge of a debit or commit, else
    /// the idempotency record.
    fn stored<'s>(st: &'s StorageState, scope: &IdempotencyScope) -> Option<&'s IdempotencyRecord> {
        Self::live_charge(st, scope)
            .map(|charge| &charge.record)
            .or_else(|| st.idempotency.get(scope))
    }

    /// The charge under `scope` unless it is deleted (ADR-0011): the replay
    /// window has passed since its creation and its last return or rollback,
    /// and no allocation entry holds an amount on an active Quota.
    fn live_charge<'s>(st: &'s StorageState, scope: &IdempotencyScope) -> Option<&'s ChargeRow> {
        st.charges
            .get(scope)
            .filter(|charge| Self::charge_live(st, charge, Self::now(st)))
    }

    /// Whether `scope` admits the Quotas of every entry of `charge`, as the SQL
    /// storage reads a charge and its entries under the caller's scope.
    fn charge_admitted(st: &StorageState, scope: &InMemoryScope<'_>, charge: &ChargeRow) -> bool {
        charge.entries.iter().all(|entry| {
            st.quotas
                .get(&entry.charged.quota_id)
                .is_some_and(|quota| scope.admits(quota))
        })
    }

    fn charge_live(st: &StorageState, charge: &ChargeRow, now: OffsetDateTime) -> bool {
        now < charge.record.expires_at
            || charge.kept_until.is_some_and(|kept| now < kept)
            || Self::holds_active_allocation(st, charge)
    }

    fn holds_active_allocation(st: &StorageState, charge: &ChargeRow) -> bool {
        charge.entries.iter().any(|entry| {
            entry.returned < entry.charged.amount
                && st.quotas.get(&entry.charged.quota_id).is_some_and(|quota| {
                    quota.quota_type == QuotaType::Allocation && quota.status == QuotaStatus::Active
                })
        })
    }

    /// Give back `amount` of the live charge under `scope` (ADR-0011): unwind
    /// it over the entries, grow each reached entry's returned amount, and
    /// lower the counters of the entries whose Quota takes a return. Returns
    /// the reached entries and the counters that moved.
    fn give_back(
        st: &mut StorageState,
        actor: &Actor,
        scope: &IdempotencyScope,
        amount: u64,
        operation: &'static str,
    ) -> Result<(DebitPlan, MutationResult), StorageError> {
        let now = Self::now(st);
        let charge = Self::live_charge(st, scope).cloned().ok_or_else(|| {
            StorageError::OperationNotFound {
                key: scope.key.clone(),
            }
        })?;
        let held: Vec<HeldEntry> = charge
            .entries
            .iter()
            .map(|entry| HeldEntry {
                chain: entry.charged.slot.0,
                position: entry.charged.slot.1,
                held: entry.charged.amount - entry.returned,
            })
            .collect();
        let remaining = Self::remaining(&held);
        let exceeds = StorageError::ReturnExceedsCharge {
            requested: amount,
            remaining,
        };
        if amount > remaining {
            return Err(exceeds);
        }
        let taken = crate::models::unwind(&held, amount).map_err(|_| exceeds)?;
        let can_take = |st: &StorageState, entry: &ChargeEntry| {
            let Some(quota) = st.quotas.get(&entry.charged.quota_id) else {
                return false;
            };
            match quota.quota_type {
                QuotaType::Allocation => true,
                QuotaType::Consumption => {
                    now < charge.record.expires_at
                        && entry
                            .charged
                            .period_id
                            .and_then(|id| st.periods.get(&id))
                            .is_some_and(|row| !row.settled)
                }
                QuotaType::Rate => false,
            }
        };
        if !charge
            .entries
            .iter()
            .zip(&taken)
            .any(|(entry, take)| *take > 0 && can_take(st, entry))
        {
            return Err(StorageError::ReturnNotApplicable);
        }
        let mut reached = Vec::new();
        let mut moved = Vec::new();
        for (entry, take) in charge.entries.iter().zip(&taken) {
            if *take == 0 {
                continue;
            }
            let part = AppliedEntry {
                amount: *take,
                ..entry.charged.clone()
            };
            if can_take(st, entry) {
                Self::credit_counter(st, &part, now);
                moved.push(part.clone());
            }
            reached.push(part);
        }
        let retention = st.defaults.map_or(86_400, |d| d.idempotency_retention_secs);
        if let Some(stored) = st.charges.get_mut(scope) {
            for (entry, take) in stored.entries.iter_mut().zip(&taken) {
                entry.returned += take;
            }
            stored.kept_until = Some(now + Duration::from_secs(retention));
        }
        let mut result = Self::counters_of(st, &moved);
        if let Some(quota) = moved
            .first()
            .and_then(|entry| st.quotas.get(&entry.quota_id))
            .cloned()
        {
            // Emitted even inside the settlement window: ADR-0004 silences
            // adjusted and threshold events there, not this one.
            let event = Self::event(
                st,
                actor,
                NotificationEventKind::QuotaRollbackApplied,
                &quota,
                serde_json::json!({
                    "operation": scope.operation_type.as_str(),
                    "idempotency_key": scope.key,
                    "returned_amount": amount,
                    "remaining_amount": remaining - amount,
                    "affected_quotas": moved.iter().map(|entry| entry.quota_id).collect::<Vec<_>>(),
                    "principal": actor.to_string(),
                }),
            );
            result.event_ids.push(event.event_id);
            st.events.push(event);
        }
        Self::push_events_for(
            st,
            actor,
            &[],
            operation,
            moved.iter().map(|entry| entry.quota_id).collect(),
        );
        Ok((Self::plan_of_entries(&reached), result))
    }

    /// What remains of a charge: the least any chain still holds.
    fn remaining(held: &[HeldEntry]) -> u64 {
        let mut chains: BTreeMap<u32, u64> = BTreeMap::new();
        for entry in held {
            *chains.entry(entry.chain).or_default() += entry.held;
        }
        chains.into_values().min().unwrap_or(0)
    }

    /// The record of what this transaction decided, attributed to the policy
    /// it selected, kept for the replay window.
    fn record_of(
        st: &StorageState,
        write: &IdempotencyWrite,
        blob: Value,
        policy: Option<&PolicyVersion>,
    ) -> IdempotencyRecord {
        let retention = st.defaults.map_or(86_400, |d| d.idempotency_retention_secs);
        let now = Self::now(st);
        IdempotencyRecord {
            scope: write.scope.clone(),
            payload_hash: write.payload_hash,
            decision_blob: blob,
            engine_id: policy.map(|p| p.engine_id.clone()),
            policy_id: policy.map(|p| p.policy_id.clone()),
            policy_version: policy.map(|p| p.version),
            created_at: now,
            expires_at: now + Duration::from_secs(retention),
        }
    }

    /// Record what this transaction decided. A primitive that evaluates
    /// nothing records no policy.
    fn remember(
        st: &mut StorageState,
        write: &IdempotencyWrite,
        blob: Value,
        policy: Option<&PolicyVersion>,
    ) -> OffsetDateTime {
        let record = Self::record_of(st, write, blob, policy);
        let expires_at = record.expires_at;
        st.idempotency.insert(write.scope.clone(), record);
        expires_at
    }

    /// Write the charge of an allowed debit or a lease commit, which is its
    /// record. Returns the end of its return window.
    fn charge(
        st: &mut StorageState,
        write: &IdempotencyWrite,
        blob: Value,
        policy: Option<&PolicyVersion>,
        authorized: AttributionDigest,
        metric: &MetricId,
        entries: &[AppliedEntry],
    ) -> OffsetDateTime {
        let record = Self::record_of(st, write, blob, policy);
        let expires_at = record.expires_at;
        st.charges.insert(
            write.scope.clone(),
            ChargeRow {
                id: Uuid::now_v7(),
                record,
                authorized,
                metric: metric.clone(),
                kept_until: None,
                entries: entries
                    .iter()
                    .map(|entry| ChargeEntry {
                        charged: entry.clone(),
                        returned: 0,
                    })
                    .collect(),
            },
        );
        expires_at
    }

    /// Whether `scope` of a batch item names a record of a single debit: a
    /// `debit` charge or the record of a denied debit.
    fn item_key_taken(st: &StorageState, scope: &IdempotencyScope) -> bool {
        Self::stored(st, scope).is_some()
    }

    /// The versioned blob a record stores: the decision plus its schema
    /// version, which `Decision` ignores when reading it back.
    fn versioned_blob(decision: &Decision) -> Result<Value, StorageError> {
        let mut blob = Self::blob(decision)?;
        if let Value::Object(map) = &mut blob {
            map.insert(
                "__version".to_owned(),
                Value::from(crate::models::DECISION_BLOB_VERSION),
            );
        }
        Ok(blob)
    }

    /// A stored record's retention, for the replay that returns it.
    fn retention_of(st: &StorageState, scope: &IdempotencyScope) -> Retention {
        Self::stored(st, scope).map_or(Retention::Unrecorded, |record| Retention::Recorded {
            expires_at: record.expires_at,
        })
    }

    /// What a primitive that evaluates no policy applied: a credit, a rollback
    /// or a lease settlement is always allowed by the time it reaches storage.
    fn applied(plan: DebitPlan) -> Decision {
        Decision {
            result: DecisionResult::Allowed,
            debit_plan: plan,
            diagnostics: BTreeMap::new(),
        }
    }

    fn blob(decision: &impl serde::Serialize) -> Result<Value, StorageError> {
        serde_json::to_value(decision).map_err(|e| StorageError::Internal(e.to_string()))
    }

    fn decision_from(blob: Value) -> Result<Decision, StorageError> {
        serde_json::from_value(blob).map_err(|e| StorageError::Internal(e.to_string()))
    }

    /// The policy this transaction evaluates: the metric's own when one is
    /// active, the global fallback otherwise. Selection happens here, under the
    /// same lock as the mutation, never in the caller.
    fn select_policy(st: &StorageState, metric: &MetricId) -> Result<PolicyVersion, StorageError> {
        let active = |scope: &PolicyScope| {
            st.policies
                .values()
                .filter_map(|v| Self::active_version(v))
                .find(|v| &v.scope == scope)
                .cloned()
        };
        active(&PolicyScope::Metric {
            metric: metric.clone(),
        })
        .or_else(|| active(&PolicyScope::Global))
        .ok_or_else(|| StorageError::Internal("no active policy for the operation".to_owned()))
    }

    /// Materialize the engine environment from the rows this transaction holds
    /// and run the caller's evaluator. Nothing is written here: the decision is
    /// validated by the callback before any counter moves.
    fn evaluated(
        st: &StorageState,
        mutation: &EvaluatedMutation<'_>,
    ) -> Result<(PolicyVersion, Decision), StorageError> {
        Self::evaluated_within(st, mutation, None)
    }

    /// [`Self::evaluated`] with the engine's budget given rather than resolved
    /// from the policy: a batch item runs on what remains of the batch timer.
    fn evaluated_within(
        st: &StorageState,
        mutation: &EvaluatedMutation<'_>,
        budget: Option<EvaluationBudget>,
    ) -> Result<(PolicyVersion, Decision), StorageError> {
        let policy = Self::select_policy(st, &mutation.applicable.metric)?;
        let snapshots: Vec<QuotaSnapshot> = st
            .quotas
            .values()
            .filter(|q| {
                Self::matches(q, mutation.applicable)
                    && crate::conditions_hold(&q.metadata, mutation.request)
            })
            .map(|q| Self::snapshot(st, q))
            .collect();
        let quotas: Vec<EvaluationQuota<'_>> = snapshots
            .iter()
            .map(|snapshot| EvaluationQuota {
                snapshot,
                tier: QuotaScopeTier::of(&snapshot.subject),
            })
            .collect();
        // Resolve the budget from the version selected by this transaction,
        // unless the caller already fixed it.
        let budget = match budget {
            Some(budget) => budget,
            None => mutation.limits.budget(policy.timeout_ms).map_err(|error| {
                StorageError::EvaluationFailed {
                    engine_id: policy.engine_id.clone(),
                    failure: error.into(),
                }
            })?,
        };
        let decision = {
            let context = EvaluationContext {
                policy: &policy,
                metric: &mutation.applicable.metric,
                amount: mutation.amount,
                time: OffsetDateTime::now_utc(),
                quotas: &quotas,
                request: mutation.request,
                metric_id: mutation.metric_id,
                budget,
            };
            (mutation.evaluate)(&context).map_err(|failure| match failure {
                EvaluationFailure::PreparationRequired { policy_id, version } => {
                    StorageError::PreparationRequired { policy_id, version }
                }
                failure => StorageError::EvaluationFailed {
                    engine_id: policy.engine_id.clone(),
                    failure,
                },
            })?
        };
        Ok((policy, decision.into_decision()))
    }

    /// Evaluate every item of an envelope in submission order, each against the
    /// counters every earlier allowed item moved, and report whether all were
    /// allowed. A denial does not stop the loop: later items are still
    /// evaluated for the caller's diagnostics. The caller restores the
    /// counters when the batch is denied.
    ///
    /// The double holds every row at once, so it arms the batch timer here;
    /// each engine runs on what remains of it, and the timer is checked once
    /// more before anything is kept.
    fn run_batch(
        st: &mut StorageState,
        actor: &Actor,
        batch: &EvaluatedBatch<'_>,
    ) -> Result<BatchRun, StorageError> {
        let mut decisions = Vec::with_capacity(batch.items.len());
        let mut entries = Vec::with_capacity(batch.items.len());
        let mut policies = Vec::with_capacity(batch.items.len());
        let mut denied = false;
        let now = Self::now(st);
        if batch.timer.arm().is_zero() {
            return Err(StorageError::BatchTimeout);
        }
        for entry in batch.items {
            let item = entry.item;
            let budget = EvaluationBudget::within(batch.timer.remaining(), batch.limits.cost_limit)
                .ok_or(StorageError::BatchTimeout)?;
            let (selected, decision) = Self::evaluated_within(
                st,
                &EvaluatedMutation {
                    applicable: &item.applicable,
                    amount: item.amount,
                    request: &item.request,
                    metric_id: item.metric_id.as_deref(),
                    limits: batch.limits,
                    idempotency: &item.idempotency,
                    authorized: item.authorized,
                    evaluate: Arc::clone(&batch.evaluate),
                },
                Some(budget),
            )
            .map_err(Self::batch_timeout)?;
            policies.push(selected);
            if matches!(decision.result, DecisionResult::Denied { .. }) {
                denied = true;
                entries.push(Vec::new());
            } else {
                entries.push(Self::debit(st, actor, &decision.debit_plan, now)?);
            }
            decisions.push(decision);
        }
        if batch.timer.expired() {
            return Err(StorageError::BatchTimeout);
        }
        Ok(BatchRun {
            policies,
            decisions,
            entries,
            committed: !denied,
        })
    }

    /// An engine that ran out of the batch's time is the batch timing out.
    fn batch_timeout(error: StorageError) -> StorageError {
        match error {
            StorageError::EvaluationFailed {
                failure: EvaluationFailure::Engine(EngineError::Timeout),
                ..
            } => StorageError::BatchTimeout,
            other => other,
        }
    }

    /// Apply a validated plan, refusing a deactivated Quota before the first
    /// mutation and settling any period the plan's Quotas have outgrown.
    fn debit(
        st: &mut StorageState,
        actor: &Actor,
        plan: &DebitPlan,
        now: OffsetDateTime,
    ) -> Result<Vec<AppliedEntry>, StorageError> {
        for (_, entry) in crate::models::plan_entries(plan) {
            Self::active_quota(st, entry.quota_id)?;
        }
        for (_, entry) in crate::models::plan_entries(plan) {
            Self::settle_elapsed_rows(st, actor, entry.quota_id, now);
        }
        crate::models::plan_entries(plan)
            .map(|(slot, entry)| {
                Self::debit_counter(st, actor, entry.quota_id, entry.amount, now)
                    .map(|applied| AppliedEntry { slot, ..applied })
            })
            .collect()
    }

    /// The plan a set of movements applied.
    fn plan_of_entries(entries: &[AppliedEntry]) -> DebitPlan {
        crate::models::plan_of(entries.iter().map(|entry| {
            (
                entry.slot,
                crate::models::QuotaDebitPlan {
                    quota_id: entry.quota_id,
                    amount: entry.amount,
                },
            )
        }))
    }

    /// Counter state after a set of movements, each reported against the row it
    /// touched.
    fn counters_of(st: &StorageState, entries: &[AppliedEntry]) -> MutationResult {
        MutationResult {
            counters: entries
                .iter()
                .map(|entry| crate::models::CounterSnapshot {
                    quota_id: entry.quota_id,
                    period_id: entry.period_id,
                    value: match entry.period_id {
                        Some(id) => st.periods.get(&id).map_or(0, |row| row.consumed),
                        None => st.in_flight.get(&entry.quota_id).copied().unwrap_or(0),
                    },
                })
                .collect(),
            threshold_crossings: Vec::new(),
            event_ids: Vec::new(),
        }
    }

    fn active_quota(st: &StorageState, id: QuotaId) -> Result<&Quota, StorageError> {
        let quota = st
            .quotas
            .get(&id)
            .ok_or(StorageError::QuotaNotFound { id })?;
        if quota.status == QuotaStatus::Deactivated {
            return Err(StorageError::QuotaDeactivated { id });
        }
        Ok(quota)
    }

    /// Offset pagination; the cursor is the decimal offset. A cursor that is
    /// not one is refused, as a real backend refuses a foreign cursor.
    fn paginate<T: Clone>(items: &[T], page: &PageRequest) -> Result<PageResult<T>, StorageError> {
        let start: usize = match page.cursor.as_deref() {
            None => 0,
            Some(cursor) => cursor.parse().map_err(|_| StorageError::InvalidCursor)?,
        };
        let limit = page.limit.max(1) as usize;
        let end = start.saturating_add(limit).min(items.len());
        let next_cursor = (end < items.len()).then(|| end.to_string());
        Ok(PageResult {
            items: items.get(start..end).unwrap_or_default().to_vec(),
            next_cursor,
        })
    }

    fn snapshot(st: &StorageState, quota: &Quota) -> QuotaSnapshot {
        let consumed = Self::counter_value(st, quota.id);
        let now = Self::now(st);
        QuotaSnapshot {
            quota_id: quota.id,
            subject: quota.subject.clone(),
            metric: quota.metric.clone(),
            quota_type: quota.quota_type,
            enforcement_mode: quota.enforcement_mode,
            cap: quota.cap,
            consumed,
            remaining: quota.cap.map(|cap| cap.saturating_sub(consumed)),
            period: quota.period.map(|period| period.window_containing(now)),
            metadata: quota.metadata.clone(),
            validity_window: quota.validity_window,
            currently_within_window: quota.validity_window.is_none_or(|w| w.contains(now)),
        }
    }

    /// Snapshots of `ids`, in that order. A read writes nothing (I3): a
    /// missing or elapsed row reads as zero, and the next mutation creates
    /// the current one.
    fn read_snapshots(st: &StorageState, ids: &[QuotaId]) -> Vec<QuotaSnapshot> {
        ids.iter()
            .filter_map(|id| st.quotas.get(id).cloned())
            .map(|quota| Self::snapshot(st, &quota))
            .collect()
    }

    fn matches(quota: &Quota, applicable: &ApplicableQuotas) -> bool {
        quota.status == QuotaStatus::Active
            && quota.tenant_id == applicable.tenant_id
            && quota.metric == applicable.metric
            && applicable.subjects.contains(&quota.subject)
    }

    fn active_version(versions: &[PolicyVersion]) -> Option<&PolicyVersion> {
        versions
            .iter()
            .find(|v| v.state == PolicyVersionState::Active)
    }

    /// Settle a lease: commit unwinds the unused part from the holds and keeps
    /// the rest, release keeps nothing. The two differ only in what they keep and in the
    /// terminal state they leave behind, so they share one body.
    ///
    /// The idempotency scope is completed from the key the acquisition
    /// persisted, never from caller input, and the replay check runs before the
    /// lease-state guards, so a settlement that already succeeded replays even
    /// though its lease is no longer active.
    #[allow(
        clippy::too_many_arguments,
        reason = "the contract's commit arguments plus the operation"
    )]
    fn settle(
        &self,
        actor: &Actor,
        token: LeaseToken,
        actual_amount: Option<u64>,
        metric: Option<&MetricId>,
        idempotency: &PartialIdempotencyWrite,
        events: &[NotificationEvent],
        operation: OperationType,
    ) -> Result<TransitionOutcome<AppliedMutation>, StorageError> {
        self.transact(|st| {
            let lease = st
                .leases
                .get(&token)
                .filter(|lease| lease.tenant_id == idempotency.tenant_id)
                .cloned()
                .ok_or(StorageError::LeaseNotFound { token })?;
            let write = idempotency.clone().complete(operation);
            if let Some(blob) = Self::replayed(st, &write)? {
                return Ok(TransitionOutcome::NoOp(AppliedMutation {
                    decision: Self::decision_from(blob)?,
                    mutation: MutationResult::default(),
                }));
            }
            let now = Self::now(st);
            if lease.state != LeaseState::Active || lease.expires_at <= now {
                return Err(StorageError::LeaseNotActive { token });
            }
            if let Some(metric) = metric.filter(|metric| **metric != lease.metric) {
                return Err(StorageError::MetricTypeMismatch {
                    requested: metric.as_str().to_owned(),
                    charged: lease.metric.as_str().to_owned(),
                });
            }
            let actual = actual_amount.unwrap_or(lease.reserved_amount);
            if actual > lease.reserved_amount {
                return Err(StorageError::OverCommitNotAuthorized {
                    reserved: lease.reserved_amount,
                    actual,
                });
            }
            let entries_held: Vec<crate::models::HeldEntry> = lease
                .holds
                .iter()
                .map(|row| crate::models::HeldEntry {
                    chain: row.slot.0,
                    position: row.slot.1,
                    held: row.hold.held_amount,
                })
                .collect();
            let returns = crate::models::unwind(&entries_held, lease.reserved_amount - actual)
                .map_err(|error| StorageError::Internal(error.to_string()))?;
            let mut entries = Vec::with_capacity(returns.len());
            for (held_row, returned) in lease.holds.iter().zip(returns) {
                let entry = AppliedEntry {
                    quota_id: held_row.hold.quota_id,
                    period_id: held_row.hold.period_id,
                    amount: returned,
                    slot: held_row.slot,
                };
                // Against the acquisition period, whatever period the wall
                // clock is in now (I5).
                Self::credit_counter(st, &entry, now);
                let kept = held_row.hold.held_amount - returned;
                if kept > 0 {
                    entries.push(AppliedEntry {
                        amount: kept,
                        ..entry
                    });
                }
            }
            if let Some(row) = st.leases.get_mut(&token) {
                row.state = match operation {
                    OperationType::Release => LeaseState::Released,
                    _ => LeaseState::Committed,
                };
                for held in &mut row.holds {
                    held.returned = true;
                }
            }
            // Settling evaluates nothing: the plan was fixed at acquisition.
            let decision = Self::applied(Self::plan_of_entries(&entries));
            let mut result = Self::counters_of(st, &entries);
            let blob = Self::versioned_blob(&decision)?;
            if operation == OperationType::Commit {
                Self::charge(
                    st,
                    &write,
                    blob,
                    None,
                    lease.authorized,
                    &lease.metric,
                    &entries,
                );
            } else {
                Self::remember(st, &write, blob, None);
            }
            result.event_ids = Self::push_events_for(
                st,
                actor,
                events,
                match operation {
                    OperationType::Release => "lease_release",
                    _ => "lease_commit",
                },
                entries.iter().map(|entry| entry.quota_id).collect(),
            );
            Ok(TransitionOutcome::Applied(AppliedMutation {
                decision,
                mutation: result,
            }))
        })
    }
}

#[async_trait]
impl QuotaEnforcementStoragePluginV1 for InMemoryStorage {
    async fn bootstrap(&self, bundle: &BootstrapBundle) -> Result<(), StorageError> {
        let mut st = self.state.lock();
        st.bootstrap_calls += 1;
        Self::check(&st)?;
        let installed = st.installed_major.unwrap_or(bundle.contract_major);
        if installed != bundle.contract_major {
            return Err(StorageError::SchemaVersionMismatch {
                installed,
                expected: bundle.contract_major,
            });
        }
        st.installed_major = Some(installed);
        if st.defaults.is_none() {
            st.defaults = Some(bundle.config_defaults);
        }
        if let Some(draft) = &bundle.global_policy {
            st.policies
                .entry(PolicyId::global())
                .or_insert_with(|| vec![new_version(PolicyId::global(), 1, draft.clone())]);
        }
        st.bootstrapped = Some(bundle.clone());
        Ok(())
    }

    async fn read_active_quota_counts(&self) -> Result<ActiveQuotaCounts, StorageError> {
        let st = self.state.lock();
        Self::check(&st)?;
        let mut counts = ActiveQuotaCounts::default();
        for quota in st
            .quotas
            .values()
            .filter(|quota| quota.status == QuotaStatus::Active)
        {
            match quota.cap {
                Some(0) => counts.cap_zero += 1,
                None => counts.cap_unbounded += 1,
                Some(_) => {}
            }
            *counts.by_metric.entry(quota.metric.clone()).or_insert(0) += 1;
            if quota.metadata.is_empty() {
                continue;
            }
            match counts
                .conditions
                .iter_mut()
                .find(|set| set.metric == quota.metric && set.metadata == quota.metadata)
            {
                Some(set) => set.quotas += 1,
                None => counts.conditions.push(crate::ActiveConditions {
                    metric: quota.metric.clone(),
                    metadata: quota.metadata.clone(),
                    quotas: 1,
                }),
            }
        }
        Ok(counts)
    }

    async fn create_quota(
        &self,
        actor: &Actor,
        _scope: &AccessScope,
        draft: QuotaDraft,
        events: &[NotificationEvent],
    ) -> Result<QuotaId, StorageError> {
        let mut st = self.state.lock();
        Self::check(&st)?;
        Ok(Self::create_in(&mut st, actor, draft, events))
    }

    async fn update_quota(
        &self,
        actor: &Actor,
        _scope: &AccessScope,
        quota_id: QuotaId,
        patch: QuotaPatch,
        events: &[NotificationEvent],
    ) -> Result<Quota, StorageError> {
        let mut st = self.state.lock();
        Self::check(&st)?;
        Self::update_in(&mut st, actor, quota_id, patch, events)
    }

    async fn deactivate_quota(
        &self,
        actor: &Actor,
        _scope: &AccessScope,
        quota_id: QuotaId,
        events: &[NotificationEvent],
    ) -> Result<DeactivateOutcome, StorageError> {
        let mut st = self.state.lock();
        Self::check(&st)?;
        Self::deactivate_in(&mut st, actor, quota_id, events)
    }

    async fn bulk_create_quotas(
        &self,
        actor: &Actor,
        envelope: &BulkCreateEnvelope,
    ) -> Result<TransitionOutcome<BulkCreated>, StorageError> {
        self.transact(|st| {
            if let Some(stored) = Self::bulk_replayed(st, &envelope.idempotency)? {
                return Ok(TransitionOutcome::NoOp(stored));
            }
            let mut items = Vec::with_capacity(envelope.items.len());
            for (index, entry) in envelope.items.iter().enumerate() {
                if entry.draft.tenant_id != envelope.tenant_id {
                    return Err(StorageError::SubjectOutOfScope.at_item(index));
                }
                let quota_id = Self::create_in(st, actor, entry.draft.clone(), &entry.events);
                items.push(BulkCreatedItem {
                    index,
                    idempotency_key: entry.idempotency_key.clone(),
                    quota_id,
                });
            }
            let outcome = BulkCreated { items };
            Self::remember_bulk(st, &envelope.idempotency, &outcome)?;
            Ok(TransitionOutcome::Applied(outcome))
        })
    }

    async fn bulk_update_quotas(
        &self,
        actor: &Actor,
        envelope: &BulkUpdateEnvelope,
    ) -> Result<TransitionOutcome<BulkUpdated>, StorageError> {
        self.transact(|st| {
            let targets: Vec<QuotaId> = envelope.items.iter().map(|item| item.quota_id).collect();
            if let Some(stored) = Self::bulk_replayed(st, &envelope.idempotency)? {
                Self::replay_visible(st, envelope.tenant_id, &targets)?;
                return Ok(TransitionOutcome::NoOp(stored));
            }
            let mut items = Vec::with_capacity(envelope.items.len());
            for (index, entry) in envelope.items.iter().enumerate() {
                Self::visible(st, envelope.tenant_id, entry.quota_id)
                    .map_err(|e| e.at_item(index))?;
                let updated = Self::update_in(
                    st,
                    actor,
                    entry.quota_id,
                    entry.patch.clone(),
                    &entry.events,
                )
                .map_err(|e| e.at_item(index))?;
                items.push(BulkUpdatedItem {
                    index,
                    idempotency_key: entry.idempotency_key.clone(),
                    quota_id: entry.quota_id,
                    record_version: updated.record_version,
                });
            }
            let outcome = BulkUpdated { items };
            Self::remember_bulk(st, &envelope.idempotency, &outcome)?;
            Ok(TransitionOutcome::Applied(outcome))
        })
    }

    async fn bulk_deactivate_quotas(
        &self,
        actor: &Actor,
        envelope: &BulkDeactivateEnvelope,
    ) -> Result<TransitionOutcome<BulkDeactivated>, StorageError> {
        self.transact(|st| {
            let targets: Vec<QuotaId> = envelope.items.iter().map(|item| item.quota_id).collect();
            if let Some(stored) = Self::bulk_replayed(st, &envelope.idempotency)? {
                Self::replay_visible(st, envelope.tenant_id, &targets)?;
                return Ok(TransitionOutcome::NoOp(stored));
            }
            let mut items = Vec::with_capacity(envelope.items.len());
            for (index, entry) in envelope.items.iter().enumerate() {
                Self::visible(st, envelope.tenant_id, entry.quota_id)
                    .map_err(|e| e.at_item(index))?;
                let deactivated = Self::deactivate_in(st, actor, entry.quota_id, &entry.events)
                    .map_err(|e| e.at_item(index))?;
                items.push(BulkDeactivatedItem {
                    index,
                    idempotency_key: entry.idempotency_key.clone(),
                    quota_id: entry.quota_id,
                    resolved_leases: deactivated.resolved_leases,
                });
            }
            let outcome = BulkDeactivated { items };
            Self::remember_bulk(st, &envelope.idempotency, &outcome)?;
            Ok(TransitionOutcome::Applied(outcome))
        })
    }

    async fn read_quotas(
        &self,
        _scope: &AccessScope,
        filter: QuotaFilter,
        page: PageRequest,
    ) -> Result<PageResult<Quota>, StorageError> {
        let st = self.state.lock();
        Self::check(&st)?;
        let items: Vec<Quota> = st
            .quotas
            .values()
            .filter(|q| filter.tenant_id.is_none_or(|t| q.tenant_id == t))
            .filter(|q| filter.subject.as_ref().is_none_or(|s| &q.subject == s))
            .filter(|q| filter.metric.as_ref().is_none_or(|m| &q.metric == m))
            .filter(|q| filter.status.is_none_or(|s| q.status == s))
            .filter(|q| filter.ids.is_empty() || filter.ids.contains(&q.id))
            .cloned()
            .collect();
        if st.empty_first_quota_page && page.cursor.is_none() {
            return Ok(PageResult {
                items: Vec::new(),
                next_cursor: Some("0".to_owned()),
            });
        }
        let page = match st.page_cap {
            Some(cap) => PageRequest {
                limit: page.limit.min(cap),
                ..page
            },
            None => page,
        };
        Self::paginate(&items, &page)
    }

    async fn apply_debit_plan(
        &self,
        actor: &Actor,
        scope: &AccessScope,
        mutation: &EvaluatedMutation<'_>,
        events: &[NotificationEvent],
    ) -> Result<TransitionOutcome<EvaluatedDebit>, StorageError> {
        self.record_scope(scope);
        self.transact(|st| {
            if let Some(blob) = Self::replayed(st, mutation.idempotency)? {
                // Replays return the recorded decision without side effects.
                return Ok(TransitionOutcome::NoOp(EvaluatedDebit {
                    decision: Self::decision_from(blob)?,
                    mutation: MutationResult::default(),
                    retention: Self::retention_of(st, &mutation.idempotency.scope),
                }));
            }
            let now = Self::now(st);
            let (policy, decision) = Self::evaluated(st, mutation)?;
            if decision.is_no_applicable_quota() {
                // Provisioning must be able to change this unrecorded denial.
                return Ok(TransitionOutcome::Applied(EvaluatedDebit {
                    decision,
                    mutation: MutationResult::default(),
                    retention: Retention::Unrecorded,
                }));
            }
            let entries = Self::debit(st, actor, &decision.debit_plan, now)?;
            let mut counters = Self::counters_of(st, &entries);
            let blob = Self::versioned_blob(&decision)?;
            if !matches!(decision.result, DecisionResult::Allowed) {
                // A recorded denial occupies its key but emits no mutation event.
                let expires_at = Self::remember(st, mutation.idempotency, blob, Some(&policy));
                return Ok(TransitionOutcome::Applied(EvaluatedDebit {
                    decision,
                    mutation: counters,
                    retention: Retention::Recorded { expires_at },
                }));
            }
            let expires_at = Self::charge(
                st,
                mutation.idempotency,
                blob,
                Some(&policy),
                mutation.authorized,
                &mutation.applicable.metric,
                &entries,
            );
            counters.event_ids = Self::push_events_for(
                st,
                actor,
                events,
                "debit",
                entries.iter().map(|e| e.quota_id).collect(),
            );
            Ok(TransitionOutcome::Applied(EvaluatedDebit {
                decision,
                mutation: counters,
                retention: Retention::Recorded { expires_at },
            }))
        })
    }

    async fn apply_batch_debit(
        &self,
        actor: &Actor,
        scope: &AccessScope,
        batch: &EvaluatedBatch<'_>,
        events: &[NotificationEvent],
    ) -> Result<TransitionOutcome<Vec<EvaluatedDebit>>, StorageError> {
        self.record_scope(scope);
        self.transact(|st| {
            if let Some(blob) = Self::replayed(st, batch.envelope)? {
                let decisions = serde_json::from_value::<BatchRecord>(blob)
                    .map_err(|e| StorageError::Internal(e.to_string()))?
                    .decisions;
                let retention = Self::retention_of(st, &batch.envelope.scope);
                return Ok(TransitionOutcome::NoOp(
                    decisions
                        .into_iter()
                        .map(|decision| EvaluatedDebit {
                            decision,
                            mutation: MutationResult::default(),
                            retention,
                        })
                        .collect(),
                ));
            }
            // A batch is replayed only as a whole: an item key that names a
            // record of a single debit refuses it.
            if let Some(index) = batch
                .items
                .iter()
                .position(|entry| Self::item_key_taken(st, &entry.item.idempotency.scope))
            {
                return Err(StorageError::IdempotencyPayloadMismatch.at_item(index));
            }
            // Each item observes prior items in the batch. Staging preserves
            // all-or-nothing counters and threshold events.
            let staged = st.clone();
            let run = Self::run_batch(st, actor, batch)?;
            if !run.committed {
                *st = staged;
            }
            let blob = Self::blob(&BatchRecord::new(run.decisions.clone()))?;
            // A denied envelope still occupies its idempotency key.
            let expires_at = Self::remember(st, batch.envelope, blob, run.policies.first());
            let moved: Vec<AppliedEntry> = run.entries.iter().flatten().cloned().collect();
            if run.committed {
                for (((entry, decision), policy), applied) in batch
                    .items
                    .iter()
                    .zip(&run.decisions)
                    .zip(&run.policies)
                    .zip(&run.entries)
                {
                    let blob = Self::versioned_blob(decision)?;
                    Self::charge(
                        st,
                        &entry.item.idempotency,
                        blob,
                        Some(policy),
                        entry.item.authorized,
                        &entry.item.applicable.metric,
                        applied,
                    );
                }
                Self::push_events_for(
                    st,
                    actor,
                    events,
                    "batch-debit",
                    moved.iter().map(|e| e.quota_id).collect(),
                );
            }
            let counters = Self::counters_of(st, &moved);
            Ok(TransitionOutcome::Applied(
                run.decisions
                    .into_iter()
                    .map(|decision| EvaluatedDebit {
                        decision,
                        mutation: if run.committed {
                            counters.clone()
                        } else {
                            MutationResult::default()
                        },
                        retention: Retention::Recorded { expires_at },
                    })
                    .collect(),
            ))
        })
    }

    async fn apply_credit(
        &self,
        actor: &Actor,
        scope: &AccessScope,
        quota_id: QuotaId,
        amount: u64,
        idempotency: &PartialIdempotencyWrite,
        events: &[NotificationEvent],
    ) -> Result<TransitionOutcome<AppliedMutation>, StorageError> {
        self.record_scope(scope);
        let scope = InMemoryScope::of(scope)?;
        let principal = actor.to_string();
        self.transact(|st| {
            // Check replay before fresh-credit guards so an earlier success
            // still replays after deactivation.
            let quota = st
                .quotas
                .get(&quota_id)
                .filter(|quota| scope.admits(quota))
                .cloned()
                .ok_or(StorageError::QuotaNotFound { id: quota_id })?;
            let write = idempotency.clone().complete(OperationType::Credit);
            if let Some(blob) = Self::replayed(st, &write)? {
                return Ok(TransitionOutcome::NoOp(AppliedMutation {
                    decision: Self::decision_from(blob)?,
                    mutation: MutationResult::default(),
                }));
            }
            if quota.status == QuotaStatus::Deactivated {
                return Err(StorageError::QuotaDeactivated { id: quota_id });
            }
            let now = Self::now(st);
            if quota.quota_type == QuotaType::Consumption {
                // Credit uses calendar closure; materialize an absent current row.
                let closed = st
                    .current_period
                    .get(&quota_id)
                    .and_then(|id| st.periods.get(id))
                    .is_some_and(|row| row.window.has_elapsed(now));
                if closed {
                    return Err(StorageError::PeriodClosed);
                }
                Self::ensure_current_row(st, quota_id, now);
                Self::settle_elapsed_rows(st, actor, quota_id, now);
            }
            let entry = AppliedEntry {
                quota_id,
                period_id: (quota.quota_type == QuotaType::Consumption)
                    .then(|| Self::ensure_current_row(st, quota_id, now))
                    .filter(|_| true),
                amount,
                slot: (0, 0),
            };
            Self::credit_counter(st, &entry, now);
            let entries = [entry];
            let mut result = Self::counters_of(st, &entries);
            let decision = Self::applied(vec![vec![crate::models::QuotaDebitPlan {
                quota_id,
                amount,
            }]]);
            // A credit evaluates no policy, so its record carries neither
            // engine attribution nor a reversible movement.
            let blob = Self::versioned_blob(&decision)?;
            Self::remember(st, &write, blob, None);
            let adjusted = Self::event(
                st,
                actor,
                NotificationEventKind::QuotaCounterAdjusted,
                &quota,
                serde_json::json!({
                    "credited_amount": amount,
                    "quota_id": quota_id,
                    "principal": principal,
                }),
            );
            st.events.push(adjusted);
            result.event_ids = Self::push_events_for(st, actor, events, "credit", vec![quota_id]);
            Ok(TransitionOutcome::Applied(AppliedMutation {
                decision,
                mutation: result,
            }))
        })
    }

    async fn apply_return(
        &self,
        actor: &Actor,
        scope: &AccessScope,
        ret: &ChargeReturn,
        _events: &[NotificationEvent],
    ) -> Result<TransitionOutcome<AppliedMutation>, StorageError> {
        self.record_scope(scope);
        let scope = InMemoryScope::of(scope)?;
        self.transact(|st| {
            let key = ret.scope();
            if let Some(row) = st.returns.get(&key).cloned() {
                let recorded = st
                    .charges
                    .get(&row.charge)
                    .filter(|charge| charge.id == row.charge_id)
                    .filter(|charge| Self::charge_live(st, charge, Self::now(st)))
                    .filter(|charge| Self::charge_admitted(st, &scope, charge))
                    .cloned();
                match recorded {
                    Some(charge) => {
                        if row.charge != ret.charge || row.amount != ret.amount {
                            return Err(StorageError::IdempotencyPayloadMismatch);
                        }
                        if charge.metric != ret.metric {
                            return Err(StorageError::MetricTypeMismatch {
                                requested: ret.metric.as_str().to_owned(),
                                charged: charge.metric.as_str().to_owned(),
                            });
                        }
                        return Ok(TransitionOutcome::NoOp(AppliedMutation {
                            decision: Self::applied(Vec::new()),
                            mutation: MutationResult::default(),
                        }));
                    }
                    // Deleted with its charge.
                    None => {
                        st.returns.remove(&key);
                    }
                }
            }
            let charge = Self::live_charge(st, &ret.charge)
                .filter(|charge| Self::charge_admitted(st, &scope, charge))
                .cloned()
                .ok_or_else(|| StorageError::OperationNotFound {
                    key: ret.charge.key.clone(),
                })?;
            if charge.metric != ret.metric {
                return Err(StorageError::MetricTypeMismatch {
                    requested: ret.metric.as_str().to_owned(),
                    charged: charge.metric.as_str().to_owned(),
                });
            }
            let (plan, result) = Self::give_back(st, actor, &ret.charge, ret.amount, "return")?;
            st.returns.insert(
                key,
                ReturnRow {
                    charge_id: charge.id,
                    charge: ret.charge.clone(),
                    amount: ret.amount,
                },
            );
            Ok(TransitionOutcome::Applied(AppliedMutation {
                decision: Self::applied(plan),
                mutation: result,
            }))
        })
    }

    async fn apply_rollback(
        &self,
        actor: &Actor,
        scope: &AccessScope,
        target: &RollbackTarget,
        _events: &[NotificationEvent],
    ) -> Result<TransitionOutcome<AppliedMutation>, StorageError> {
        self.record_scope(scope);
        let scope = InMemoryScope::of(scope)?;
        self.transact(|st| {
            let charge = Self::live_charge(st, &target.original)
                .filter(|charge| charge.authorized == target.authorized)
                .filter(|charge| Self::charge_admitted(st, &scope, charge))
                .cloned()
                .ok_or_else(|| StorageError::OperationNotFound {
                    key: target.original.key.clone(),
                })?;
            let held: Vec<HeldEntry> = charge
                .entries
                .iter()
                .map(|entry| HeldEntry {
                    chain: entry.charged.slot.0,
                    position: entry.charged.slot.1,
                    held: entry.charged.amount - entry.returned,
                })
                .collect();
            let remaining = Self::remaining(&held);
            if remaining == 0 {
                return Ok(TransitionOutcome::NoOp(AppliedMutation {
                    decision: Self::applied(Vec::new()),
                    mutation: MutationResult::default(),
                }));
            }
            let (plan, result) =
                Self::give_back(st, actor, &target.original, remaining, "rollback")?;
            Ok(TransitionOutcome::Applied(AppliedMutation {
                decision: Self::applied(plan),
                mutation: result,
            }))
        })
    }

    async fn acquire_lease(
        &self,
        actor: &Actor,
        scope: &AccessScope,
        mutation: &EvaluatedMutation<'_>,
        ttl: Duration,
    ) -> Result<TransitionOutcome<EvaluatedLease>, StorageError> {
        self.record_scope(scope);
        self.transact(|st| {
            if let Some(blob) = Self::replayed(st, mutation.idempotency)? {
                // Persist the acquisition outcome because subjects may hold
                // several leases and a replay must return the original token or
                // denial.
                let acquired: EvaluatedLease = serde_json::from_value(blob)
                    .map_err(|e| StorageError::Internal(e.to_string()))?;
                return Ok(TransitionOutcome::NoOp(acquired));
            }
            // One instant for the whole transaction, taken once every row this
            // acquisition needs is held: the live count, the period the holds
            // are attributed to, and the expiry all agree by construction.
            let now = Self::now(st);
            let (policy, decision) = Self::evaluated(st, mutation)?;
            // A denied acquisition holds nothing and still occupies the key, so
            // its replay denies again. The cap is not consulted: a verdict the
            // engine refused is returned whether or not the cap is full.
            let (token, expires_at) = if matches!(decision.result, DecisionResult::Allowed)
                && !decision.debit_plan.is_empty()
            {
                let cap = st.defaults.map_or(1000, |d| d.max_active_leases) as usize;
                // Expired leases never count (I4): the live count is what
                // admits, never a maintained total.
                let live = st
                    .leases
                    .values()
                    .filter(|l| {
                        l.state == LeaseState::Active
                            && l.expires_at > now
                            && l.tenant_id == mutation.applicable.tenant_id
                            && l.metric == mutation.applicable.metric
                    })
                    .count();
                if live >= cap {
                    return Err(StorageError::LeaseInflightLimitExceeded);
                }
                let entries = Self::debit(st, actor, &decision.debit_plan, now)?;
                let token = LeaseToken::generate();
                let expires_at = now + ttl;
                st.leases.insert(
                    token,
                    LeaseRow {
                        tenant_id: mutation.applicable.tenant_id,
                        metric: mutation.applicable.metric.clone(),
                        authorized: mutation.authorized,
                        reserved_amount: mutation.amount,
                        // The acquisition period is fixed here; commit and
                        // release settle against it, never against the current
                        // period (I5).
                        holds: entries
                            .iter()
                            .map(|entry| HeldRow {
                                hold: LeaseHold {
                                    quota_id: entry.quota_id,
                                    held_amount: entry.amount,
                                    period_id: entry.period_id,
                                },
                                slot: entry.slot,
                                returned: false,
                            })
                            .collect(),
                        state: LeaseState::Active,
                        expires_at,
                    },
                );
                (Some(token), Some(expires_at))
            } else {
                (None, None)
            };
            let acquired = EvaluatedLease {
                decision,
                token,
                expires_at,
            };
            let blob = Self::blob(&acquired)?;
            Self::remember(st, mutation.idempotency, blob, Some(&policy));
            Ok(TransitionOutcome::Applied(acquired))
        })
    }

    async fn commit_lease(
        &self,
        actor: &Actor,
        scope: &AccessScope,
        token: LeaseToken,
        actual_amount: Option<u64>,
        metric: Option<&MetricId>,
        idempotency: &PartialIdempotencyWrite,
        events: &[NotificationEvent],
    ) -> Result<TransitionOutcome<AppliedMutation>, StorageError> {
        self.record_scope(scope);
        self.settle(
            actor,
            token,
            actual_amount,
            metric,
            idempotency,
            events,
            OperationType::Commit,
        )
    }

    async fn release_lease(
        &self,
        actor: &Actor,
        scope: &AccessScope,
        token: LeaseToken,
        idempotency: &PartialIdempotencyWrite,
        events: &[NotificationEvent],
    ) -> Result<TransitionOutcome<AppliedMutation>, StorageError> {
        self.record_scope(scope);
        self.settle(
            actor,
            token,
            Some(0),
            None,
            idempotency,
            events,
            OperationType::Release,
        )
    }

    async fn read_quota_snapshot(
        &self,
        scope: &AccessScope,
        applicable: &ApplicableQuotas,
    ) -> Result<Vec<QuotaSnapshot>, StorageError> {
        self.record_scope(scope);
        let scope = InMemoryScope::of(scope)?;
        self.transact(|st| {
            let matched: Vec<QuotaId> = st
                .quotas
                .values()
                .filter(|quota| Self::matches(quota, applicable) && scope.admits(quota))
                .map(|quota| quota.id)
                .collect();
            Ok(Self::read_snapshots(st, &matched))
        })
    }

    async fn bulk_read_quota_snapshot(
        &self,
        scope: &AccessScope,
        pairs: &[ApplicableQuotas],
        page: PageRequest,
        in_window_only: bool,
    ) -> Result<PageResult<QuotaSnapshot>, StorageError> {
        self.record_scope(scope);
        let scope = InMemoryScope::of(scope)?;
        // Keyset on `quota_id`: the cursor holds only the last id returned.
        let after = page
            .cursor
            .as_deref()
            .map(|cursor| {
                Uuid::parse_str(cursor)
                    .map(QuotaId::new)
                    .map_err(|_| StorageError::InvalidCursor)
            })
            .transpose()?;
        let limit = if page.limit == 0 {
            PageRequest::DEFAULT_LIMIT
        } else {
            page.limit
        } as usize;
        self.transact(|st| {
            let now = Self::now(st);
            let mut selected = Vec::new();
            for quota in st.quotas.values() {
                if after.is_some_and(|after| quota.id <= after)
                    || !pairs.iter().any(|pair| Self::matches(quota, pair))
                    || !scope.admits(quota)
                    || (in_window_only
                        && quota
                            .validity_window
                            .is_some_and(|window| !window.contains(now)))
                {
                    continue;
                }
                selected.push(quota.id);
                if selected.len() > limit {
                    break;
                }
            }
            let more = selected.len() > limit;
            selected.truncate(limit);
            let next_cursor = more
                .then(|| selected.last().map(|id| id.as_uuid().to_string()))
                .flatten();
            Ok(PageResult {
                items: Self::read_snapshots(st, &selected),
                next_cursor,
            })
        })
    }

    async fn create_policy(
        &self,
        actor: &Actor,
        mut draft: PolicyDraft,
        events: &[NotificationEvent],
    ) -> Result<PolicyVersion, StorageError> {
        let mut st = self.state.lock();
        Self::check(&st)?;
        if st
            .policies
            .values()
            .filter_map(|v| Self::active_version(v))
            .any(|v| v.scope == draft.scope)
        {
            return Err(StorageError::PolicyScopeOccupied { scope: draft.scope });
        }
        let policy_id = match &draft.scope {
            PolicyScope::Global => PolicyId::global(),
            PolicyScope::Metric { .. } => PolicyId::new(Uuid::now_v7().to_string()),
        };
        draft.created_by = actor.to_string();
        let version = new_version(policy_id.clone(), 1, draft);
        st.policies.insert(policy_id.clone(), vec![version.clone()]);
        st.policy_audit.push(PolicyTransitionAudit {
            policy_id,
            version: 1,
            kind: "create",
            actor: actor.to_string(),
            comment: version.comment.clone(),
        });
        let events: Vec<_> = events
            .iter()
            .cloned()
            .map(|mut event| {
                if event.kind == crate::NotificationEventKind::PolicyChanged {
                    event.policy_id = Some(version.policy_id.clone());
                }
                event
            })
            .collect();
        Self::push_events(&mut st, actor, &events);
        Ok(version)
    }

    async fn update_policy(
        &self,
        actor: &Actor,
        policy_id: PolicyId,
        update: PolicyUpdate,
        events: &[NotificationEvent],
    ) -> Result<PolicyVersion, StorageError> {
        let mut st = self.state.lock();
        Self::check(&st)?;
        let versions =
            st.policies
                .get_mut(&policy_id)
                .ok_or_else(|| StorageError::PolicyNotFound {
                    policy_id: policy_id.clone(),
                })?;
        let current = versions
            .iter()
            .position(|v| v.state == PolicyVersionState::Active)
            .ok_or_else(|| StorageError::PolicyDeleted {
                policy_id: policy_id.clone(),
            })?;
        if versions[current].version != update.if_match_version {
            return Err(StorageError::VersionConflict {
                expected: update.if_match_version,
                actual: versions[current].version,
            });
        }
        let latest = versions.iter().map(|v| v.version).max().unwrap_or(0);
        let mut next = versions[current].clone();
        next.version = latest
            .checked_add(1)
            .ok_or_else(|| StorageError::Internal("policy version exhausted".into()))?;
        next.state = PolicyVersionState::Active;
        next.created_at = OffsetDateTime::now_utc();
        next.created_by = actor.to_string();
        next.comment = update.comment;
        if let Some(snapshot) = update.schema_snapshot {
            next.schema_snapshot = snapshot;
        }
        if let Some(engine_id) = update.engine_id {
            next.engine_id = engine_id;
        }
        if let Some(config) = update.engine_config {
            next.engine_config = config;
        }
        if update.timeout_ms.is_some() {
            next.timeout_ms = update.timeout_ms;
        }
        versions[current].state = PolicyVersionState::Superseded;
        versions.push(next.clone());
        st.policy_audit.push(PolicyTransitionAudit {
            policy_id,
            version: next.version,
            kind: "update",
            actor: actor.to_string(),
            comment: next.comment.clone(),
        });
        Self::push_events(&mut st, actor, events);
        Ok(next)
    }

    async fn rollback_policy(
        &self,
        actor: &Actor,
        policy_id: PolicyId,
        target_version: u32,
        comment: Option<String>,
        events: &[NotificationEvent],
    ) -> Result<TransitionOutcome<PolicyVersion>, StorageError> {
        let mut st = self.state.lock();
        Self::check(&st)?;
        let versions =
            st.policies
                .get_mut(&policy_id)
                .ok_or_else(|| StorageError::PolicyNotFound {
                    policy_id: policy_id.clone(),
                })?;
        if Self::active_version(versions).is_none() {
            return Err(StorageError::PolicyDeleted { policy_id });
        }
        let target = versions
            .iter()
            .position(|v| v.version == target_version)
            .ok_or_else(|| StorageError::UnknownPolicyVersion {
                policy_id: policy_id.clone(),
                version: target_version,
            })?;
        if versions[target].state == PolicyVersionState::Active {
            return Ok(TransitionOutcome::NoOp(versions[target].clone()));
        }
        if versions[target].state == PolicyVersionState::RolledBack {
            return Err(StorageError::VersionRolledBack {
                policy_id,
                version: target_version,
            });
        }
        if let Some(active) = versions
            .iter_mut()
            .find(|v| v.state == PolicyVersionState::Active && v.version != target_version)
        {
            active.state = PolicyVersionState::RolledBack;
        }
        versions[target].state = PolicyVersionState::Active;
        let result = versions[target].clone();
        st.policy_audit.push(PolicyTransitionAudit {
            policy_id,
            version: target_version,
            kind: "rollback",
            actor: actor.to_string(),
            comment,
        });
        Self::push_events(&mut st, actor, events);
        Ok(TransitionOutcome::Applied(result))
    }

    async fn delete_policy(
        &self,
        actor: &Actor,
        policy_id: PolicyId,
        comment: Option<String>,
        events: &[NotificationEvent],
    ) -> Result<TransitionOutcome<()>, StorageError> {
        let mut st = self.state.lock();
        Self::check(&st)?;
        if policy_id.is_global() {
            return Err(StorageError::CannotDeleteSeededGlobalPolicy);
        }
        let versions =
            st.policies
                .get_mut(&policy_id)
                .ok_or_else(|| StorageError::PolicyNotFound {
                    policy_id: policy_id.clone(),
                })?;
        let Some(active) = versions
            .iter_mut()
            .find(|v| v.state == PolicyVersionState::Active)
        else {
            return Ok(TransitionOutcome::NoOp(()));
        };
        active.state = PolicyVersionState::Deleted;
        let version = active.version;
        st.policy_audit.push(PolicyTransitionAudit {
            policy_id,
            version,
            kind: "delete",
            actor: actor.to_string(),
            comment,
        });
        Self::push_events(&mut st, actor, events);
        Ok(TransitionOutcome::Applied(()))
    }

    async fn read_policy(
        &self,
        scope: &PolicyScope,
    ) -> Result<Option<PolicyVersion>, StorageError> {
        let st = self.state.lock();
        Self::check(&st)?;
        Ok(st
            .policies
            .values()
            .filter_map(|v| Self::active_version(v))
            .find(|v| &v.scope == scope)
            .cloned())
    }

    async fn read_active_policy_by_id(
        &self,
        policy_id: &PolicyId,
    ) -> Result<Option<PolicyVersion>, StorageError> {
        let st = self.state.lock();
        Self::check(&st)?;
        Ok(st
            .policies
            .get(policy_id)
            .and_then(|versions| Self::active_version(versions))
            .cloned())
    }

    async fn read_active_policies(&self) -> Result<Vec<PolicyVersion>, StorageError> {
        let st = self.state.lock();
        Self::check(&st)?;
        Ok(st
            .policies
            .values()
            .filter_map(|versions| Self::active_version(versions))
            .cloned()
            .collect())
    }

    async fn read_policy_version(
        &self,
        policy_id: &PolicyId,
        version: u32,
    ) -> Result<Option<PolicyVersion>, StorageError> {
        let st = self.state.lock();
        Self::check(&st)?;
        Ok(st
            .policies
            .get(policy_id)
            .and_then(|v| v.iter().find(|p| p.version == version))
            .cloned())
    }

    async fn list_policy_versions(
        &self,
        policy_id: &PolicyId,
        page: PageRequest,
    ) -> Result<PageResult<PolicyVersionMeta>, StorageError> {
        let st = self.state.lock();
        Self::check(&st)?;
        if !st.policies.contains_key(policy_id) {
            return Err(StorageError::PolicyNotFound {
                policy_id: policy_id.clone(),
            });
        }
        let items: Vec<PolicyVersionMeta> = st
            .policies
            .get(policy_id)
            .map(|versions| {
                versions
                    .iter()
                    .map(|v| PolicyVersionMeta {
                        version: v.version,
                        state: v.state,
                        created_at: v.created_at,
                        created_by: v.created_by.clone(),
                        comment: v.comment.clone(),
                    })
                    .collect()
            })
            .unwrap_or_default();
        Self::paginate(&items, &page)
    }

    async fn lookup_idempotency(
        &self,
        scope: &IdempotencyScope,
    ) -> Result<Option<IdempotencyRecord>, StorageError> {
        let mut st = self.state.lock();
        Self::check(&st)?;
        if st.hide_lookups {
            return Ok(None);
        }
        if st.hidden_lookups > 0 {
            st.hidden_lookups -= 1;
            return Ok(None);
        }
        Ok(Self::stored(&st, scope).cloned())
    }

    async fn count_expired_unreclaimed_leases(
        &self,
        before: OffsetDateTime,
    ) -> Result<Vec<(MetricId, u64)>, StorageError> {
        let st = self.state.lock();
        Self::check(&st)?;
        // `MetricId` is not `Ord`, and a backlog spans few metrics, so a linear
        // tally is both correct and cheap enough for a double.
        let mut by_metric: Vec<(MetricId, u64)> = Vec::new();
        for lease in st.leases.values() {
            if lease.state != LeaseState::Active || lease.expires_at > before {
                continue;
            }
            match by_metric.iter_mut().find(|(m, _)| *m == lease.metric) {
                Some((_, count)) => *count += 1,
                None => by_metric.push((lease.metric.clone(), 1)),
            }
        }
        Ok(by_metric)
    }

    async fn reclaim_expired_leases(
        &self,
        batch_size: u32,
        before: OffsetDateTime,
    ) -> Result<Vec<ExpiredLease>, StorageError> {
        self.transact(|st| {
            let mut reclaimed = Vec::new();
            let mut returns = Vec::new();
            for (token, lease) in &mut st.leases {
                if reclaimed.len() >= batch_size as usize {
                    break;
                }
                if lease.state != LeaseState::Active || lease.expires_at > before {
                    continue;
                }
                lease.state = LeaseState::AutoReleased;
                // Only what nobody has given back yet: a writer that met this
                // hold first already returned it, and returning it twice would
                // credit capacity that was never held.
                for held in &mut lease.holds {
                    if held.returned {
                        continue;
                    }
                    held.returned = true;
                    returns.push(AppliedEntry {
                        quota_id: held.hold.quota_id,
                        period_id: held.hold.period_id,
                        amount: held.hold.held_amount,
                        slot: held.slot,
                    });
                }
                reclaimed.push(ExpiredLease {
                    token: *token,
                    tenant_id: lease.tenant_id,
                    holds: lease.holds.iter().map(|held| held.hold.clone()).collect(),
                    expired_at: lease.expires_at,
                });
            }
            for entry in &returns {
                Self::lower_counter(st, entry);
            }
            // The sweeper is the canonical emission point for the
            // auto-release event, whichever writer returned the capacity.
            for lease in &reclaimed {
                let held: u64 = lease.holds.iter().map(|hold| hold.held_amount).sum();
                let quota_id = lease.holds.first().map(|hold| hold.quota_id);
                let subject = quota_id
                    .and_then(|id| st.quotas.get(&id))
                    .map(|quota| quota.subject.clone());
                let event = NotificationEvent {
                    event_id: EventId::generate(),
                    kind: NotificationEventKind::LeaseAutoReleased,
                    actor: Actor::Task {
                        task: crate::models::BackgroundTask::LeaseSweeper,
                    },
                    scope: NotificationScope::Tenant {
                        tenant_id: lease.tenant_id,
                    },
                    quota_id,
                    policy_id: None,
                    subject,
                    payload: serde_json::json!({
                        "lease_token": lease.token,
                        "held_amount": held,
                        "affected_quotas": lease
                            .holds
                            .iter()
                            .map(|hold| hold.quota_id)
                            .collect::<Vec<_>>(),
                        "expired_at": lease.expired_at,
                    }),
                    emitted_at: lease.expired_at,
                };
                st.events.push(event);
            }
            Ok(reclaimed)
        })
    }

    async fn reclaim_expired_idempotency(
        &self,
        batch_size: u32,
        before: OffsetDateTime,
    ) -> Result<u64, StorageError> {
        let mut st = self.state.lock();
        Self::check(&st)?;
        let victims: Vec<IdempotencyScope> = st
            .idempotency
            .iter()
            .filter(|(_, r)| r.expires_at <= before)
            .take(batch_size as usize)
            .map(|(k, _)| k.clone())
            .collect();
        for scope in &victims {
            st.idempotency.remove(scope);
        }
        Ok(victims.len() as u64)
    }

    async fn start_notification_delivery(
        &self,
        delivery: Arc<dyn NotificationDeliveryV1>,
    ) -> Result<Box<dyn NotificationDeliveryHandle>, StorageError> {
        let mut state = self.delivery.lock();
        if state.started {
            return Err(StorageError::Internal(
                "the notification pipeline is already started".to_owned(),
            ));
        }
        state.started = true;
        state.callback = Some(delivery);
        Ok(Box::new(DeliveryHandle {
            delivery: Arc::clone(&self.delivery),
            drain: Arc::clone(&self.drain),
        }))
    }

    async fn reclaim_operation_log(
        &self,
        batch_size: u32,
        before: OffsetDateTime,
    ) -> Result<u64, StorageError> {
        let mut st = self.state.lock();
        Self::check(&st)?;
        let before_len = st.log.len();
        let mut removed = 0_usize;
        st.log.retain(|entry| {
            if removed < batch_size as usize && entry.at <= before {
                removed += 1;
                false
            } else {
                true
            }
        });
        Ok((before_len - st.log.len()) as u64)
    }

    async fn reclaim_expired_charges(
        &self,
        batch_size: u32,
        now: OffsetDateTime,
    ) -> Result<u64, StorageError> {
        let mut st = self.state.lock();
        Self::check(&st)?;
        let deleted: Vec<IdempotencyScope> = st
            .charges
            .iter()
            .filter(|(_, charge)| !Self::charge_live(&st, charge, now))
            .take(batch_size as usize)
            .map(|(scope, _)| scope.clone())
            .collect();
        for scope in &deleted {
            st.charges.remove(scope);
        }
        let charges = std::mem::take(&mut st.charges);
        st.returns.retain(|_, row| {
            charges
                .get(&row.charge)
                .is_some_and(|charge| charge.id == row.charge_id)
        });
        st.charges = charges;
        Ok(deleted.len() as u64)
    }

    async fn read_held_allocation_charge_stats(
        &self,
        before: OffsetDateTime,
    ) -> Result<Vec<HeldAllocationCharges>, StorageError> {
        let st = self.state.lock();
        Self::check(&st)?;
        let mut stats: Vec<HeldAllocationCharges> = Vec::new();
        for charge in st.charges.values() {
            if charge.record.expires_at > before || !Self::holds_active_allocation(&st, charge) {
                continue;
            }
            let created = charge.record.created_at;
            match stats.iter_mut().find(|row| row.metric == charge.metric) {
                Some(row) => {
                    row.count += 1;
                    row.oldest_created_at = row.oldest_created_at.min(created);
                }
                None => stats.push(HeldAllocationCharges {
                    metric: charge.metric.clone(),
                    count: 1,
                    oldest_created_at: created,
                }),
            }
        }
        Ok(stats)
    }
}

fn new_version(policy_id: PolicyId, version: u32, draft: PolicyDraft) -> PolicyVersion {
    PolicyVersion {
        schema_snapshot: draft.schema_snapshot,
        policy_id,
        version,
        scope: draft.scope,
        engine_id: draft.engine_id,
        engine_config: draft.engine_config,
        timeout_ms: draft.timeout_ms,
        description: draft.description,
        state: PolicyVersionState::Active,
        created_at: OffsetDateTime::now_utc(),
        created_by: draft.created_by,
        comment: draft.comment,
    }
}

/// Convenience: a JSON `null` engine config for policy drafts in tests.
#[must_use]
pub fn empty_engine_config() -> Value {
    Value::Object(serde_json::Map::new())
}

/// Engine identifier of the policy the fixtures seed. Deliberately not
/// `most-restrictive-wins`: that engine's plan invariant is checked inside
/// [`crate::engine::EvaluationOutcome::validate`] and would constrain what a scripted
/// evaluator may return.
pub const TEST_ENGINE_ID: &str = "test-engine";

/// A bootstrap bundle carrying a global policy, so an evaluated mutation has
/// something to select inside its transaction.
#[must_use]
pub fn bundle_with_global_policy() -> BootstrapBundle {
    let mut bundle = BootstrapBundle::foundation();
    bundle.global_policy = Some(PolicyDraft {
        schema_snapshot: crate::engine::PolicySchemaSnapshot::default(),
        scope: PolicyScope::Global,
        engine_id: TEST_ENGINE_ID.to_owned(),
        engine_config: empty_engine_config(),
        timeout_ms: None,
        description: None,
        comment: None,
        created_by: "bootstrap".to_owned(),
    });
    bundle
}

/// A stand-in for the gear's prepared-artifact callback, for tests that
/// exercise the transaction convention rather than an engine.
///
/// It debits every applicable Quota that has room by the full requested
/// amount, and denies naming the Quotas that do not. Constructed with a
/// preparation-miss count, it reports [`EvaluationFailure::PreparationRequired`]
/// that many times first, so a caller's retry budget can be exercised.
#[derive(Debug, Default)]
pub struct ScriptedEvaluator {
    misses: Mutex<u32>,
    calls: Mutex<u32>,
    timeouts: Mutex<Vec<Duration>>,
}

impl ScriptedEvaluator {
    /// An evaluator that decides on its first call.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// An evaluator whose first `misses` calls ask for preparation.
    #[must_use]
    pub fn with_preparation_misses(misses: u32) -> Self {
        Self {
            misses: Mutex::new(misses),
            calls: Mutex::new(0),
            timeouts: Mutex::default(),
        }
    }

    /// The wall-time bound of every budget the transaction handed this
    /// callback, in call order. A caller cannot resolve it: it belongs to the
    /// version the transaction selected.
    #[must_use]
    pub fn observed_timeouts(&self) -> Vec<Duration> {
        self.timeouts.lock().clone()
    }

    /// How many times the transaction has invoked this callback.
    #[must_use]
    pub fn calls(&self) -> u32 {
        *self.calls.lock()
    }

    /// Decide, or ask for preparation while the scripted miss budget lasts.
    ///
    /// # Errors
    /// Returns [`EvaluationFailure::PreparationRequired`] for a scripted miss
    /// and the plan invariant for a decision the shared boundary refuses.
    pub fn evaluate(
        &self,
        context: &EvaluationContext<'_>,
    ) -> Result<crate::engine::EvaluationOutcome, EvaluationFailure> {
        *self.calls.lock() += 1;
        self.timeouts.lock().push(context.budget.timeout());
        {
            let mut misses = self.misses.lock();
            if *misses > 0 {
                *misses -= 1;
                return Err(EvaluationFailure::PreparationRequired {
                    policy_id: context.policy.policy_id.clone(),
                    version: context.policy.version,
                });
            }
        }
        if context.quotas.is_empty() {
            // Nothing applies: a denial, not an empty allowance.
            return Ok(crate::engine::EvaluationOutcome::validate(
                Decision {
                    result: DecisionResult::Denied {
                        violated_quota_ids: Vec::new(),
                        reason: "NO_APPLICABLE_QUOTA".to_owned(),
                    },
                    debit_plan: DebitPlan::new(),
                    diagnostics: BTreeMap::new(),
                },
                context,
            )?);
        }
        let violated: Vec<QuotaId> = context
            .quotas
            .iter()
            .filter(|q| {
                q.snapshot
                    .remaining
                    .is_some_and(|left| left < context.amount)
            })
            .map(|q| q.snapshot.quota_id)
            .collect();
        let decision = if violated.is_empty() {
            Decision {
                result: DecisionResult::Allowed,
                debit_plan: context
                    .quotas
                    .iter()
                    .map(|q| {
                        vec![crate::models::QuotaDebitPlan {
                            quota_id: q.snapshot.quota_id,
                            amount: context.amount,
                        }]
                    })
                    .collect(),
                diagnostics: BTreeMap::new(),
            }
        } else {
            Decision {
                result: DecisionResult::Denied {
                    violated_quota_ids: violated,
                    reason: "QUOTA_EXCEEDED".to_owned(),
                },
                debit_plan: DebitPlan::new(),
                diagnostics: BTreeMap::new(),
            }
        };
        Ok(crate::engine::EvaluationOutcome::validate(
            decision, context,
        )?)
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "testing_tests.rs"]
mod testing_tests;
