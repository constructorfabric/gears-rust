//! `ApprovalService` — the dual-control lifecycle engine. Owns the
//! `PENDING → APPROVED | REJECTED | NEEDS_REWORK | CANCELLED | EXPIRED` state
//! machine, the `preparer ≠ approver` rule, and the same-transaction decision
//! audit. It dispatches the governed mutation through an [`ApprovalExecutor`]
//! port so the lifecycle stays testable in isolation from the posting engine.
//!
//! **Latch-execute-mark (DC-impl, H2).** `PostingService` opens its own
//! serializable transaction, so `approve` cannot nest the mutation inside the
//! approval transaction. To keep the dual-control invariant "a rejected/cancelled
//! approval never executes", `approve` (0) atomically latches `PENDING →
//! APPROVING` in its own txn — once latched, `reject`/`cancel`/`request-changes`
//! (all keyed on the `PENDING` state) can no longer win; then (1) executes the
//! stored `intent` through the executor (an idempotent mutation in its own txn);
//! then (2) marks `APPROVING → APPROVED` + writes the decision audit. If the
//! mutation fails (its txn rolled back, nothing committed) the latch is reverted
//! to `PENDING` so the approval is actionable again. Idempotency covers the crash
//! windows: a crash after the latch (before/during/after execute, before the
//! mark) leaves the row `APPROVING`; a later approve recovers it — re-running the
//! mutation (the idempotency key short-circuits a committed post) and completing
//! the mark. Without the latch, a concurrent `reject` landing during execute would
//! leave the mutation committed but the approval `REJECTED`.
//!
//! **Audit (DC7).** No `secured_audit_record` writer exists on this base (Slice 6
//! brings it). The decision audit is recorded in the same transaction as the
//! state transition via the append-only `ledger_approval_comment` thread, carrying
//! a structured JSON body. When Slice 6 lands, add the `secured_audit_record`
//! write alongside this call in the same txn.

use std::sync::Arc;

use crate::domain::exact_money::{map_exact_error, map_money_error};
use crate::infra::approval::stored_intent::{
    SnapshotBasis, StoredThresholdSnapshot, canonical_identity, check_threshold_snapshot,
    decode_intent, encode_intent, validate_threshold_snapshot,
};
use crate::infra::currency_scale::CurrencyScaleResolver;
use crate::infra::storage::repo::approval_repo::{ApprovalRow, InsertPendingError};
use crate::infra::storage::repo::fx_repo::RateSnapshotRow;
use bss_ledger_sdk::{CurrencySpec, PostedMoney, SourceDocType};

use crate::infra::posting::error_transport::scale_error_to_domain;
use crate::infra::posting::retry::{AttemptError, RetryOp, retry_transaction_for};
use toolkit_db::secure::AccessScope;
use toolkit_db::{DBProvider, DbError};
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::config::FxConfig;
use crate::domain::approval::ApprovalState;
use crate::domain::approval::intent::ApprovalIntent;
use crate::domain::approval::policy::{
    D2_DEFAULT_RULE, D2Thresholds, DualControlPolicy, OperationFacts, PolicyConfigError,
    PolicyVersion, ValuationBasis, amount_gated, effective_version, requires_dual_control,
    resolve_policy, valuation_basis,
};
use crate::domain::error::DomainError;
use crate::domain::fx::translate::translate_amount;
use crate::domain::instant::format_rfc3339;
use crate::domain::instant::to_naive_date;
use crate::domain::ports::metrics::LedgerMetricsPort;
use crate::infra::fx::rate_source::RateSource;
use crate::infra::storage::repo::{
    ApprovalRepo, FxRepo, JournalRepo, NewPendingApproval, NewPolicyVersion, ReferenceRepo,
};
use time::Duration;
use time::OffsetDateTime;

/// The seam through which an approved governed mutation is actually executed.
/// `ApprovalService` reconstructs the [`ApprovalIntent`] from the stored row and
/// hands it here; the concrete adapter (wired in `module`) replays the inline
/// flow (reverse / credit-grant / chargeback) idempotently. Kept a port so the
/// lifecycle engine is unit-testable with a stub.
#[async_trait::async_trait]
pub trait ApprovalExecutor: Send + Sync {
    /// Execute the governed mutation captured by `intent`, idempotently (a replay
    /// short-circuits via the foundation idempotency key).
    ///
    /// # Errors
    /// A [`DomainError`] propagates the mutation's own rejection unchanged.
    async fn execute(
        &self,
        ctx: &SecurityContext,
        scope: &AccessScope,
        intent: &ApprovalIntent,
    ) -> Result<(), DomainError>;
}

/// Dual-control lifecycle engine. Cheap to clone (`Arc` + handle fields).
#[derive(Clone)]
pub struct ApprovalService {
    db: DBProvider<DbError>,
    repo: ApprovalRepo,
    executor: Arc<dyn ApprovalExecutor>,
    metrics: Arc<dyn LedgerMetricsPort>,
    // Preserve the existing eligible functional valuation before resolving the
    // currency-specific threshold; derived kinds retain transaction basis.
    source: RateSource,
    reference: ReferenceRepo,
    // D2 (FX): reads the OPERATION's locked rate (the referenced posted entry's
    // `rate_snapshot_ref`) so the threshold is valued at the operation's own rate,
    // not a fresh gate-time rate.
    journal: JournalRepo,
}

impl ApprovalService {
    #[must_use]
    pub fn new(
        db: DBProvider<DbError>,
        executor: Arc<dyn ApprovalExecutor>,
        metrics: Arc<dyn LedgerMetricsPort>,
        fx_config: FxConfig,
    ) -> Self {
        let repo = ApprovalRepo::new(db.clone());
        let source = RateSource::new(FxRepo::new(db.clone()), fx_config);
        let reference = ReferenceRepo::new(db.clone());
        let journal = JournalRepo::new(db.clone());
        Self {
            db,
            repo,
            executor,
            metrics,
            source,
            reference,
            journal,
        }
    }

