//! Orchestration of the four consumption operations.
//!
//! The shape is the same for all of them: validate what the caller supplied,
//! admit it at the PEP boundary, decide what the idempotency scope is, ask the
//! cache and then storage whether this operation already happened, and only
//! then mutate. Evaluation itself belongs to the storage transaction, which
//! selects the policy under the same row locks it mutates.
//!
//! A denial is a successful call: it carries a verdict, and the REST surface
//! renders it as HTTP 200. Only a failure to decide is an error.

use std::sync::Arc;
use std::time::Instant;

use quota_enforcement_sdk::{
    AppliedMutation, CreditRequest, DebitRequest, Decision, DecisionPreview, DecisionResult,
    EvaluatedMutation, IdempotencyScope, IdempotencySubjectKey, IdempotencyWrite, MetricId,
    OperationType, PartialIdempotencyWrite, PayloadHash, PreviewRequest,
    QuotaEnforcementStoragePluginV1, Retention, RollbackRequest, RollbackTarget, TransitionOutcome,
    positive_amount,
};
use serde::Serialize;
use serde_json::Value;
use time::OffsetDateTime;

use super::idempotency::{IdempotencyCache, ReplayRecord};
use crate::domain::admission::{Admission, AdmissionTarget};
use crate::domain::attribution::{AdmittedEvaluation, Attribution};
use crate::domain::catalog::MetricClassifications;
use crate::domain::engines::{EngineRegistry, PolicyArtifactCache, PreparedEvaluation};
use crate::domain::error::DomainError;
use crate::domain::pep::{actions, resources};
use crate::domain::ports::metrics::{DenialReason, OperationKind, QeMetrics};
use crate::domain::tokens;

/// The consumption operations over the bound dependencies.
///
/// A bundle of borrows rather than a constructor: the field list is the
/// dependency list, and it stays under the argument limit that a `new` would
/// breach.
pub struct Operations<'a> {
    /// The PEP boundary, for the operations that name a Quota rather than an
    /// attribution.
    pub admission: &'a Admission,
    /// Ingress for the operations that carry an attribution.
    pub attribution: Attribution<'a>,
    /// The published catalogue, for the one mapping the pipeline needs beyond
    /// ingress: which projection this metric treats as its user tier.
    pub catalog: &'a crate::domain::catalog::ProjectionContractCatalog,
    /// Metric classifications frozen at bootstrap. The evaluation path does
    /// not call the types registry.
    pub classifications: &'a MetricClassifications,
    /// The active storage plugin.
    pub storage: &'a dyn QuotaEnforcementStoragePluginV1,
    /// Statically linked engines.
    pub engines: Arc<EngineRegistry>,
    /// Resident compiled artifacts.
    pub artifacts: Arc<PolicyArtifactCache>,
    /// Recent replay records.
    pub idempotency: &'a IdempotencyCache,
    /// Telemetry sink.
    pub metrics: Arc<dyn QeMetrics>,
    /// Operator clamp the evaluation budget is resolved from.
    pub evaluation: quota_enforcement_sdk::engine::EvaluationLimits,
    /// How many preparations one operation may trigger.
    pub preparation_max_attempts: std::num::NonZeroU32,
    /// The TTL window lease acquisitions are checked against.
    pub leases: super::leases::LeaseLimits,
}

/// What a dry run evaluates: the read snapshot and the documents the policy
/// sees. Bundled so the call stays under the argument limit and reads as the
/// one subject it is.
#[derive(Clone, Copy)]
struct PreviewSubject<'a> {
    metric: &'a MetricId,
    amount: u64,
    snapshots: &'a [quota_enforcement_sdk::QuotaSnapshot],
    user_projection: Option<&'a gts::GtsTypeId>,
    request: &'a Value,
    resource: &'a Value,
}

/// What a debit's payload digest covers.
#[derive(Serialize)]
struct DebitPayload<'a> {
    attribution: &'a quota_enforcement_sdk::EvaluationAttribution,
    amount: i64,
}

/// What a credit's payload digest covers.
#[derive(Serialize)]
struct CreditPayload {
    quota_id: quota_enforcement_sdk::QuotaId,
    amount: i64,
}

/// What a rollback's payload digest covers.
#[derive(Serialize)]
struct RollbackPayload<'a> {
    attribution: &'a quota_enforcement_sdk::EvaluationAttribution,
    original_idempotency_key: &'a str,
}

