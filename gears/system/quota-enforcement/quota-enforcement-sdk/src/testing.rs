//! Test doubles for the plugin contract. Enabled with the `test-util` feature.
//!
//! [`InMemoryStorage`] implements every method of
//! [`QuotaEnforcementStoragePluginV1`] with simple in-memory semantics, so the
//! gear's bootstrap and readiness paths run against a complete contract
//! (foundation `DoD`, "Workspace and Crate Skeletons").
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

use std::collections::{BTreeMap, HashMap, HashSet};
use std::time::Duration;

use async_trait::async_trait;
use gts::GtsTypeId;
use parking_lot::Mutex;
use serde_json::Value;
use time::OffsetDateTime;
use toolkit_security::{AccessScope, SecurityContext};
use uuid::Uuid;

use crate::engine::{EvaluationContext, EvaluationFailure, EvaluationQuota, QuotaScopeTier};
use crate::models::{
    ActiveQuotaCounts, ApplicableQuotas, BootstrapBundle, CapPatch, ConfigDefaults, ContractRef,
    DeactivateOutcome, DebitPlan, Decision, DecisionResult, EnforcementMode, EvaluatedDebit,
    EvaluatedLease, EventId, ExpiredLease, IdempotencyRecord, IdempotencyScope, IdempotencyWrite,
    LeaseHold, LeaseState, LeaseToken, MetricId, MutationResult, NotificationEvent, PageRequest,
    PageResult, PolicyDraft, PolicyId, PolicyScope, PolicyUpdate, PolicyVersion, PolicyVersionMeta,
    PolicyVersionState, ProjectionBinding, Quota, QuotaDraft, QuotaFilter, QuotaId, QuotaPatch,
    QuotaSnapshot, QuotaSource, QuotaStatus, QuotaType, SubjectRef, TenantId, TransitionOutcome,
    ValidityWindowPatch,
};
use crate::storage_plugin::{
    CONTRACT_MAJOR, EvaluatedBatch, EvaluatedMutation, QuotaEnforcementStoragePluginV1,
    StorageError,
};

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// A fixed tenant for tests.
#[must_use]
pub fn test_tenant() -> TenantId {
    TenantId::new(Uuid::from_u128(0x7e57_0000_0000_0000_0000_0000_0000_0001))
}

/// A registered-looking metric instance id.
#[must_use]
pub fn test_metric() -> MetricId {
    MetricId::parse("gts.cf.qe.metric.type.v1~cf.genai.llm_gateway.token.v1")
        .expect("well-formed metric id")
}

/// A user-scope subject under a test owner projection.
#[must_use]
pub fn test_subject(subject_id: &str) -> SubjectRef {
    SubjectRef {
        projection_type: GtsTypeId::new("gts.cf.core.qe.subj.v1~cf.genai.llm_gateway.user.v1~"),
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
        constraint_contract: ContractRef {
            type_id: GtsTypeId::new(
                "gts.cf.core.qe.constraint.v1~cf.genai.llm_gateway.token_constraint.v1~",
            ),
            version: 1,
        },
    }
}

// ---------------------------------------------------------------------------
// Storage double
// ---------------------------------------------------------------------------

struct LeaseRow {
    tenant_id: TenantId,
    metric: MetricId,
    subject_key: crate::models::IdempotencySubjectKey,
    holds: Vec<LeaseHold>,
    state: LeaseState,
    expires_at: OffsetDateTime,
}

struct LogEntry {
    at: OffsetDateTime,
}

#[derive(Default)]
struct StorageState {
    installed_major: Option<u32>,
    bootstrapped: Option<BootstrapBundle>,
    bootstrap_calls: usize,
    defaults: Option<ConfigDefaults>,
    quotas: BTreeMap<QuotaId, Quota>,
    consumed: BTreeMap<QuotaId, u64>,
    leases: BTreeMap<LeaseToken, LeaseRow>,
    idempotency: HashMap<IdempotencyScope, IdempotencyRecord>,
    policies: BTreeMap<PolicyId, Vec<PolicyVersion>>,
    events: Vec<NotificationEvent>,
    policy_audit: Vec<PolicyTransitionAudit>,
    log: Vec<LogEntry>,
    failure: Option<StorageError>,
}