    /// Create (or idempotently return) a `PENDING` approval for an over-threshold
    /// mutation. A retry with the same `(tenant, kind, business_key)` **and the
    /// same intent** returns the existing active record rather than a duplicate
    /// (DC13); a retry under that key carrying a *different* intent is refused,
    /// never silently answered with the other intent's approval.
    ///
    /// # Errors
    /// [`DomainError::ApprovalNotActionable`] when an active approval for the same
    /// `(tenant, kind, business_key)` captured a different intent;
    /// [`DomainError::Internal`] when the threshold snapshot is inconsistent with
    /// its own captured policy, or on a storage failure.
    #[allow(clippy::too_many_arguments)] // a pending record carries several snapshot fields
    pub async fn create_pending(
        &self,
        ctx: &SecurityContext,
        scope: &AccessScope,
        intent: ApprovalIntent,
        reason_code: String,
        threshold_snapshot: StoredThresholdSnapshot,
        amount: Option<PostedMoney>,
        ttl_seconds: i64,
    ) -> Result<Uuid, DomainError> {
        // Typed at the call site; serialized once, at the storage boundary.
        let threshold_snapshot =
            serde_json::to_value(check_threshold_snapshot(threshold_snapshot)?).map_err(|e| {
                DomainError::Internal(format!("encode approval policy snapshot: {e}"))
            })?;
        let tenant = ctx.subject_tenant_id();
        let kind = intent.kind();
        let business_key = intent.business_key();

        let now = OffsetDateTime::now_utc();
        if let Some(existing) = self
            .repo
            .read_active(scope, tenant, kind.as_str(), &business_key, now)
            .await?
        {
            ensure_same_intent(&existing, &intent)?;
            return Ok(existing.approval_id);
        }

        let approval_id = Uuid::now_v7();
        let prepared_at = now;
        let expires_at = prepared_at + Duration::seconds(ttl_seconds);
        let intent_json = encode_intent(&intent)?;
        let row = NewPendingApproval {
            approval_id,
            tenant,
            kind: kind.as_str().to_owned(),
            business_key: business_key.clone(),
            intent: intent_json,
            amount,
            threshold_snapshot,
            reason_code,
            prepared_by: ctx.subject_id(),
            prepared_at,
            correlation_id: Uuid::now_v7(),
            expires_at,
        };
        let scope_owned = scope.clone();
        let op = RetryOp::new("dual-control pending create", Some(tenant));
        let created = retry_transaction_for(&self.db.db(), op, move |txn| {
            let row = row.clone();
            let scope = scope_owned.clone();
            Box::pin(async move {
                // Lazy expiry pass (DC13/DC12): flip this tenant's lapsed
                // PENDING/NEEDS_REWORK rows to EXPIRED first, so an abandoned
                // approval past its TTL no longer occupies the active-uniqueness
                // slot and the insert below can claim it. This is the per-tenant
                // lazy complement to the cross-tenant TTL sweep job
                // (`expire_due_all`, wired in `module.rs`) — DC12 ships BOTH, so
                // a slot is freed on the next prepare even between sweep ticks.
                ApprovalRepo::expire_due(txn, &scope, tenant, now)
                    .await
                    .map_err(AttemptError::from)?;
                ApprovalRepo::insert_pending(txn, &scope, row)
                    .await
                    .map_err(pending_insert_error)?;
                Ok::<(), AttemptError>(())
            })
        })
        .await;
        if let Err(e) = created {
            // ONLY the DC13 active-uniqueness race is recoverable: a concurrent
            // preparer that both passed the read_active check and won the partial-
            // unique index, leaving this caller the loser (23505). The repository
            // names it (`InsertPendingError::ActiveExists`) and the attempt stops
            // at once, without retry sleeps (see `pending_insert_error`); exhausted
            // contention also lands here. Any OTHER error (connection drop, CHECK
            // violation, …) is a real failure — surface it, never mask it behind a
            // re-read. On a confirmed dup, return the winner idempotently.
            if matches!(e, DomainError::ConcurrentModification(_))
                && let Some(existing) = self
                    .repo
                    .read_active(
                        scope,
                        tenant,
                        kind.as_str(),
                        &business_key,
                        OffsetDateTime::now_utc(),
                    )
                    .await?
            {
                ensure_same_intent(&existing, &intent)?;
                return Ok(existing.approval_id);
            }
            return Err(e);
        }
        self.metrics.dual_control_pending(kind.as_str());
        Ok(approval_id)
    }

    /// The retrofit gate (Group E): resolve the tenant's effective policy and
    /// decide whether `facts` crosses the threshold. Over threshold → create a
    /// `PENDING` approval and return its id (the handler returns
    /// `409 DUAL_CONTROL_REQUIRED`); at/under threshold → `None` (the handler
    /// proceeds single-actor, unchanged).
    ///
    /// # Errors
    /// [`DomainError::Internal`] on a storage failure.
    pub async fn gate(
        &self,
        ctx: &SecurityContext,
        scope: &AccessScope,
        intent: ApprovalIntent,
        facts: OperationFacts,
        reason_code: String,
    ) -> Result<Option<Uuid>, DomainError> {
        let versions = self
            .repo
            .read_policy_versions(scope, ctx.subject_tenant_id())
            .await?;
        let now = OffsetDateTime::now_utc();
        let policy = resolve_policy(&versions, now);
        if facts.kind != intent.kind() {
            return Err(DomainError::InvalidRequest(
                "approval facts kind differs from intent".into(),
            ));
        }
        if let Some(direct) = intent.amount()? {
            if let Some(comparand) = &facts.amount {
                crate::domain::exact_money::matching_spec(&direct, comparand)?;
                if direct != *comparand {
                    return Err(DomainError::InvalidRequest(
                        "approval facts differ from captured transaction amount".into(),
                    ));
                }
            } else if amount_gated(intent.kind()) {
                return Err(DomainError::InvalidRequest(
                    "captured monetary intent requires amount facts".into(),
                ));
            }
        }
        let original_currency = facts.amount.as_ref().map(|v| v.currency().clone());
        let facts = self
            .to_functional_facts(scope, ctx.subject_tenant_id(), facts, &intent, now)
            .await?;
        if !requires_dual_control(&facts, &policy, to_naive_date(now))
            .map_err(policy_config_to_domain)?
        {
            return Ok(None);
        }
        let resolved = facts
            .amount
            .as_ref()
            .map(|v| policy.d2_threshold(v.currency()))
            .transpose()
            .map_err(policy_config_to_domain)?;
        let snapshot = threshold_snapshot(
            &policy,
            now,
            effective_version(&versions, now).as_ref(),
            if facts
                .amount
                .as_ref()
                .map(bss_ledger_sdk::PostedMoney::currency)
                == original_currency.as_ref()
            {
                SnapshotBasis::TransactionGate
            } else {
                SnapshotBasis::FunctionalGate
            },
            resolved.as_ref(),
        );
        let id = self
            .create_pending(
                ctx,
                scope,
                intent,
                reason_code,
                snapshot,
                facts.amount,
                policy.pending_ttl_seconds(),
            )
            .await?;
        Ok(Some(id))
    }