// @cpt-dod:cpt-cf-quota-enforcement-dod-evaluation-orchestrator:p1
// @cpt-dod:cpt-cf-quota-enforcement-dod-consumption-endpoints:p1
// @cpt-dod:cpt-cf-quota-enforcement-dod-idempotency:p1
impl Operations<'_> {
    /// Charge `amount` against every applicable Quota.
    ///
    /// # Errors
    ///
    /// `InvalidArgument` for a non-positive amount, a missing key, or a metric
    /// that is not quota-gated; the admission error when the PDP refuses; the
    /// storage error of a failed evaluation or mutation.
    // @cpt-flow:cpt-cf-quota-enforcement-flow-debit:p1
    // @cpt-algo:cpt-cf-quota-enforcement-algo-evaluation-pipeline:p1
    // @cpt-algo:cpt-cf-quota-enforcement-algo-idempotency-replay:p1
    pub async fn debit(
        &self,
        ctx: &toolkit_security::SecurityContext,
        request: DebitRequest,
    ) -> Result<Decision, DomainError> {
        // @cpt-begin:cpt-cf-quota-enforcement-algo-evaluation-pipeline:p1:inst-pipe-spans
        let started = Instant::now();
        let outcome = self.debit_inner(ctx, request).await;
        self.metrics
            .record_evaluation(OperationKind::Debit, started.elapsed());
        // @cpt-end:cpt-cf-quota-enforcement-algo-evaluation-pipeline:p1:inst-pipe-spans
        outcome
    }

    async fn debit_inner(
        &self,
        ctx: &toolkit_security::SecurityContext,
        request: DebitRequest,
    ) -> Result<Decision, DomainError> {
        // @cpt-begin:cpt-cf-quota-enforcement-flow-debit:p1:inst-deb-amount-if
        // @cpt-begin:cpt-cf-quota-enforcement-flow-debit:p1:inst-deb-amount
        let amount = self.validate_amount(request.amount)?;
        // @cpt-end:cpt-cf-quota-enforcement-flow-debit:p1:inst-deb-amount
        // @cpt-end:cpt-cf-quota-enforcement-flow-debit:p1:inst-deb-amount-if
        self.validate_key(&request.idempotency_key)?;
        // Hash caller input before catalogue mapping for stable replay checks.
        // @cpt-begin:cpt-cf-quota-enforcement-flow-debit:p1:inst-deb-request
        // @cpt-begin:cpt-cf-quota-enforcement-flow-debit:p1:inst-deb-trust
        let payload_hash = digest(&DebitPayload {
            attribution: &request.attribution,
            amount: request.amount,
        })?;
        // @cpt-end:cpt-cf-quota-enforcement-flow-debit:p1:inst-deb-trust
        // @cpt-end:cpt-cf-quota-enforcement-flow-debit:p1:inst-deb-request
        // @cpt-begin:cpt-cf-quota-enforcement-algo-evaluation-pipeline:p1:inst-pipe-resolve
        let admitted = self
            .attribution
            .admit_evaluation(
                ctx,
                &resources::OPERATION,
                actions::DEBIT,
                request.attribution,
            )
            .await?;
        // @cpt-end:cpt-cf-quota-enforcement-algo-evaluation-pipeline:p1:inst-pipe-resolve
        // @cpt-begin:cpt-cf-quota-enforcement-algo-evaluation-pipeline:p1:inst-pipe-gated-if
        // @cpt-begin:cpt-cf-quota-enforcement-algo-evaluation-pipeline:p1:inst-pipe-gated
        self.classifications.ensure_quota_gated(&admitted.metric)?;
        // @cpt-end:cpt-cf-quota-enforcement-algo-evaluation-pipeline:p1:inst-pipe-gated
        // @cpt-end:cpt-cf-quota-enforcement-algo-evaluation-pipeline:p1:inst-pipe-gated-if

        // @cpt-begin:cpt-cf-quota-enforcement-algo-idempotency-replay:p1:inst-idem-scope
        // @cpt-begin:cpt-cf-quota-enforcement-algo-idempotency-replay:p1:inst-idem-independent
        let scope = Self::scope_of(&admitted, OperationType::Debit, request.idempotency_key);
        // @cpt-end:cpt-cf-quota-enforcement-algo-idempotency-replay:p1:inst-idem-independent
        // @cpt-end:cpt-cf-quota-enforcement-algo-idempotency-replay:p1:inst-idem-scope
        // @cpt-begin:cpt-cf-quota-enforcement-algo-evaluation-pipeline:p1:inst-pipe-idem
        // @cpt-begin:cpt-cf-quota-enforcement-algo-idempotency-replay:p1:inst-idem-replay-if
        if let Some(decision) = self
            .replay(&scope, payload_hash, OperationKind::Debit)
            .await?
        {
            // @cpt-begin:cpt-cf-quota-enforcement-algo-idempotency-replay:p1:inst-idem-replay
            return Ok(decision);
            // @cpt-end:cpt-cf-quota-enforcement-algo-idempotency-replay:p1:inst-idem-replay
        }
        // @cpt-end:cpt-cf-quota-enforcement-algo-idempotency-replay:p1:inst-idem-replay-if
        // @cpt-end:cpt-cf-quota-enforcement-algo-evaluation-pipeline:p1:inst-pipe-idem

        let applicable = applicable_of(&admitted);
        let user_projection = self.user_projection(&admitted.metric);
        let (request_value, resource_value) = policy_values(&admitted);
        let write = IdempotencyWrite {
            scope: scope.clone(),
            payload_hash,
        };
        let prepared = PreparedEvaluation::new(
            Arc::clone(&self.engines),
            Arc::clone(&self.artifacts),
            Arc::clone(&self.metrics),
            self.storage,
            self.preparation_max_attempts,
        );
        let evaluate = prepared.evaluator();
        let mutation = EvaluatedMutation {
            applicable: &applicable,
            amount,
            request: &request_value,
            resource: &resource_value,
            user_projection: user_projection.as_ref(),
            limits: self.evaluation,
            idempotency: &write,
            authorized: admitted.authorized,
            evaluate,
        };
        // @cpt-begin:cpt-cf-quota-enforcement-flow-debit:p1:inst-deb-pipeline
        // @cpt-begin:cpt-cf-quota-enforcement-algo-evaluation-pipeline:p1:inst-pipe-scope
        let outcome = prepared
            .run(|| {
                self.storage.apply_debit_plan(
                    ctx,
                    &admitted.attribution.access_scope,
                    &mutation,
                    // Storage supplies counter and threshold event details.
                    &[],
                )
            })
            .await?;
        // @cpt-end:cpt-cf-quota-enforcement-algo-evaluation-pipeline:p1:inst-pipe-scope
        // @cpt-end:cpt-cf-quota-enforcement-flow-debit:p1:inst-deb-pipeline
        let replayed = matches!(outcome, TransitionOutcome::NoOp(_));
        let evaluated = outcome.into_inner();
        if replayed {
            // A transaction-detected replay is not a second denial.
            self.metrics.record_idempotency_replay(OperationKind::Debit);
        } else {
            // @cpt-begin:cpt-cf-quota-enforcement-flow-debit:p1:inst-deb-denied-if
            // @cpt-begin:cpt-cf-quota-enforcement-flow-debit:p1:inst-deb-denied
            self.count_denial(&evaluated.decision);
            // @cpt-end:cpt-cf-quota-enforcement-flow-debit:p1:inst-deb-denied
            // @cpt-end:cpt-cf-quota-enforcement-flow-debit:p1:inst-deb-denied-if
        }
        // Unrecorded denials must be re-evaluated after provisioning changes.
        if let Retention::Recorded { expires_at } = evaluated.retention {
            self.idempotency.insert(
                scope,
                ReplayRecord {
                    payload_hash,
                    decision: evaluated.decision.clone(),
                    expires_at,
                },
                OffsetDateTime::now_utc(),
            );
        }
        // @cpt-begin:cpt-cf-quota-enforcement-flow-debit:p1:inst-deb-return
        // @cpt-begin:cpt-cf-quota-enforcement-algo-evaluation-pipeline:p1:inst-pipe-return
        Ok(evaluated.decision)
        // @cpt-end:cpt-cf-quota-enforcement-algo-evaluation-pipeline:p1:inst-pipe-return
        // @cpt-end:cpt-cf-quota-enforcement-flow-debit:p1:inst-deb-return
    }

    /// Return `amount` to one named Quota.
    ///
    /// # Errors
    ///
    /// `InvalidArgument` for a non-positive amount or a missing key; the
    /// admission error; the storage error of the mutation.
    // @cpt-flow:cpt-cf-quota-enforcement-flow-credit:p1
    pub async fn credit(
        &self,
        ctx: &toolkit_security::SecurityContext,
        request: CreditRequest,
    ) -> Result<Decision, DomainError> {
        let started = Instant::now();
        let outcome = self.credit_inner(ctx, request).await;
        self.metrics
            .record_evaluation(OperationKind::Credit, started.elapsed());
        outcome
    }

    async fn credit_inner(
        &self,
        ctx: &toolkit_security::SecurityContext,
        request: CreditRequest,
    ) -> Result<Decision, DomainError> {
        // @cpt-begin:cpt-cf-quota-enforcement-flow-credit:p1:inst-cre-amount-if
        // @cpt-begin:cpt-cf-quota-enforcement-flow-credit:p1:inst-cre-amount
        let amount = self.validate_amount(request.amount)?;
        // @cpt-end:cpt-cf-quota-enforcement-flow-credit:p1:inst-cre-amount
        // @cpt-end:cpt-cf-quota-enforcement-flow-credit:p1:inst-cre-amount-if
        self.validate_key(&request.idempotency_key)?;
        // @cpt-begin:cpt-cf-quota-enforcement-flow-credit:p1:inst-cre-request
        let payload_hash = digest(&CreditPayload {
            quota_id: request.quota_id,
            amount: request.amount,
        })?;
        // @cpt-end:cpt-cf-quota-enforcement-flow-credit:p1:inst-cre-request
        let admitted = self
            .admission
            .admit(
                ctx,
                &resources::OPERATION,
                actions::CREDIT,
                AdmissionTarget::tenant(request.tenant_id),
            )
            .await?;
        // Storage completes the idempotency scope from the locked Quota row.
        let partial = PartialIdempotencyWrite {
            tenant_id: request.tenant_id,
            key: request.idempotency_key,
            payload_hash,
        };
        let outcome = self
            .storage
            .apply_credit(
                ctx,
                &admitted.access_scope,
                request.quota_id,
                amount,
                &partial,
                &[],
            )
            .await?;
        // @cpt-begin:cpt-cf-quota-enforcement-flow-credit:p1:inst-cre-return
        Ok(self.applied(outcome, OperationKind::Credit))
        // @cpt-end:cpt-cf-quota-enforcement-flow-credit:p1:inst-cre-return
    }

    /// Reverse a committed debit.
    ///
    /// # Errors
    ///
    /// `InvalidArgument` for a missing key; the admission error; `NotFound`
    /// when no committed debit answers the request under the recomputed scope
    /// and authorized attribution.
    // @cpt-flow:cpt-cf-quota-enforcement-flow-rollback:p1
    pub async fn rollback(
        &self,
        ctx: &toolkit_security::SecurityContext,
        request: RollbackRequest,
    ) -> Result<Decision, DomainError> {
        let started = Instant::now();
        let outcome = self.rollback_inner(ctx, request).await;
        self.metrics
            .record_evaluation(OperationKind::Rollback, started.elapsed());
        outcome
    }

    async fn rollback_inner(
        &self,
        ctx: &toolkit_security::SecurityContext,
        request: RollbackRequest,
    ) -> Result<Decision, DomainError> {
        self.validate_key(&request.idempotency_key)?;
        self.validate_key(&request.original_idempotency_key)?;
        // @cpt-begin:cpt-cf-quota-enforcement-flow-rollback:p1:inst-rlb-request
        let payload_hash = digest(&RollbackPayload {
            attribution: &request.attribution,
            original_idempotency_key: &request.original_idempotency_key,
        })?;
        // @cpt-end:cpt-cf-quota-enforcement-flow-rollback:p1:inst-rlb-request
        // Re-authorize the original debit attribution before reversing it.
        let admitted = self
            .attribution
            .admit_evaluation(
                ctx,
                &resources::OPERATION,
                actions::ROLLBACK,
                request.attribution,
            )
            .await?;
        let scope = Self::scope_of(&admitted, OperationType::Rollback, request.idempotency_key);
        // @cpt-begin:cpt-cf-quota-enforcement-flow-rollback:p1:inst-rlb-idem
        if let Some(decision) = self
            .replay(&scope, payload_hash, OperationKind::Rollback)
            .await?
        {
            return Ok(decision);
        }
        // @cpt-end:cpt-cf-quota-enforcement-flow-rollback:p1:inst-rlb-idem
        let target = RollbackTarget {
            original: IdempotencyScope {
                tenant_id: admitted.attribution.tenant_id,
                subject_key: IdempotencySubjectKey::of(&admitted.attribution.subjects),
                // A debit and a lease commit are separate namespaces under the
                // same key; the caller says which one it reverses.
                operation_type: request.original_operation.operation_type(),
                key: request.original_idempotency_key,
            },
            authorized: admitted.authorized,
        };
        let outcome = self
            .storage
            .apply_rollback(
                ctx,
                &admitted.attribution.access_scope,
                &target,
                &IdempotencyWrite {
                    scope: scope.clone(),
                    payload_hash,
                },
                &[],
            )
            .await?;
        let decision = self.applied_cached(outcome, OperationKind::Rollback, scope, payload_hash);
        // @cpt-begin:cpt-cf-quota-enforcement-flow-rollback:p1:inst-rlb-return
        Ok(decision)
        // @cpt-end:cpt-cf-quota-enforcement-flow-rollback:p1:inst-rlb-return
    }

    /// Evaluate without mutating anything and without occupying a key.
    ///
    /// # Errors
    ///
    /// The errors of [`Self::debit`], minus the idempotency ones.
    // @cpt-flow:cpt-cf-quota-enforcement-flow-evaluate-preview:p1
    pub async fn preview(
        &self,
        ctx: &toolkit_security::SecurityContext,
        request: PreviewRequest,
    ) -> Result<DecisionPreview, DomainError> {
        let started = Instant::now();
        let outcome = self.preview_inner(ctx, request).await;
        self.metrics
            .record_evaluation(OperationKind::Preview, started.elapsed());
        outcome
    }

    async fn preview_inner(
        &self,
        ctx: &toolkit_security::SecurityContext,
        request: PreviewRequest,
    ) -> Result<DecisionPreview, DomainError> {
        // @cpt-begin:cpt-cf-quota-enforcement-flow-evaluate-preview:p1:inst-prv-request
        let amount = self.validate_amount(request.amount)?;
        let admitted = self
            .attribution
            .admit_evaluation(
                ctx,
                &resources::OPERATION,
                actions::PREVIEW,
                request.attribution,
            )
            .await?;
        // @cpt-end:cpt-cf-quota-enforcement-flow-evaluate-preview:p1:inst-prv-request
        self.classifications.ensure_quota_gated(&admitted.metric)?;
        let applicable = applicable_of(&admitted);
        let user_projection = self.user_projection(&admitted.metric);
        // @cpt-begin:cpt-cf-quota-enforcement-flow-evaluate-preview:p1:inst-prv-read
        // @cpt-begin:cpt-cf-quota-enforcement-flow-evaluate-preview:p1:inst-prv-nopersist
        let snapshots = self
            .storage
            .read_quota_snapshot(ctx, &admitted.attribution.access_scope, &applicable)
            .await?;
        // @cpt-end:cpt-cf-quota-enforcement-flow-evaluate-preview:p1:inst-prv-nopersist
        // @cpt-end:cpt-cf-quota-enforcement-flow-evaluate-preview:p1:inst-prv-read
        let (request_value, resource_value) = policy_values(&admitted);
        let prepared = PreparedEvaluation::new(
            Arc::clone(&self.engines),
            Arc::clone(&self.artifacts),
            Arc::clone(&self.metrics),
            self.storage,
            self.preparation_max_attempts,
        );
        let decision = self
            .preview_decision(
                &prepared,
                &PreviewSubject {
                    metric: &admitted.metric,
                    amount,
                    snapshots: &snapshots,
                    user_projection: user_projection.as_ref(),
                    request: &request_value,
                    resource: &resource_value,
                },
            )
            .await?;
        // Preview denials are not operational denials.
        // @cpt-begin:cpt-cf-quota-enforcement-flow-evaluate-preview:p1:inst-prv-return
        Ok(DecisionPreview::of(decision))
        // @cpt-end:cpt-cf-quota-enforcement-flow-evaluate-preview:p1:inst-prv-return
    }

    /// Evaluate the selected policy against a read snapshot, preparing a
    /// missing artifact within the same bounded budget a mutation uses.
    async fn preview_decision(
        &self,
        prepared: &PreparedEvaluation<'_>,
        subject: &PreviewSubject<'_>,
    ) -> Result<Decision, DomainError> {
        let PreviewSubject {
            metric,
            amount,
            snapshots,
            user_projection,
            request,
            resource,
        } = *subject;
        // Match transaction policy selection: metric first, then global.
        let policy = match self
            .storage
            .read_policy(&quota_enforcement_sdk::PolicyScope::Metric {
                metric: metric.clone(),
            })
            .await?
        {
            Some(policy) => policy,
            None => self
                .storage
                .read_policy(&quota_enforcement_sdk::PolicyScope::Global)
                .await?
                .ok_or_else(|| {
                    DomainError::Internal("no active global resolution policy is seeded".to_owned())
                })?,
        };
        let arbitration: Vec<Value> = snapshots
            .iter()
            .map(|snapshot| Value::Object(snapshot.metadata.clone()))
            .collect();
        let quotas: Vec<quota_enforcement_sdk::engine::EvaluationQuota<'_>> = snapshots
            .iter()
            .zip(&arbitration)
            .map(
                |(snapshot, arbitration)| quota_enforcement_sdk::engine::EvaluationQuota {
                    snapshot,
                    // Match transaction tier resolution.
                    tier: match user_projection {
                        Some(user) if snapshot.subject.projection_type == *user => {
                            quota_enforcement_sdk::engine::QuotaScopeTier::User
                        }
                        _ => quota_enforcement_sdk::engine::QuotaScopeTier::Tenant,
                    },
                    arbitration,
                },
            )
            .collect();
        let budget = self
            .evaluation
            .budget(policy.timeout_ms)
            .map_err(|error| DomainError::Internal(error.to_string()))?;
        let evaluate = prepared.evaluator();
        let context = quota_enforcement_sdk::EvaluationContext {
            policy: &policy,
            metric,
            amount,
            time: OffsetDateTime::now_utc(),
            quotas: &quotas,
            request,
            resource,
            budget,
        };
        // Use the mutation path's bounded artifact-preparation retry.
        // @cpt-begin:cpt-cf-quota-enforcement-flow-evaluate-preview:p1:inst-prv-engine
        let outcome = prepared
            .run(|| std::future::ready(evaluate(&context).map_err(storage_failure)))
            .await?;
        // @cpt-end:cpt-cf-quota-enforcement-flow-evaluate-preview:p1:inst-prv-engine
        Ok(outcome.into_decision())
    }

    // --- shared steps -------------------------------------------------------

    /// A positive amount, or the closed token that says why not.
    pub(super) fn validate_amount(&self, amount: i64) -> Result<u64, DomainError> {
        positive_amount(amount).ok_or_else(|| {
            self.metrics.record_denial(DenialReason::InvalidArgument);
            DomainError::InvalidArgument {
                field: "amount",
                reason: tokens::INVALID_AMOUNT,
            }
        })
    }

    /// Every write carries a key. An empty one would make the scope
    /// meaningless rather than absent.
    pub(super) fn validate_key(&self, key: &str) -> Result<(), DomainError> {
        if key.trim().is_empty() {
            self.metrics.record_denial(DenialReason::InvalidArgument);
            return Err(DomainError::InvalidArgument {
                field: "idempotency_key",
                reason: tokens::IDEMPOTENCY_KEY_REQUIRED,
            });
        }
        Ok(())
    }

    /// The projection this metric treats as its user tier, when the catalogue
    /// admits one.
    ///
    /// The engine ranks a user-scoped Quota above a tenant-scoped one, so a
    /// deployment whose metric has no user projection simply has one tier. A
    /// catalogue miss is not an error here: ingress already proved the metric
    /// is admitted, and a metric admitted only at tenant scope is ordinary.
    pub(super) fn user_projection(&self, metric: &MetricId) -> Option<gts::GtsTypeId> {
        self.catalog
            .map_subject(metric, &quota_enforcement_sdk::SubjectScope::user())
            .ok()
            .cloned()
    }

    pub(super) fn scope_of(
        admitted: &AdmittedEvaluation,
        operation_type: OperationType,
        key: String,
    ) -> IdempotencyScope {
        IdempotencyScope {
            tenant_id: admitted.attribution.tenant_id,
            subject_key: IdempotencySubjectKey::of(&admitted.attribution.subjects),
            operation_type,
            key,
        }
    }

    /// The cache, then storage. A divergent payload is refused here rather
    /// than inside the transaction, so nothing is locked to reject it.
    async fn replay(
        &self,
        scope: &IdempotencyScope,
        payload_hash: PayloadHash,
        operation: OperationKind,
    ) -> Result<Option<Decision>, DomainError> {
        let now = OffsetDateTime::now_utc();
        // @cpt-begin:cpt-cf-quota-enforcement-algo-idempotency-replay:p1:inst-idem-lookup
        if let Some(cached) = self.idempotency.get(scope, now) {
            // @cpt-begin:cpt-cf-quota-enforcement-algo-idempotency-replay:p1:inst-idem-mismatch-if
            if cached.payload_hash != payload_hash {
                // @cpt-begin:cpt-cf-quota-enforcement-algo-idempotency-replay:p1:inst-idem-mismatch
                return Err(DomainError::IdempotencyPayloadMismatch);
                // @cpt-end:cpt-cf-quota-enforcement-algo-idempotency-replay:p1:inst-idem-mismatch
            }
            // @cpt-end:cpt-cf-quota-enforcement-algo-idempotency-replay:p1:inst-idem-mismatch-if
            self.metrics.record_idempotency_replay(operation);
            return Ok(Some(cached.decision));
        }
        let Some(record) = self.storage.lookup_idempotency(scope).await? else {
            return Ok(None);
        };
        // @cpt-end:cpt-cf-quota-enforcement-algo-idempotency-replay:p1:inst-idem-lookup
        if record.payload_hash != payload_hash {
            return Err(DomainError::IdempotencyPayloadMismatch);
        }
        let replay = ReplayRecord::try_from(record)
            .map_err(|error| DomainError::Internal(error.to_string()))?;
        self.metrics.record_idempotency_replay(operation);
        self.idempotency.insert(scope.clone(), replay.clone(), now);
        Ok(Some(replay.decision))
    }

    pub(super) fn applied(
        &self,
        outcome: TransitionOutcome<AppliedMutation>,
        operation: OperationKind,
    ) -> Decision {
        if matches!(outcome, TransitionOutcome::NoOp(_)) {
            self.metrics.record_idempotency_replay(operation);
        }
        outcome.into_inner().decision
    }

    fn applied_cached(
        &self,
        outcome: TransitionOutcome<AppliedMutation>,
        operation: OperationKind,
        scope: IdempotencyScope,
        payload_hash: PayloadHash,
    ) -> Decision {
        if matches!(outcome, TransitionOutcome::NoOp(_)) {
            self.metrics.record_idempotency_replay(operation);
        }
        let applied = outcome.into_inner();
        self.idempotency.insert(
            scope,
            ReplayRecord {
                payload_hash,
                decision: applied.decision.clone(),
                expires_at: applied.expires_at,
            },
            OffsetDateTime::now_utc(),
        );
        applied.decision
    }

    pub(super) fn count_denial(&self, decision: &Decision) {
        if let DecisionResult::Denied { reason, .. } = &decision.result {
            self.metrics
                .record_denial(DenialReason::from_decision_reason(reason));
        }
    }
}