/// What one atomic envelope decided: the policy its items selected, their
/// decisions in submission order, and whether the batch committed. A batch that
/// did not commit leaves the counters exactly as it found them.
struct BatchRun {
    policy: Option<PolicyVersion>,
    decisions: Vec<Decision>,
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
    /// Actor from the authenticated security context.
    pub actor: String,
    /// Transition-specific operator comment.
    pub comment: Option<String>,
}

/// Complete in-memory [`QuotaEnforcementStoragePluginV1`].
pub struct InMemoryStorage {
    state: Mutex<StorageState>,
}

impl Default for InMemoryStorage {
    fn default() -> Self {
        Self::new()
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
        }
    }

    /// A backend that reports another installed schema major (I12 tests).
    #[must_use]
    pub fn with_installed_schema_major(major: u32) -> Self {
        let this = Self::new();
        this.state.lock().installed_major = Some(major);
        this
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

    /// Consumed or in-flight amount of a Quota.
    #[must_use]
    pub fn consumed(&self, id: QuotaId) -> u64 {
        self.state.lock().consumed.get(&id).copied().unwrap_or(0)
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

    fn push_events(st: &mut StorageState, events: &[NotificationEvent]) -> Vec<EventId> {
        st.events.extend_from_slice(events);
        st.log.push(LogEntry {
            at: OffsetDateTime::now_utc(),
        });
        events.iter().map(|e| e.event_id).collect()
    }

    /// What an earlier transaction recorded under this key, if any. A replay
    /// carrying a different payload is I2, not a second mutation.
    fn replayed(
        st: &StorageState,
        write: &IdempotencyWrite,
    ) -> Result<Option<Value>, StorageError> {
        let Some(existing) = st.idempotency.get(&write.scope) else {
            return Ok(None);
        };
        if existing.payload_hash != write.payload_hash {
            return Err(StorageError::IdempotencyPayloadMismatch);
        }
        Ok(Some(existing.decision_blob.clone()))
    }

    /// Record what this transaction decided, attributed to the policy it
    /// selected. A primitive that evaluates nothing records no attribution.
    fn remember(
        st: &mut StorageState,
        write: &IdempotencyWrite,
        blob: Value,
        policy: Option<&PolicyVersion>,
    ) {
        let retention = st.defaults.map_or(86_400, |d| d.idempotency_retention_secs);
        let now = OffsetDateTime::now_utc();
        st.idempotency.insert(
            write.scope.clone(),
            IdempotencyRecord {
                scope: write.scope.clone(),
                payload_hash: write.payload_hash,
                decision_blob: blob,
                engine_id: policy.map(|p| p.engine_id.clone()),
                policy_id: policy.map(|p| p.policy_id.clone()),
                policy_version: policy.map(|p| p.version),
                created_at: now,
                expires_at: now + Duration::from_secs(retention),
            },
        );
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
        let policy = Self::select_policy(st, &mutation.applicable.metric)?;
        let snapshots: Vec<QuotaSnapshot> = st
            .quotas
            .values()
            .filter(|q| Self::matches(q, mutation.applicable))
            .map(|q| Self::snapshot(st, q))
            .collect();
        let arbitration: Vec<Value> = snapshots
            .iter()
            .map(|s| Value::Object(s.metadata.clone()))
            .collect();
        let quotas: Vec<EvaluationQuota<'_>> = snapshots
            .iter()
            .zip(&arbitration)
            .map(|(snapshot, arbitration)| EvaluationQuota {
                snapshot,
                tier: match mutation.user_projection {
                    Some(user) if &snapshot.subject.projection_type == user => QuotaScopeTier::User,
                    _ => QuotaScopeTier::Tenant,
                },
                arbitration,
            })
            .collect();
        // The budget belongs to the version this transaction selected, so it is
        // resolved here rather than by a caller that could not have known it.
        let budget = mutation.limits.budget(policy.timeout_ms).map_err(|error| {
            StorageError::EvaluationFailed {
                engine_id: policy.engine_id.clone(),
                failure: error.into(),
            }
        })?;
        let decision = {
            let context = EvaluationContext {
                policy: &policy,
                metric: &mutation.applicable.metric,
                amount: mutation.amount,
                time: OffsetDateTime::now_utc(),
                quotas: &quotas,
                request: mutation.request,
                resource: mutation.resource,
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

    /// Evaluate and apply an envelope's items in submission order, each against
    /// the counters its predecessors moved. Stops at the first denial and
    /// reports that the batch did not commit; the caller restores the counters,
    /// so nothing an earlier item applied survives a later refusal.
    fn run_batch(
        st: &mut StorageState,
        batch: &EvaluatedBatch<'_>,
    ) -> Result<BatchRun, StorageError> {
        let mut decisions = Vec::with_capacity(batch.items.len());
        let mut policy = None;
        for item in batch.items {
            let idempotency = IdempotencyWrite {
                scope: item
                    .item_scope
                    .clone()
                    .unwrap_or_else(|| batch.envelope.scope.clone()),
                payload_hash: batch.envelope.payload_hash,
            };
            let (selected, decision) = Self::evaluated(
                st,
                &EvaluatedMutation {
                    applicable: &item.applicable,
                    amount: item.amount,
                    request: &item.request,
                    resource: &item.resource,
                    user_projection: batch.user_projection,
                    limits: batch.limits,
                    idempotency: &idempotency,
                    evaluate: batch.evaluate,
                },
            )?;
            policy = Some(selected);
            let denied = matches!(decision.result, DecisionResult::Denied { .. });
            if !denied {
                Self::debit(st, &decision.debit_plan)?;
            }
            decisions.push(decision);
            if denied {
                return Ok(BatchRun {
                    policy,
                    decisions,
                    committed: false,
                });
            }
        }
        Ok(BatchRun {
            policy,
            decisions,
            committed: true,
        })
    }

    /// Apply a validated plan to the counters, refusing a deactivated Quota
    /// before the first mutation.
    fn debit(st: &mut StorageState, plan: &DebitPlan) -> Result<(), StorageError> {
        for id in plan.keys() {
            Self::active_quota(st, *id)?;
        }
        for (id, entry) in plan {
            let counter = st.consumed.entry(*id).or_insert(0);
            *counter = counter.saturating_add(entry.amount);
        }
        Ok(())
    }

    fn snapshot_counters(st: &StorageState, plan: &DebitPlan) -> MutationResult {
        MutationResult {
            counters: plan
                .keys()
                .map(|id| crate::models::CounterSnapshot {
                    quota_id: *id,
                    period_id: None,
                    value: st.consumed.get(id).copied().unwrap_or(0),
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
        let consumed = st.consumed.get(&quota.id).copied().unwrap_or(0);
        let now = OffsetDateTime::now_utc();
        QuotaSnapshot {
            quota_id: quota.id,
            subject: quota.subject.clone(),
            metric: quota.metric.clone(),
            quota_type: quota.quota_type,
            enforcement_mode: quota.enforcement_mode,
            cap: quota.cap,
            consumed,
            remaining: quota.cap.map(|cap| cap.saturating_sub(consumed)),
            period: None,
            metadata: quota.metadata.clone(),
            validity_window: quota.validity_window,
            currently_within_window: quota.validity_window.is_none_or(|w| w.contains(now)),
        }
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

    async fn read_active_projection_bindings(
        &self,
    ) -> Result<HashSet<ProjectionBinding>, StorageError> {
        let st = self.state.lock();
        Self::check(&st)?;
        Ok(st
            .quotas
            .values()
            .filter(|quota| quota.status == QuotaStatus::Active)
            .map(|quota| ProjectionBinding {
                metric: quota.metric.clone(),
                projection_type: quota.subject.projection_type.clone(),
            })
            .collect())
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
        }
        Ok(counts)
    }

    async fn create_quota(
        &self,
        _ctx: &SecurityContext,
        _scope: &AccessScope,
        draft: QuotaDraft,
        events: &[NotificationEvent],
    ) -> Result<QuotaId, StorageError> {
        let mut st = self.state.lock();
        Self::check(&st)?;
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
                constraint_contract: draft.constraint_contract,
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
        Self::push_events(&mut st, &events);
        Ok(id)
    }

    async fn update_quota(
        &self,
        _ctx: &SecurityContext,
        _scope: &AccessScope,
        quota_id: QuotaId,
        patch: QuotaPatch,
        events: &[NotificationEvent],
    ) -> Result<Quota, StorageError> {
        let mut st = self.state.lock();
        Self::check(&st)?;
        Self::active_quota(&st, quota_id)?;
        let consumed = st.consumed.get(&quota_id).copied().unwrap_or(0);
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
            let contract = patch.constraint_contract.ok_or_else(|| {
                StorageError::Internal(
                    "metadata patch without the contract it was validated against".to_owned(),
                )
            })?;
            quota.metadata = metadata;
            quota.constraint_contract = contract;
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
        Self::push_events(&mut st, events);
        Ok(updated)
    }

    async fn deactivate_quota(
        &self,
        _ctx: &SecurityContext,
        _scope: &AccessScope,
        quota_id: QuotaId,
        events: &[NotificationEvent],
    ) -> Result<DeactivateOutcome, StorageError> {
        let mut st = self.state.lock();
        Self::check(&st)?;
        Self::active_quota(&st, quota_id)?;
        let now = OffsetDateTime::now_utc();
        let quota = st
            .quotas
            .get_mut(&quota_id)
            .ok_or(StorageError::QuotaNotFound { id: quota_id })?;
        quota.status = QuotaStatus::Deactivated;
        quota.record_version += 1;
        quota.updated_at = now;
        // The cascade: every live lease holding this Quota is resolved and its
        // held capacity returned. Expired leases are already released (I4).
        let mut resolved = Vec::new();
        let mut returned: Vec<LeaseHold> = Vec::new();
        for (token, lease) in &mut st.leases {
            if lease.state == LeaseState::Active
                && lease.expires_at > now
                && lease.holds.iter().any(|h| h.quota_id == quota_id)
            {
                lease.state = LeaseState::ResolvedByDeactivation;
                resolved.push(*token);
                returned.extend(lease.holds.iter().cloned());
            }
        }
        for hold in &returned {
            let counter = st.consumed.entry(hold.quota_id).or_insert(0);
            *counter = counter.saturating_sub(hold.held_amount);
        }
        Self::push_events(&mut st, events);
        Ok(DeactivateOutcome {
            resolved_leases: resolved,
        })
    }

    async fn read_quotas(
        &self,
        _ctx: &SecurityContext,
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
        Self::paginate(&items, &page)
    }

    async fn apply_debit_plan(
        &self,
        _ctx: &SecurityContext,
        _scope: &AccessScope,
        mutation: &EvaluatedMutation<'_>,
        events: &[NotificationEvent],
    ) -> Result<TransitionOutcome<EvaluatedDebit>, StorageError> {
        let mut st = self.state.lock();
        Self::check(&st)?;
        if let Some(blob) = Self::replayed(&st, mutation.idempotency)? {
            let decision = Self::decision_from(blob)?;
            let counters = Self::snapshot_counters(&st, &decision.debit_plan);
            return Ok(TransitionOutcome::NoOp(EvaluatedDebit {
                decision,
                mutation: counters,
            }));
        }
        let (policy, decision) = Self::evaluated(&st, mutation)?;
        Self::debit(&mut st, &decision.debit_plan)?;
        let mut counters = Self::snapshot_counters(&st, &decision.debit_plan);
        let blob = Self::blob(&decision)?;
        Self::remember(&mut st, mutation.idempotency, blob, Some(&policy));
        counters.event_ids = Self::push_events(&mut st, events);
        Ok(TransitionOutcome::Applied(EvaluatedDebit {
            decision,
            mutation: counters,
        }))
    }

    async fn apply_batch_debit(
        &self,
        _ctx: &SecurityContext,
        _scope: &AccessScope,
        batch: &EvaluatedBatch<'_>,
        events: &[NotificationEvent],
    ) -> Result<TransitionOutcome<Vec<EvaluatedDebit>>, StorageError> {
        let mut st = self.state.lock();
        Self::check(&st)?;
        if let Some(blob) = Self::replayed(&st, batch.envelope)? {
            let decisions: Vec<Decision> =
                serde_json::from_value(blob).map_err(|e| StorageError::Internal(e.to_string()))?;
            return Ok(TransitionOutcome::NoOp(
                decisions
                    .into_iter()
                    .map(|decision| {
                        let counters = Self::snapshot_counters(&st, &decision.debit_plan);
                        EvaluatedDebit {
                            decision,
                            mutation: counters,
                        }
                    })
                    .collect(),
            ));
        }
        // Each item's evaluation must see the counters every earlier item of the
        // same batch moved, so items are applied as they are decided. The
        // envelope is all-or-nothing: one denial restores every counter this
        // batch touched and the batch as a whole is denied.
        let restore = st.consumed.clone();
        let run = match Self::run_batch(&mut st, batch) {
            Ok(run) => run,
            Err(error) => {
                st.consumed = restore;
                return Err(error);
            }
        };
        if !run.committed {
            st.consumed = restore;
        }
        let applied: Vec<EvaluatedDebit> = run
            .decisions
            .into_iter()
            .map(|decision| {
                let counters = Self::snapshot_counters(&st, &decision.debit_plan);
                EvaluatedDebit {
                    decision,
                    mutation: counters,
                }
            })
            .collect();
        let blob = Self::blob(
            &applied
                .iter()
                .map(|item| item.decision.clone())
                .collect::<Vec<_>>(),
        )?;
        // A denied envelope still occupies its key: the replay of a denial is a
        // denial, and no counter moved either time.
        Self::remember(&mut st, batch.envelope, blob, run.policy.as_ref());
        if run.committed {
            Self::push_events(&mut st, events);
        }
        Ok(TransitionOutcome::Applied(applied))
    }

    async fn apply_credit(
        &self,
        _ctx: &SecurityContext,
        _scope: &AccessScope,
        quota_id: QuotaId,
        amount: u64,
        idempotency: &IdempotencyWrite,
        events: &[NotificationEvent],
    ) -> Result<MutationResult, StorageError> {
        let mut st = self.state.lock();
        Self::check(&st)?;
        let plan: DebitPlan =
            BTreeMap::from([(quota_id, crate::models::QuotaDebitPlan { amount })]);
        if let Some(blob) = Self::replayed(&st, idempotency)? {
            return Ok(Self::snapshot_counters(
                &st,
                &Self::decision_from(blob)?.debit_plan,
            ));
        }
        Self::active_quota(&st, quota_id)?;
        let counter = st.consumed.entry(quota_id).or_insert(0);
        *counter = counter.saturating_sub(amount);
        let mut result = Self::snapshot_counters(&st, &plan);
        // A credit evaluates no policy, so its record carries no attribution.
        let blob = Self::blob(&Self::applied(plan))?;
        Self::remember(&mut st, idempotency, blob, None);
        result.event_ids = Self::push_events(&mut st, events);
        Ok(result)
    }

    async fn apply_rollback(
        &self,
        _ctx: &SecurityContext,
        _scope: &AccessScope,
        original: &IdempotencyScope,
        idempotency: &IdempotencyWrite,
        events: &[NotificationEvent],
    ) -> Result<MutationResult, StorageError> {
        let mut st = self.state.lock();
        Self::check(&st)?;
        let plan: DebitPlan = {
            let record = st
                .idempotency
                .get(original)
                .ok_or_else(|| StorageError::Internal("original operation unknown".to_owned()))?;
            Self::decision_from(record.decision_blob.clone())?.debit_plan
        };
        if let Some(blob) = Self::replayed(&st, idempotency)? {
            return Ok(Self::snapshot_counters(
                &st,
                &Self::decision_from(blob)?.debit_plan,
            ));
        }
        for (id, entry) in &plan {
            let counter = st.consumed.entry(*id).or_insert(0);
            *counter = counter.saturating_sub(entry.amount);
        }
        let mut result = Self::snapshot_counters(&st, &plan);
        let blob = Self::blob(&Self::applied(plan))?;
        Self::remember(&mut st, idempotency, blob, None);
        result.event_ids = Self::push_events(&mut st, events);
        Ok(result)
    }

    async fn acquire_lease(
        &self,
        _ctx: &SecurityContext,
        _scope: &AccessScope,
        mutation: &EvaluatedMutation<'_>,
        ttl: Duration,
    ) -> Result<TransitionOutcome<EvaluatedLease>, StorageError> {
        let mut st = self.state.lock();
        Self::check(&st)?;
        if let Some(blob) = Self::replayed(&st, mutation.idempotency)? {
            // The acquisition's own outcome, token included. A subject may hold
            // several leases at once, so a replay cannot look one up by subject:
            // it would hand back an unrelated token, and a replayed denial
            // would acquire one it never held.
            let acquired: EvaluatedLease =
                serde_json::from_value(blob).map_err(|e| StorageError::Internal(e.to_string()))?;
            return Ok(TransitionOutcome::NoOp(acquired));
        }
        let now = OffsetDateTime::now_utc();
        let cap = st.defaults.map_or(1000, |d| d.max_active_leases) as usize;
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
        let (policy, decision) = Self::evaluated(&st, mutation)?;
        // A denied acquisition holds nothing, and still occupies the key: the
        // replay of a denial is a denial, not a second evaluation.
        let token = if decision.debit_plan.is_empty() {
            None
        } else {
            Self::debit(&mut st, &decision.debit_plan)?;
            let token = LeaseToken::generate();
            st.leases.insert(
                token,
                LeaseRow {
                    tenant_id: mutation.applicable.tenant_id,
                    metric: mutation.applicable.metric.clone(),
                    subject_key: mutation.idempotency.scope.subject_key,
                    holds: decision
                        .debit_plan
                        .iter()
                        .map(|(id, e)| LeaseHold {
                            quota_id: *id,
                            held_amount: e.amount,
                            period_id: None,
                        })
                        .collect(),
                    state: LeaseState::Active,
                    expires_at: now + ttl,
                },
            );
            Some(token)
        };
        let acquired = EvaluatedLease { decision, token };
        let blob = Self::blob(&acquired)?;
        Self::remember(&mut st, mutation.idempotency, blob, Some(&policy));
        Ok(TransitionOutcome::Applied(acquired))
    }

    async fn commit_lease(
        &self,
        _ctx: &SecurityContext,
        _scope: &AccessScope,
        token: LeaseToken,
        actual_amount: Option<u64>,
        idempotency: &IdempotencyWrite,
        events: &[NotificationEvent],
    ) -> Result<MutationResult, StorageError> {
        let mut st = self.state.lock();
        Self::check(&st)?;
        if Self::replayed(&st, idempotency)?.is_some() {
            return Ok(MutationResult::default());
        }
        let now = OffsetDateTime::now_utc();
        let holds = {
            let lease = st
                .leases
                .get_mut(&token)
                .filter(|l| l.state == LeaseState::Active && l.expires_at > now)
                .ok_or(StorageError::LeaseNotActive { token })?;
            let reserved: u64 = lease.holds.iter().map(|h| h.held_amount).sum();
            let actual = actual_amount.unwrap_or(reserved);
            if actual > reserved {
                return Err(StorageError::OverCommitNotAuthorized { reserved, actual });
            }
            lease.state = LeaseState::Committed;
            let unused = reserved - actual;
            let holds = lease.holds.clone();
            (holds, unused)
        };
        let (holds, mut unused) = holds;
        for hold in &holds {
            let give_back = unused.min(hold.held_amount);
            unused -= give_back;
            let counter = st.consumed.entry(hold.quota_id).or_insert(0);
            *counter = counter.saturating_sub(give_back);
        }
        let plan: DebitPlan = holds
            .iter()
            .map(|h| {
                (
                    h.quota_id,
                    crate::models::QuotaDebitPlan {
                        amount: h.held_amount,
                    },
                )
            })
            .collect();
        let mut result = Self::snapshot_counters(&st, &plan);
        // Settling a lease evaluates nothing: the plan was fixed at acquisition.
        let blob = Self::blob(&Self::applied(plan))?;
        Self::remember(&mut st, idempotency, blob, None);
        result.event_ids = Self::push_events(&mut st, events);
        Ok(result)
    }

    async fn release_lease(
        &self,
        _ctx: &SecurityContext,
        _scope: &AccessScope,
        token: LeaseToken,
        idempotency: &IdempotencyWrite,
        events: &[NotificationEvent],
    ) -> Result<MutationResult, StorageError> {
        let mut st = self.state.lock();
        Self::check(&st)?;
        if Self::replayed(&st, idempotency)?.is_some() {
            return Ok(MutationResult::default());
        }
        let now = OffsetDateTime::now_utc();
        let holds = {
            let lease = st
                .leases
                .get_mut(&token)
                .filter(|l| l.state == LeaseState::Active && l.expires_at > now)
                .ok_or(StorageError::LeaseNotActive { token })?;
            lease.state = LeaseState::Released;
            lease.holds.clone()
        };
        for hold in &holds {
            let counter = st.consumed.entry(hold.quota_id).or_insert(0);
            *counter = counter.saturating_sub(hold.held_amount);
        }
        let plan: DebitPlan = holds
            .iter()
            .map(|h| {
                (
                    h.quota_id,
                    crate::models::QuotaDebitPlan {
                        amount: h.held_amount,
                    },
                )
            })
            .collect();
        let mut result = Self::snapshot_counters(&st, &plan);
        let blob = Self::blob(&Self::applied(plan))?;
        Self::remember(&mut st, idempotency, blob, None);
        result.event_ids = Self::push_events(&mut st, events);
        Ok(result)
    }

    async fn read_quota_snapshot(
        &self,
        _ctx: &SecurityContext,
        _scope: &AccessScope,
        applicable: &ApplicableQuotas,
    ) -> Result<Vec<QuotaSnapshot>, StorageError> {
        let st = self.state.lock();
        Self::check(&st)?;
        Ok(st
            .quotas
            .values()
            .filter(|q| Self::matches(q, applicable))
            .map(|q| Self::snapshot(&st, q))
            .collect())
    }

    async fn bulk_read_quota_snapshot(
        &self,
        _ctx: &SecurityContext,
        _scope: &AccessScope,
        pairs: &[ApplicableQuotas],
        page: PageRequest,
    ) -> Result<PageResult<QuotaSnapshot>, StorageError> {
        let st = self.state.lock();
        Self::check(&st)?;
        let items: Vec<QuotaSnapshot> = st
            .quotas
            .values()
            .filter(|q| pairs.iter().any(|a| Self::matches(q, a)))
            .map(|q| Self::snapshot(&st, q))
            .collect();
        Self::paginate(&items, &page)
    }

    async fn create_policy(
        &self,
        ctx: &SecurityContext,
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
        draft.created_by = ctx.subject_id().to_string();
        let version = new_version(policy_id.clone(), 1, draft);
        st.policies.insert(policy_id.clone(), vec![version.clone()]);
        st.policy_audit.push(PolicyTransitionAudit {
            policy_id,
            version: 1,
            kind: "create",
            actor: ctx.subject_id().to_string(),
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
        Self::push_events(&mut st, &events);
        Ok(version)
    }

    async fn update_policy(
        &self,
        ctx: &SecurityContext,
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
        next.created_by = ctx.subject_id().to_string();
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
            actor: ctx.subject_id().to_string(),
            comment: next.comment.clone(),
        });
        Self::push_events(&mut st, events);
        Ok(next)
    }

    async fn rollback_policy(
        &self,
        ctx: &SecurityContext,
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
            actor: ctx.subject_id().to_string(),
            comment,
        });
        Self::push_events(&mut st, events);
        Ok(TransitionOutcome::Applied(result))
    }

    async fn delete_policy(
        &self,
        ctx: &SecurityContext,
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
            actor: ctx.subject_id().to_string(),
            comment,
        });
        Self::push_events(&mut st, events);
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
        let st = self.state.lock();
        Self::check(&st)?;
        Ok(st.idempotency.get(scope).cloned())
    }

    async fn reclaim_expired_leases(
        &self,
        batch_size: u32,
        before: OffsetDateTime,
    ) -> Result<Vec<ExpiredLease>, StorageError> {
        let mut st = self.state.lock();
        Self::check(&st)?;
        let mut reclaimed = Vec::new();
        for (token, lease) in &mut st.leases {
            if reclaimed.len() >= batch_size as usize {
                break;
            }
            if lease.state == LeaseState::Active && lease.expires_at <= before {
                lease.state = LeaseState::AutoReleased;
                reclaimed.push(ExpiredLease {
                    token: *token,
                    tenant_id: lease.tenant_id,
                    subject_key: lease.subject_key,
                    holds: lease.holds.clone(),
                    expired_at: lease.expires_at,
                });
            }
        }
        for lease in &reclaimed {
            for hold in &lease.holds {
                let counter = st.consumed.entry(hold.quota_id).or_insert(0);
                *counter = counter.saturating_sub(hold.held_amount);
            }
        }
        Ok(reclaimed)
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
/// [`EvaluationOutcome::validate`] and would constrain what a scripted
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
                        (
                            q.snapshot.quota_id,
                            crate::models::QuotaDebitPlan {
                                amount: context.amount,
                            },
                        )
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