    /// Read the operation's locked evidence, preserving both historical specs.
    async fn operation_locked_rate(
        &self,
        scope: &AccessScope,
        tenant: Uuid,
        intent: &ApprovalIntent,
    ) -> Result<Option<RateSnapshotRow>, DomainError> {
        let (business_id, doc_type) = match intent {
            ApprovalIntent::Refund(i) => (i.payment_id.as_str(), SourceDocType::PaymentSettle),
            ApprovalIntent::RefundWithCreditNote(i) => {
                (i.refund.payment_id.as_str(), SourceDocType::PaymentSettle)
            }
            ApprovalIntent::CreditNote(i) => {
                (i.origin_invoice_id.as_str(), SourceDocType::InvoicePost)
            }
            ApprovalIntent::DebitNote(i) => {
                (i.origin_invoice_id.as_str(), SourceDocType::InvoicePost)
            }
            _ => return Ok(None),
        };
        self.journal
            .locked_rate_evidence_for(scope, tenant, business_id, doc_type.as_str())
            .await
            .map_err(|e| DomainError::Internal(format!("operation locked-rate lookup: {e}")))
    }

    /// Preserve derived transaction basis; other kinds use locked operation evidence
    /// when present and the existing current-quote fallback only when absent.
    async fn to_functional_facts(
        &self,
        scope: &AccessScope,
        tenant: Uuid,
        mut facts: OperationFacts,
        intent: &ApprovalIntent,
        now: OffsetDateTime,
    ) -> Result<OperationFacts, DomainError> {
        if valuation_basis(facts.kind) == ValuationBasis::Transaction {
            return Ok(facts);
        }
        let Some(amount) = facts.amount.as_ref() else {
            return Ok(facts);
        };
        let Some(functional) = self
            .reference
            .functional_currency(scope, tenant)
            .await
            .map_err(|e| DomainError::Internal(format!("dual-control functional currency: {e}")))?
        else {
            return Ok(facts);
        };
        if functional == amount.currency().code() {
            return Ok(facts);
        }
        let locked = self.operation_locked_rate(scope, tenant, intent).await?;
        let (rate, target) = if let Some(locked) = locked {
            if locked.quote.base_currency.code() != amount.currency().code()
                || locked.quote.quote_currency.code() != functional
            {
                return Err(DomainError::CurrencyMismatch(
                    "operation locked FX pair differs from comparand/functional currency".into(),
                ));
            }
            if locked.quote.base_currency.scale() != amount.currency().scale() {
                return Err(DomainError::InconsistentScale(
                    "operation locked FX base scale differs from comparand".into(),
                ));
            }
            (locked.quote.rate, locked.quote.quote_currency)
        } else {
            let resolver = CurrencyScaleResolver::new(self.reference.clone());
            let scale = resolver
                .resolve(scope, tenant, &functional)
                .await
                .map_err(scale_error_to_domain)?;
            let target =
                CurrencySpec::try_new(functional.clone(), scale).map_err(map_money_error)?;
            let rate = self
                .source
                .resolve(scope, tenant, amount.currency().code(), &functional, now)
                .await?
                .rate;
            (rate, target)
        };
        facts.amount = Some(translate_amount(amount, rate, target).map_err(|e| match e {
            crate::domain::fx::translate::FxTranslateError::Exact(e) => map_exact_error(e),
            other => DomainError::Internal(format!("approval FX translation: {other}")),
        })?);
        Ok(facts)
    }

    /// Read the tenant's effective dual-control policy *version* at `now` for the
    /// read surface (`GET /dual-control-policy`): the row in force (greatest
    /// `effective_from <= now`, highest `version` on a tie), or `None` when the
    /// tenant has set no row and the ratified [`DualControlPolicy::DEFAULT`]
    /// applies. Tenant-scoped — `scope` is the SQL-level BOLA filter, so a tenant
    /// outside the caller's authorized subtree reads as no rows ⇒ `None` ⇒ the
    /// platform defaults (the thresholds are public constants, so this leaks
    /// neither row existence nor a configured value).
    ///
    /// # Errors
    /// [`DomainError::Internal`] on a storage failure.
    pub async fn read_effective_policy(
        &self,
        scope: &AccessScope,
        tenant: Uuid,
        now: OffsetDateTime,
    ) -> Result<Option<PolicyVersion>, DomainError> {
        let versions = self.repo.read_policy_versions(scope, tenant).await?;
        Ok(effective_version(&versions, now))
    }