pub(super) fn applicable_of(
    admitted: &AdmittedEvaluation,
) -> quota_enforcement_sdk::ApplicableQuotas {
    quota_enforcement_sdk::ApplicableQuotas {
        tenant_id: admitted.attribution.tenant_id,
        subjects: admitted.attribution.subjects.clone(),
        metric: admitted.metric.clone(),
    }
}

/// The two Policy-visible documents, as the engine sees them.
pub(super) fn policy_values(admitted: &AdmittedEvaluation) -> (Value, Value) {
    let request = Value::Object(admitted.input.request.clone());
    let resource = admitted
        .input
        .resource
        .as_ref()
        .map_or(Value::Null, |projection| {
            serde_json::to_value(projection).unwrap_or(Value::Null)
        });
    (request, resource)
}

/// An evaluation failure as the storage contract reports it, so the shared
/// preparation driver can recognize a missing artifact.
fn storage_failure(
    failure: quota_enforcement_sdk::engine::EvaluationFailure,
) -> quota_enforcement_sdk::StorageError {
    match failure {
        quota_enforcement_sdk::engine::EvaluationFailure::PreparationRequired {
            policy_id,
            version,
        } => quota_enforcement_sdk::StorageError::PreparationRequired { policy_id, version },
        failure => quota_enforcement_sdk::StorageError::EvaluationFailed {
            engine_id: String::new(),
            failure,
        },
    }
}

pub(super) fn digest<T: Serialize>(payload: &T) -> Result<PayloadHash, DomainError> {
    PayloadHash::of_canonical(payload).map_err(|error| DomainError::Internal(error.to_string()))
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "service_tests.rs"]
mod tests;