    /// Write a new effective-dated dual-control threshold policy version for the
    /// caller's tenant (DC8). Validates the D2/A6/TTL ranges first (out-of-range →
    /// [`DomainError::DualControlPolicyOutOfRange`], never clamped — DC9/DC11), then
    /// appends a `(tenant, version = max + 1)` row in one serializable txn, after
    /// checking every threshold's scale against the currency registry on that
    /// same txn. The resolver picks the latest `effective_from` (highest `version`
    /// on a tie), so a later write supersedes without mutating history. Returns
    /// the new `version`.
    ///
    /// # Errors
    /// [`DomainError::DualControlPolicyOutOfRange`] on an out-of-range threshold;
    /// [`DomainError::InconsistentScale`] when a threshold's scale differs from the
    /// registry (naming the currency and both scales); [`DomainError::InvalidRequest`]
    /// for a currency with no registry scale; [`DomainError::Internal`] on a storage
    /// failure.
    pub async fn set_policy(
        &self,
        ctx: &SecurityContext,
        scope: &AccessScope,
        d2_thresholds: Vec<PostedMoney>,
        a6_backdating_biz_days: i32,
        pending_ttl_seconds: i64,
        effective_from: OffsetDateTime,
    ) -> Result<i64, DomainError> {
        // Validated once, by type: no repeated currency, every value in range,
        // A6 and the TTL within their limits.
        let policy = D2Thresholds::try_new(d2_thresholds)
            .and_then(|d2| {
                DualControlPolicy::try_new(d2, a6_backdating_biz_days, pending_ttl_seconds)
            })
            .map_err(policy_config_to_domain)?;
        let tenant = ctx.subject_tenant_id();
        let created_at_utc = OffsetDateTime::now_utc();
        let scope_c = scope.clone();
        let op = RetryOp::new("dual-control policy write", Some(tenant));
        let version = retry_transaction_for(&self.db.db(), op, move |txn| {
            let scope = scope_c.clone();
            let policy = policy.clone();
            let resolver = CurrencyScaleResolver::new(self.reference.clone());
            Box::pin(async move {
                // Every threshold's scale must match the currency registry on this
                // transaction: a threshold at a stale scale would make every D2
                // lookup in its currency fail.
                for value in policy.d2_thresholds().iter() {
                    let currency = value.currency();
                    let registered = resolver
                        .resolve_in(txn, &scope, tenant, currency.code())
                        .await
                        .map_err(AttemptError::from)?;
                    if registered != currency.scale() {
                        return Err(AttemptError::Business(threshold_scale_mismatch(
                            currency.code(),
                            registered,
                            currency.scale(),
                        )));
                    }
                }
                let next = ApprovalRepo::max_policy_version(txn, &scope, tenant)
                    .await
                    .map_err(AttemptError::from)?
                    .map(|v| {
                        v.checked_add(1).ok_or_else(|| {
                            AttemptError::Business(DomainError::Internal(
                                "policy version overflow".into(),
                            ))
                        })
                    })
                    .transpose()?
                    .unwrap_or(0);
                ApprovalRepo::insert_policy_row(
                    txn,
                    &scope,
                    NewPolicyVersion {
                        tenant,
                        version: next,
                        effective_from,
                        policy,
                        created_at_utc,
                    },
                )
                .await
                .map_err(AttemptError::from)?;
                Ok::<i64, AttemptError>(next)
            })
        })
        .await?;
        Ok(version)
    }

    /// Approve an approval: latch `PENDING → APPROVING` (so a concurrent
    /// reject/cancel/request-changes can no longer win), execute the stored
    /// mutation (idempotent), then mark `APPROVING → APPROVED` + write the decision
    /// audit. A row already `APPROVING` (a crash-recovery retry) skips the latch and
    /// re-executes idempotently. A mutation failure reverts the latch to `PENDING`.
    ///
    /// # Errors
    /// [`DomainError::ApprovalNotFound`] / [`DomainError::ApprovalNotActionable`]
    /// (wrong state, expired, or lost the latch race) / [`DomainError::SelfApprovalForbidden`]
    /// (`approver == preparer`); the mutation's own rejection propagates unchanged.
    pub async fn approve(
        &self,
        ctx: &SecurityContext,
        scope: &AccessScope,
        approval_id: Uuid,
    ) -> Result<(), DomainError> {
        let tenant = ctx.subject_tenant_id();
        let approver = ctx.subject_id();
        let row = self.load_for_approve(scope, tenant, approval_id).await?;
        if approver == row.prepared_by {
            self.metrics.dual_control_self_approval_denied(&row.kind);
            return Err(DomainError::SelfApprovalForbidden(format!(
                "approver must differ from preparer for approval {approval_id}"
            )));
        }
        validate_threshold_snapshot(row.threshold_snapshot.clone())?;
        let intent = decode_intent(row.intent.clone())?;

        // (0) Latch PENDING → APPROVING in its own txn. Once latched, the decision
        //     verbs (reject/cancel/request-changes — all keyed on PENDING) match no
        //     row and fail, so the mutation about to execute can never be retro-
        //     actively rejected (H2). A row already APPROVING is a crash-recovery
        //     retry: skip the latch and re-execute idempotently.
        if parse_state(&row.state)? == ApprovalState::Pending
            && !self
                .bare_transition(
                    tenant,
                    scope,
                    approval_id,
                    ApprovalState::Pending,
                    row.revision,
                    ApprovalState::Approving,
                )
                .await?
        {
            return Err(DomainError::ApprovalNotActionable(format!(
                "approval {approval_id} was not in PENDING (concurrent decision)"
            )));
        }

        // (1) idempotent mutation in its own transaction.
        if let Err(e) = self.executor.execute(ctx, scope, &intent).await {
            // The mutation's txn rolled back (nothing committed), so the approval
            // is safe to return to PENDING — actionable again (re-approve / reject /
            // rework). Best-effort: a failed revert leaves the row APPROVING, which
            // a later approve recovers idempotently. The original error propagates.
            if let Err(revert) = self
                .bare_transition(
                    tenant,
                    scope,
                    approval_id,
                    ApprovalState::Approving,
                    row.revision,
                    ApprovalState::Pending,
                )
                .await
            {
                tracing::error!(
                    error = %revert,
                    %approval_id,
                    "bss-ledger: failed to revert APPROVING→PENDING after an executor error"
                );
            }
            return Err(e);
        }

        // (2) mark APPROVED + decision audit (APPROVING → APPROVED) in one txn.
        let body = decision_audit(
            "approved",
            &row.kind,
            &row.business_key,
            row.prepared_by,
            approver,
            None,
        );
        self.commit_transition(
            tenant,
            scope,
            approval_id,
            ApprovalState::Approving,
            ApprovalState::Approved,
            approver,
            row.revision,
            body,
        )
        .await?;
        self.metrics.dual_control_decided(&row.kind, "approved");
        Ok(())
    }

    /// Reject a `PENDING` approval with a mandatory reason. The mutation never
    /// runs.
    ///
    /// # Errors
    /// As [`Self::approve`] (minus the executor), with the reason recorded.
    pub async fn reject(
        &self,
        ctx: &SecurityContext,
        scope: &AccessScope,
        approval_id: Uuid,
        reason: String,
    ) -> Result<(), DomainError> {
        let tenant = ctx.subject_tenant_id();
        let decider = ctx.subject_id();
        let row = self.load_pending(scope, tenant, approval_id).await?;
        if decider == row.prepared_by {
            self.metrics.dual_control_self_approval_denied(&row.kind);
            return Err(DomainError::SelfApprovalForbidden(format!(
                "rejecter must differ from preparer for approval {approval_id}"
            )));
        }
        let body = decision_audit(
            "rejected",
            &row.kind,
            &row.business_key,
            row.prepared_by,
            decider,
            Some(&reason),
        );
        self.commit_transition(
            tenant,
            scope,
            approval_id,
            ApprovalState::Pending,
            ApprovalState::Rejected,
            decider,
            row.revision,
            body,
        )
        .await?;
        self.metrics.dual_control_decided(&row.kind, "rejected");
        Ok(())
    }

    /// Cancel an active (`PENDING`/`NEEDS_REWORK`) approval — only the preparer
    /// may withdraw their own request.
    ///
    /// # Errors
    /// [`DomainError::ApprovalNotFound`] / [`DomainError::ApprovalNotActionable`]
    /// (terminal, or caller is not the preparer / lost race).
    pub async fn cancel(
        &self,
        ctx: &SecurityContext,
        scope: &AccessScope,
        approval_id: Uuid,
    ) -> Result<(), DomainError> {
        let tenant = ctx.subject_tenant_id();
        let caller = ctx.subject_id();
        let row = self
            .repo
            .read(scope, tenant, approval_id)
            .await?
            .ok_or_else(|| DomainError::ApprovalNotFound(format!("approval {approval_id}")))?;
        let state = parse_state(&row.state)?;
        if !state.is_active() {
            return Err(DomainError::ApprovalNotActionable(format!(
                "approval {approval_id} is {} (terminal)",
                row.state
            )));
        }
        if caller != row.prepared_by {
            return Err(DomainError::ApprovalNotActionable(format!(
                "only the preparer may cancel approval {approval_id}"
            )));
        }
        let body = decision_audit(
            "cancelled",
            &row.kind,
            &row.business_key,
            row.prepared_by,
            caller,
            None,
        );
        self.commit_transition(
            tenant,
            scope,
            approval_id,
            state,
            ApprovalState::Cancelled,
            caller,
            row.revision,
            body,
        )
        .await?;
        self.metrics.dual_control_decided(&row.kind, "cancelled");
        Ok(())
    }

    /// Return a `PENDING` approval to the preparer for rework, with a mandatory
    /// reason. The mutation never runs; the preparer edits and `resubmit`s.
    ///
    /// # Errors
    /// As [`Self::reject`].
    pub async fn request_changes(
        &self,
        ctx: &SecurityContext,
        scope: &AccessScope,
        approval_id: Uuid,
        reason: String,
    ) -> Result<(), DomainError> {
        let tenant = ctx.subject_tenant_id();
        let decider = ctx.subject_id();
        let row = self.load_pending(scope, tenant, approval_id).await?;
        if decider == row.prepared_by {
            self.metrics.dual_control_self_approval_denied(&row.kind);
            return Err(DomainError::SelfApprovalForbidden(format!(
                "changer must differ from preparer for approval {approval_id}"
            )));
        }
        let body = decision_audit(
            "needs_rework",
            &row.kind,
            &row.business_key,
            row.prepared_by,
            decider,
            Some(&reason),
        );
        self.commit_transition(
            tenant,
            scope,
            approval_id,
            ApprovalState::Pending,
            ApprovalState::NeedsRework,
            decider,
            row.revision,
            body,
        )
        .await?;
        self.metrics.dual_control_decided(&row.kind, "needs_rework");
        Ok(())
    }

    /// Resubmit a `NEEDS_REWORK` approval back to `PENDING` with the preparer's
    /// edited intent, bumping `revision`. The kind cannot change. Re-evaluates the
    /// transaction threshold metadata and re-snapshots the policy in force (DC17);
    /// an approval, once required, is never dropped by shrinking the amount —
    /// resubmit always returns to `PENDING`, never auto-applies.
    ///
    /// # Errors
    /// [`DomainError::ApprovalNotFound`] / [`DomainError::ApprovalNotActionable`]
    /// (not awaiting rework, not the preparer, kind changed, or lost race).
    pub async fn resubmit(
        &self,
        ctx: &SecurityContext,
        scope: &AccessScope,
        approval_id: Uuid,
        new_intent: ApprovalIntent,
    ) -> Result<(), DomainError> {
        let tenant = ctx.subject_tenant_id();
        let caller = ctx.subject_id();
        let row = self
            .repo
            .read(scope, tenant, approval_id)
            .await?
            .ok_or_else(|| DomainError::ApprovalNotFound(format!("approval {approval_id}")))?;
        if parse_state(&row.state)? != ApprovalState::NeedsRework {
            return Err(DomainError::ApprovalNotActionable(format!(
                "approval {approval_id} is {}, expected NEEDS_REWORK",
                row.state
            )));
        }
        if caller != row.prepared_by {
            return Err(DomainError::ApprovalNotActionable(format!(
                "only the preparer may resubmit approval {approval_id}"
            )));
        }
        if new_intent.kind().as_str() != row.kind {
            return Err(DomainError::ApprovalNotActionable(
                "cannot change the approval kind on resubmit".to_owned(),
            ));
        }
        // DC #1: pin the target identity. Re-hydrate the stored intent
        // and require the resubmitted one to address the SAME target — only the
        // scalar amount may be edited (DC17). Without this a preparer could, after a
        // request-changes, swap the recipient (payer_tenant_id / entry_id /
        // payment_id) under the still-frozen business_key, and the approver would
        // book the credit / reversal / chargeback to the swapped party: the executor
        // replays the stored body and `ApprovalDto` never surfaces the recipient.
        validate_threshold_snapshot(row.threshold_snapshot.clone())?;
        let original_intent = decode_intent(row.intent.clone())?;
        if !new_intent.same_target(&original_intent) {
            return Err(DomainError::ApprovalNotActionable(
                "resubmit cannot change the approval target; only the amount may be edited"
                    .to_owned(),
            ));
        }
        // DC17: capture current policy and edited transaction money without a new
        // functional comparison. The captured basis states that this is a
        // resubmission snapshot; approval remains required even below threshold.
        let versions = self.repo.read_policy_versions(scope, tenant).await?;
        let resolved_at = OffsetDateTime::now_utc();
        let policy = resolve_policy(&versions, resolved_at);
        let new_amount = new_intent.amount()?;
        let resolved = new_amount
            .as_ref()
            .map(|v| policy.d2_threshold(v.currency()))
            .transpose()
            .map_err(policy_config_to_domain)?;
        let new_threshold_snapshot = threshold_snapshot(
            &policy,
            resolved_at,
            effective_version(&versions, resolved_at).as_ref(),
            SnapshotBasis::ResubmissionTransactionSnapshot,
            resolved.as_ref(),
        );
        let new_threshold_snapshot = serde_json::to_value(new_threshold_snapshot)
            .map_err(|e| DomainError::Internal(format!("encode approval policy snapshot: {e}")))?;
        let expected_revision = row.revision;
        let new_revision = expected_revision
            .checked_add(1)
            .ok_or_else(|| DomainError::Internal("approval revision overflow".into()))?;
        let intent_json = encode_intent(&new_intent)?;
        let body = decision_audit(
            "resubmitted",
            &row.kind,
            &row.business_key,
            row.prepared_by,
            caller,
            None,
        );
        let scope_c = scope.clone();
        let now = OffsetDateTime::now_utc();
        let comment_id = Uuid::now_v7();
        let op = RetryOp::new("dual-control resubmit", Some(tenant));
        let applied = retry_transaction_for(&self.db.db(), op, move |txn| {
            let scope = scope_c.clone();
            let intent_json = intent_json.clone();
            let snapshot = new_threshold_snapshot.clone();
            let new_amount = new_amount.clone();
            let body = body.clone();
            Box::pin(async move {
                let rows = ApprovalRepo::resubmit(
                    txn,
                    &scope,
                    tenant,
                    approval_id,
                    intent_json,
                    snapshot,
                    new_amount,
                    expected_revision,
                )
                .await
                .map_err(AttemptError::from)?;
                if rows == 0 {
                    return Ok::<bool, AttemptError>(false);
                }
                ApprovalRepo::append_comment(
                    txn,
                    &scope,
                    comment_id,
                    approval_id,
                    tenant,
                    new_revision,
                    caller,
                    body,
                    now,
                )
                .await
                .map_err(AttemptError::from)?;
                Ok(true)
            })
        })
        .await?;
        if !applied {
            return Err(DomainError::ApprovalNotActionable(format!(
                "approval {approval_id} was not in NEEDS_REWORK (concurrent decision)"
            )));
        }
        self.metrics.dual_control_decided(&row.kind, "resubmitted");
        Ok(())
    }

    /// Append a free comment / question to an approval's thread (no state change).
    /// The author is the caller; authz (preparer or `entry_approve.v1`) is gated
    /// at the REST layer. The comment is stamped with the revision read here; if
    /// the approval moved on (for example a resubmit) before the write, it is
    /// refused rather than stamped with the stale revision.
    ///
    /// # Errors
    /// [`DomainError::ApprovalNotFound`]; [`DomainError::ApprovalNotActionable`] when the
    /// revision changed before the comment was written; [`DomainError::Internal`].
    pub async fn add_comment(
        &self,
        ctx: &SecurityContext,
        scope: &AccessScope,
        approval_id: Uuid,
        body_text: String,
    ) -> Result<(), DomainError> {
        let tenant = ctx.subject_tenant_id();
        let row = self
            .repo
            .read(scope, tenant, approval_id)
            .await?
            .ok_or_else(|| DomainError::ApprovalNotFound(format!("approval {approval_id}")))?;
        self.append_comment_at(ctx, scope, approval_id, row.revision, body_text)
            .await
    }

    /// Write a comment stamped with `revision`, re-checking inside the write
    /// transaction that the approval is still at that revision.
    async fn append_comment_at(
        &self,
        ctx: &SecurityContext,
        scope: &AccessScope,
        approval_id: Uuid,
        revision: i32,
        body_text: String,
    ) -> Result<(), DomainError> {
        let tenant = ctx.subject_tenant_id();
        let author = ctx.subject_id();
        let scope_c = scope.clone();
        let now = OffsetDateTime::now_utc();
        let comment_id = Uuid::now_v7();
        let op = RetryOp::new("dual-control comment", Some(tenant));
        retry_transaction_for(&self.db.db(), op, move |txn| {
            let scope = scope_c.clone();
            let body = body_text.clone();
            Box::pin(async move {
                let current = ApprovalRepo::read_in_txn(txn, &scope, tenant, approval_id)
                    .await
                    .map_err(AttemptError::from)?;
                if current.as_ref().map(|r| r.revision) != Some(revision) {
                    return Err(AttemptError::Business(DomainError::ApprovalNotActionable(
                        "comment revision changed".into(),
                    )));
                }
                ApprovalRepo::append_comment(
                    txn,
                    &scope,
                    comment_id,
                    approval_id,
                    tenant,
                    revision,
                    author,
                    body,
                    now,
                )
                .await
                .map_err(AttemptError::from)?;
                Ok::<(), AttemptError>(())
            })
        })
        .await?;
        Ok(())
    }

    /// List the tenant's approval queue (newest-first), optionally filtered.
    ///
    /// # Errors
    /// [`DomainError::Internal`] on a storage failure.
    pub async fn list(
        &self,
        ctx: &SecurityContext,
        scope: &AccessScope,
        state: Option<&str>,
        kind: Option<&str>,
    ) -> Result<Vec<ApprovalRow>, DomainError> {
        self.repo
            .list(scope, ctx.subject_tenant_id(), state, kind)
            .await
    }

    /// Read a single approval.
    ///
    /// # Errors
    /// [`DomainError::Internal`] on a storage failure.
    pub async fn get(
        &self,
        ctx: &SecurityContext,
        scope: &AccessScope,
        approval_id: Uuid,
    ) -> Result<Option<ApprovalRow>, DomainError> {
        self.repo
            .read(scope, ctx.subject_tenant_id(), approval_id)
            .await
    }

    /// Read an approval's comment thread (oldest-first).
    ///
    /// # Errors
    /// [`DomainError::Internal`] on a storage failure.
    pub async fn thread(
        &self,
        ctx: &SecurityContext,
        scope: &AccessScope,
        approval_id: Uuid,
    ) -> Result<Vec<crate::infra::storage::entity::dual_control_comment::Model>, DomainError> {
        self.repo
            .read_thread(scope, ctx.subject_tenant_id(), approval_id)
            .await
    }

    /// Read + validate that an approval is approvable: `PENDING` (must be
    /// unexpired) or `APPROVING` (a crash-recovery retry — the decision was already
    /// latched, so expiry no longer gates it). Any other state is not actionable.
    async fn load_for_approve(
        &self,
        scope: &AccessScope,
        tenant: Uuid,
        approval_id: Uuid,
    ) -> Result<ApprovalRow, DomainError> {
        let row = self
            .repo
            .read(scope, tenant, approval_id)
            .await?
            .ok_or_else(|| DomainError::ApprovalNotFound(format!("approval {approval_id}")))?;
        match parse_state(&row.state)? {
            ApprovalState::Pending => {
                if row.expires_at <= OffsetDateTime::now_utc() {
                    return Err(DomainError::ApprovalNotActionable(format!(
                        "approval {approval_id} expired at {}",
                        row.expires_at
                    )));
                }
            }
            // Recovery: a prior approve latched APPROVING then crashed before the
            // mark; re-execute idempotently and complete it. Expiry no longer gates
            // a decision already taken.
            ApprovalState::Approving => {}
            _ => {
                return Err(DomainError::ApprovalNotActionable(format!(
                    "approval {approval_id} is {}, expected PENDING",
                    row.state
                )));
            }
        }
        Ok(row)
    }

    /// Apply a bare state transition (NO audit comment) in its own serializable
    /// txn; returns `true` iff the row was in `expected`. The H2 `APPROVING` latch
    /// and its revert are control-flow latches, not audited decisions, so they skip
    /// the audit append `commit_transition` writes; `approved_by`/`decided_at` stay
    /// `NULL` (stamped only by the final `APPROVING → APPROVED` `commit_transition`).
    async fn bare_transition(
        &self,
        tenant: Uuid,
        scope: &AccessScope,
        approval_id: Uuid,
        expected: ApprovalState,
        revision: i32,
        new_state: ApprovalState,
    ) -> Result<bool, DomainError> {
        let scope = scope.clone();
        let expected_s = expected.as_str();
        let new_s = new_state.as_str();
        let op = RetryOp::new("dual-control transition", Some(tenant));
        let applied = retry_transaction_for(&self.db.db(), op, move |txn| {
            let scope = scope.clone();
            Box::pin(async move {
                let rows = ApprovalRepo::transition(
                    txn,
                    &scope,
                    tenant,
                    approval_id,
                    expected_s,
                    revision,
                    new_s,
                    None,
                    None,
                )
                .await
                .map_err(AttemptError::from)?;
                Ok::<bool, AttemptError>(rows > 0)
            })
        })
        .await?;
        Ok(applied)
    }

    /// Read + validate that an approval is `PENDING` and not expired.
    async fn load_pending(
        &self,
        scope: &AccessScope,
        tenant: Uuid,
        approval_id: Uuid,
    ) -> Result<ApprovalRow, DomainError> {
        let row = self
            .repo
            .read(scope, tenant, approval_id)
            .await?
            .ok_or_else(|| DomainError::ApprovalNotFound(format!("approval {approval_id}")))?;
        let state = parse_state(&row.state)?;
        if state != ApprovalState::Pending {
            return Err(DomainError::ApprovalNotActionable(format!(
                "approval {approval_id} is {}, expected PENDING",
                row.state
            )));
        }
        if row.expires_at <= OffsetDateTime::now_utc() {
            return Err(DomainError::ApprovalNotActionable(format!(
                "approval {approval_id} expired at {}",
                row.expires_at
            )));
        }
        Ok(row)
    }

    /// Apply a state transition + append the decision audit in one serializable
    /// transaction. The optimistic `expected_state` filter is the in-txn race
    /// backstop: `0` rows means another decision won (or the row moved), which
    /// surfaces as [`DomainError::ApprovalNotActionable`].
    #[allow(clippy::too_many_arguments)] // a decision is intrinsically wide; a struct adds churn
    async fn commit_transition(
        &self,
        tenant: Uuid,
        scope: &AccessScope,
        approval_id: Uuid,
        expected: ApprovalState,
        new_state: ApprovalState,
        decider: Uuid,
        revision: i32,
        audit_body: String,
    ) -> Result<(), DomainError> {
        let scope = scope.clone();
        let expected_s = expected.as_str();
        let new_s = new_state.as_str();
        // `approved_by` is stamped ONLY for the actual APPROVED transition. A
        // reject / request-changes / CANCEL is not an approval — stamping the actor
        // there would, for a preparer self-cancel, set `approved_by == prepared_by`
        // and trip the `approved_by <> prepared_by` CHECK (surfacing as a 500). The
        // audit comment below records who acted regardless of the lifecycle state.
        let approved_by = (new_state == ApprovalState::Approved).then_some(decider);
        let now = OffsetDateTime::now_utc();
        let comment_id = Uuid::now_v7();
        let op = RetryOp::new("dual-control decision", Some(tenant));
        let applied = retry_transaction_for(&self.db.db(), op, move |txn| {
            let scope = scope.clone();
            let body = audit_body.clone();
            Box::pin(async move {
                let rows = ApprovalRepo::transition(
                    txn,
                    &scope,
                    tenant,
                    approval_id,
                    expected_s,
                    revision,
                    new_s,
                    approved_by,
                    Some(now),
                )
                .await
                .map_err(AttemptError::from)?;
                if rows == 0 {
                    return Ok::<bool, AttemptError>(false);
                }
                ApprovalRepo::append_comment(
                    txn,
                    &scope,
                    comment_id,
                    approval_id,
                    tenant,
                    revision,
                    decider,
                    body,
                    now,
                )
                .await
                .map_err(AttemptError::from)?;
                Ok(true)
            })
        })
        .await?;
        if applied {
            Ok(())
        } else {
            Err(DomainError::ApprovalNotActionable(format!(
                "approval {approval_id} was not in {expected_s} (concurrent decision or stale state)"
            )))
        }
    }
}

/// Classify a pending insert failure for the retry loop. A lost DC13 race is
/// deterministic (the winner's row is committed, so another attempt would hit
/// the same index): it stops the attempt without a retry, and the branch after
/// the loop reads the winner. Any other failure keeps its classification
/// (contention retries, everything else stops).
fn pending_insert_error(error: InsertPendingError) -> AttemptError {
    match error {
        InsertPendingError::ActiveExists => {
            AttemptError::Business(DomainError::ConcurrentModification(
                "an active approval already exists for the business key".into(),
            ))
        }
        InsertPendingError::Repo(error) => error.into(),
    }
}

/// Parse a stored state token, mapping an unknown literal to an internal fault.
fn parse_state(s: &str) -> Result<ApprovalState, DomainError> {
    ApprovalState::parse(s)
        .ok_or_else(|| DomainError::Internal(format!("unknown approval state {s:?}")))
}

/// The threshold-snapshot recorded on a pending approval: which policy values
/// applied + when resolved (audit trail; DC8/DC17). Built identically by `gate`
/// (on create) and `resubmit` (transaction policy provenance), so the
/// recorded snapshot is never a stub.
fn threshold_snapshot(
    policy: &DualControlPolicy,
    now: OffsetDateTime,
    version: Option<&PolicyVersion>,
    basis: SnapshotBasis,
    threshold: Option<&PostedMoney>,
) -> StoredThresholdSnapshot {
    use crate::infra::storage::money_text::StoredMoney;
    StoredThresholdSnapshot {
        d2_default: D2_DEFAULT_RULE.to_owned(),
        d2_thresholds: policy
            .d2_thresholds()
            .iter()
            .map(StoredMoney::from)
            .collect(),
        d2_threshold: threshold.map(StoredMoney::from),
        policy_version: version.map(|v| v.version),
        policy_effective_from: version.map(|v| format_rfc3339(v.effective_from)),
        basis,
        a6_backdating_biz_days: policy.a6_backdating_biz_days(),
        pending_ttl_seconds: policy.pending_ttl_seconds(),
        resolved_at: format_rfc3339(now),
    }
}

/// A D2 threshold submitted at a scale other than the currency registry's:
/// names the currency and both scales, so a caller setting several thresholds
/// can tell which one is stale.
fn threshold_scale_mismatch(currency: &str, registered: u8, submitted: u8) -> DomainError {
    DomainError::InconsistentScale(format!(
        "D2 threshold currency {currency} is registered at scale {registered} \
         but was submitted at scale {submitted}"
    ))
}

/// Map distinct policy defects without collapsing metadata conflicts.
fn policy_config_to_domain(e: PolicyConfigError) -> DomainError {
    match e {
        PolicyConfigError::MetadataConflict {
            currency,
            configured_scale,
            other_scale,
        } => DomainError::InconsistentScale(format!(
            "D2 threshold currency {currency} is configured at scale {configured_scale} \
             but used at scale {other_scale}"
        )),
        PolicyConfigError::DuplicateCurrency(currency) => {
            DomainError::InvalidRequest(format!("duplicate D2 currency {currency}"))
        }
        PolicyConfigError::D2OutOfRange {
            threshold,
            min,
            max,
        } => DomainError::DualControlPolicyOutOfRange(format!(
            "D2 threshold {threshold} (scale {}) outside [{}, {}]",
            threshold.currency().scale(),
            bss_ledger_sdk::canonical_decimal(min),
            bss_ledger_sdk::canonical_decimal(max),
        )),
        PolicyConfigError::D2DefaultUnrepresentable(currency) => {
            DomainError::DualControlPolicyOutOfRange(format!(
                "default D2 threshold is not representable for {} at scale {}",
                currency.code(),
                currency.scale()
            ))
        }
        PolicyConfigError::A6OutOfRange(v) => DomainError::DualControlPolicyOutOfRange(format!(
            "a6_backdating_biz_days {v} out of range [1..30]"
        )),
        PolicyConfigError::TtlNotPositive(v) => {
            DomainError::DualControlPolicyOutOfRange(format!("pending_ttl_seconds {v} must be > 0"))
        }
    }
}
/// The structured decision-audit body recorded on the append-only comment thread
/// (the Slice-2 stand-in for `secured_audit_record`, DC7). `event_type` mirrors
/// the design's `exception-resolution` audit class.
fn decision_audit(
    decision: &str,
    kind: &str,
    business_key: &str,
    prepared_by: Uuid,
    decided_by: Uuid,
    reason: Option<&str>,
) -> String {
    serde_json::json!({
        "event_type": "exception-resolution",
        "decision": decision,
        "kind": kind,
        "business_key": business_key,
        "prepared_by": prepared_by,
        "decided_by": decided_by,
        "reason": reason,
    })
    .to_string()
}

/// An active business key is idempotent only for the full canonical captured intent.
fn ensure_same_intent(row: &ApprovalRow, intent: &ApprovalIntent) -> Result<(), DomainError> {
    if canonical_identity(&decode_intent(row.intent.clone())?)? != canonical_identity(intent)? {
        return Err(DomainError::ApprovalNotActionable(
            "active approval has different captured intent".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
#[path = "service_tests.rs"]
mod tests;
