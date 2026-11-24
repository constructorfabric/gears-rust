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

use crate::domain::admission::admission_span;
use std::sync::Arc;
use std::time::Instant;
use tracing::Instrument as _;

use quota_enforcement_sdk::{
    AppliedMutation, ChargeReturn, CreditRequest, DebitRequest, Decision, DecisionPreview,
    DecisionResult, EvaluatedMutation, IdempotencyScope, IdempotencyWrite, MetricId, OperationType,
    PartialIdempotencyWrite, PayloadHash, PreviewRequest, QuotaEnforcementStoragePluginV1,
    Retention, RollbackRequest, RollbackTarget, TransitionOutcome, positive_amount,
};
use serde::Serialize;
use serde_json::Value;
use time::OffsetDateTime;
use toolkit_macros::domain_model;

use super::idempotency::{IdempotencyCache, ReplayRecord};
use crate::domain::admission::{AdmissionTarget, Gate};
use crate::domain::attribution::{AdmittedEvaluation, Attribution, MappedAttribution};
use crate::domain::catalog::{MetricCatalog, MetricMode, ServedMetric};
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
#[domain_model]
pub struct Operations<'a> {
    /// Admission in the mode these operations are built over.
    pub admission: Gate<'a>,
    /// The metric catalogue.
    pub catalog: &'a MetricCatalog,
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
    /// The batch debit's bounds.
    pub batch: super::batch::BatchLimits,
    /// The snapshot read's bounds.
    pub snapshot: super::snapshot::SnapshotLimits,
}

/// What a dry run evaluates: the read snapshot and the documents the policy
/// sees. Bundled so the call stays under the argument limit and reads as the
/// one subject it is.
#[derive(Clone, Copy)]
struct PreviewSubject<'a> {
    metric: &'a MetricId,
    metric_id: Option<&'a str>,
    amount: u64,
    snapshots: &'a [quota_enforcement_sdk::QuotaSnapshot],
    request: &'a Value,
}

/// Give back part of a charge (ADR-0011). Only the quota resolver plugin
/// sends it; it names the charge only, so it carries no attribution.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReturnRequest {
    /// The charge's tenant.
    pub tenant_id: quota_enforcement_sdk::TenantId,
    /// Which kind of operation wrote the charge.
    pub operation: quota_enforcement_sdk::RollbackableOperation,
    /// Idempotency key of the charged operation.
    pub charge_key: String,
    /// The metric type the amount is counted in; it must be the charge's.
    pub metric: MetricId,
    /// Amount to give back. Signed as [`quota_enforcement_sdk::DebitRequest::amount`] is.
    pub amount: i64,
    /// Key of this return, unique per tenant.
    pub idempotency_key: String,
}

/// What a debit's payload digest covers: its request fingerprint, which a
/// batch item's charge carries too. The subjects are the admitted set, the
/// tenant first and the rest ordered by type, so a retry that lists them in
/// another order, or names the tenant that the first attempt left implicit,
/// is the same request. The `metadata` is the admitted one, in its canonical
/// form, so a retry that writes an integer as `5.0` is the same request too.
/// The amount is as sent.
#[derive(Serialize)]
pub(super) struct DebitPayload<'a> {
    tenant_id: quota_enforcement_sdk::TenantId,
    subjects: &'a [quota_enforcement_sdk::SubjectRef],
    metric_type: &'a str,
    metric_id: Option<&'a str>,
    metadata: &'a serde_json::Map<String, serde_json::Value>,
    amount: i64,
}

impl<'a> DebitPayload<'a> {
    pub(super) fn of(admitted: &'a AdmittedEvaluation, amount: i64) -> Self {
        Self {
            tenant_id: admitted.attribution.tenant_id,
            subjects: &admitted.attribution.subjects,
            metric_type: admitted.metric.id().as_str(),
            metric_id: admitted.metric_id.as_deref(),
            metadata: &admitted.request,
            amount,
        }
    }
}

/// What a credit's payload digest covers.
#[derive(Serialize)]
struct CreditPayload {
    quota_id: quota_enforcement_sdk::QuotaId,
    amount: i64,
}

// @cpt-dod:cpt-cf-quota-enforcement-dod-evaluation-orchestrator:p1
// @cpt-dod:cpt-cf-quota-enforcement-dod-consumption-endpoints:p1
// @cpt-dod:cpt-cf-quota-enforcement-dod-idempotency:p1
impl Operations<'_> {
    /// Ingress for the operations that carry an attribution.
    pub(super) fn attribution(&self) -> Attribution<'_> {
        Attribution::new(&self.admission, self.catalog, self.metrics.as_ref())
    }

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
    #[tracing::instrument(name = "quota_enforcement.operations.debit", skip_all, fields(tenant_id = %request.attribution.tenant_id, metric = %request.attribution.metric.metric_type))]
    pub async fn debit(&self, request: DebitRequest) -> Result<Decision, DomainError> {
        // @cpt-begin:cpt-cf-quota-enforcement-algo-evaluation-pipeline:p1:inst-pipe-spans
        let started = Instant::now();
        let outcome = self.debit_inner(request).await;
        self.metrics
            .record_evaluation(OperationKind::Debit, started.elapsed());
        // @cpt-end:cpt-cf-quota-enforcement-algo-evaluation-pipeline:p1:inst-pipe-spans
        outcome
    }

    async fn debit_inner(&self, request: DebitRequest) -> Result<Decision, DomainError> {
        // @cpt-begin:cpt-cf-quota-enforcement-flow-debit:p1:inst-deb-amount-if
        // @cpt-begin:cpt-cf-quota-enforcement-flow-debit:p1:inst-deb-amount
        let amount = self.validate_amount(request.amount)?;
        // @cpt-end:cpt-cf-quota-enforcement-flow-debit:p1:inst-deb-amount
        // @cpt-end:cpt-cf-quota-enforcement-flow-debit:p1:inst-deb-amount-if
        self.validate_key("idempotency_key", &request.idempotency_key)?;
        // @cpt-begin:cpt-cf-quota-enforcement-algo-evaluation-pipeline:p1:inst-pipe-resolve
        let admitted = self
            .attribution()
            .admit_evaluation(&resources::OPERATION, actions::DEBIT, request.attribution)
            .instrument(admission_span(1))
            .await?;
        // @cpt-end:cpt-cf-quota-enforcement-algo-evaluation-pipeline:p1:inst-pipe-resolve
        // @cpt-begin:cpt-cf-quota-enforcement-flow-debit:p1:inst-deb-request
        // @cpt-begin:cpt-cf-quota-enforcement-flow-debit:p1:inst-deb-trust
        let payload_hash = digest(&DebitPayload::of(&admitted, request.amount))?;
        // @cpt-end:cpt-cf-quota-enforcement-flow-debit:p1:inst-deb-trust
        // @cpt-end:cpt-cf-quota-enforcement-flow-debit:p1:inst-deb-request
        // @cpt-begin:cpt-cf-quota-enforcement-algo-evaluation-pipeline:p1:inst-pipe-gated-if
        // @cpt-begin:cpt-cf-quota-enforcement-algo-evaluation-pipeline:p1:inst-pipe-gated
        ensure_quota_gated(&admitted.metric)?;
        // @cpt-end:cpt-cf-quota-enforcement-algo-evaluation-pipeline:p1:inst-pipe-gated
        // @cpt-end:cpt-cf-quota-enforcement-algo-evaluation-pipeline:p1:inst-pipe-gated-if

        // @cpt-begin:cpt-cf-quota-enforcement-algo-idempotency-replay:p1:inst-idem-scope
        // @cpt-begin:cpt-cf-quota-enforcement-algo-idempotency-replay:p1:inst-idem-independent
        let scope = Self::scope_of(
            &admitted.attribution,
            OperationType::Debit,
            request.idempotency_key,
        );
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
        let request_value = Value::Object(admitted.request.clone());
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
            metric_id: admitted.metric_id.as_deref(),
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
                    &admitted.attribution.actor,
                    &admitted.attribution.access_scope,
                    &mutation,
                    // Storage supplies counter and threshold event details.
                    &[],
                )
            })
            .await;
        if matches!(outcome, Err(DomainError::LeaseContentionTimeout)) {
            self.metrics
                .record_lease_contention_rejected(admitted.metric.label());
        }
        let outcome = self.contended(OperationKind::Debit, outcome)?;
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
    #[tracing::instrument(name = "quota_enforcement.operations.credit", skip_all, fields(tenant_id = %request.tenant_id, quota_id = %request.quota_id))]
    pub async fn credit(&self, request: CreditRequest) -> Result<Decision, DomainError> {
        let started = Instant::now();
        let outcome = self.credit_inner(request).await;
        self.metrics
            .record_evaluation(OperationKind::Credit, started.elapsed());
        outcome
    }

    async fn credit_inner(&self, request: CreditRequest) -> Result<Decision, DomainError> {
        // @cpt-begin:cpt-cf-quota-enforcement-flow-credit:p1:inst-cre-amount-if
        // @cpt-begin:cpt-cf-quota-enforcement-flow-credit:p1:inst-cre-amount
        let amount = self.validate_amount(request.amount)?;
        // @cpt-end:cpt-cf-quota-enforcement-flow-credit:p1:inst-cre-amount
        // @cpt-end:cpt-cf-quota-enforcement-flow-credit:p1:inst-cre-amount-if
        self.validate_key("idempotency_key", &request.idempotency_key)?;
        // @cpt-begin:cpt-cf-quota-enforcement-flow-credit:p1:inst-cre-request
        let payload_hash = digest(&CreditPayload {
            quota_id: request.quota_id,
            amount: request.amount,
        })?;
        // @cpt-end:cpt-cf-quota-enforcement-flow-credit:p1:inst-cre-request
        let admitted = self
            .admission
            .admit(
                &resources::OPERATION,
                actions::CREDIT,
                AdmissionTarget::tenant(request.tenant_id),
            )
            .instrument(admission_span(1))
            .await?;
        let partial = PartialIdempotencyWrite {
            tenant_id: request.tenant_id,
            key: request.idempotency_key,
            payload_hash,
        };
        let outcome = self
            .storage
            .apply_credit(
                &admitted.actor,
                &admitted.access_scope,
                request.quota_id,
                amount,
                &partial,
                &[],
            )
            .await
            .map_err(DomainError::from);
        let outcome = self.contended(OperationKind::Credit, outcome)?;
        // @cpt-begin:cpt-cf-quota-enforcement-flow-credit:p1:inst-cre-return
        Ok(self.applied(outcome, OperationKind::Credit))
        // @cpt-end:cpt-cf-quota-enforcement-flow-credit:p1:inst-cre-return
    }

    /// Give back everything that remains of a charge. It names an existing
    /// charge, so it makes no catalogue check and runs while the metric is
    /// unavailable. It has no key of its own: on a charge with nothing left
    /// it succeeds and changes nothing.
    ///
    /// # Errors
    ///
    /// `InvalidArgument` for a missing key; the admission error; `NotFound`
    /// when no live charge answers the request under its scope and authorized
    /// attribution digest; `FailedPrecondition` (`RETURN_NOT_APPLICABLE`)
    /// when no Quota of the charge can take what remains.
    // @cpt-flow:cpt-cf-quota-enforcement-flow-rollback:p1
    #[tracing::instrument(name = "quota_enforcement.operations.rollback", skip_all, fields(tenant_id = %request.attribution.tenant_id, metric = %request.attribution.metric.metric_type))]
    pub async fn rollback(&self, request: RollbackRequest) -> Result<Decision, DomainError> {
        let started = Instant::now();
        let outcome = self.rollback_inner(request).await;
        self.metrics
            .record_evaluation(OperationKind::Rollback, started.elapsed());
        outcome
    }

    async fn rollback_inner(&self, request: RollbackRequest) -> Result<Decision, DomainError> {
        // @cpt-begin:cpt-cf-quota-enforcement-flow-rollback:p1:inst-rlb-request
        self.validate_key(
            "original_idempotency_key",
            &request.original_idempotency_key,
        )?;
        // @cpt-end:cpt-cf-quota-enforcement-flow-rollback:p1:inst-rlb-request
        // @cpt-begin:cpt-cf-quota-enforcement-flow-rollback:p1:inst-rlb-authz
        let admitted = self
            .attribution()
            .admit_reversal(
                &resources::OPERATION,
                actions::ROLLBACK,
                request.attribution,
            )
            .instrument(admission_span(1))
            .await?;
        // @cpt-end:cpt-cf-quota-enforcement-flow-rollback:p1:inst-rlb-authz
        let target = RollbackTarget {
            original: IdempotencyScope {
                tenant_id: admitted.attribution.tenant_id,
                operation_type: request.original_operation.operation_type(),
                key: request.original_idempotency_key,
            },
            authorized: admitted.authorized,
        };
        let outcome = self
            .storage
            .apply_rollback(
                &admitted.attribution.actor,
                &admitted.attribution.access_scope,
                &target,
                &[],
            )
            .await
            .map_err(DomainError::from);
        let outcome = self.contended(OperationKind::Rollback, outcome)?;
        // @cpt-begin:cpt-cf-quota-enforcement-flow-rollback:p1:inst-rlb-return
        Ok(outcome.into_inner().decision)
        // @cpt-end:cpt-cf-quota-enforcement-flow-rollback:p1:inst-rlb-return
    }

    /// Give back part of a charge. Reached only through the resolver plugin,
    /// over trusted admission; it names only the charge and compares no
    /// attribution digest.
    ///
    /// # Errors
    ///
    /// `InvalidArgument` for a non-positive amount or a missing key, and
    /// `METRIC_TYPE_MISMATCH` for a metric type other than the charge's;
    /// `NotFound` (`UNKNOWN_OPERATION`) for an unknown or deleted charge;
    /// `FailedPrecondition` (`RETURN_EXCEEDS_CHARGE`,
    /// `RETURN_NOT_APPLICABLE`); `AlreadyExists` on a payload mismatch, which a
    /// retry with the same key does not pass.
    // @cpt-flow:cpt-cf-quota-enforcement-flow-return:p1
    #[tracing::instrument(name = "quota_enforcement.operations.return", skip_all, fields(tenant_id = %request.tenant_id, metric = %request.metric.as_str()))]
    pub async fn return_amount(&self, request: ReturnRequest) -> Result<(), DomainError> {
        let started = Instant::now();
        let outcome = self.return_inner(request).await;
        self.metrics
            .record_evaluation(OperationKind::Return, started.elapsed());
        outcome
    }

    async fn return_inner(&self, request: ReturnRequest) -> Result<(), DomainError> {
        // @cpt-begin:cpt-cf-quota-enforcement-flow-return:p1:inst-rtn-amount
        let amount = self.validate_amount(request.amount)?;
        // @cpt-end:cpt-cf-quota-enforcement-flow-return:p1:inst-rtn-amount
        self.validate_key("idempotency_key", &request.idempotency_key)?;
        self.validate_key("charge_key", &request.charge_key)?;
        // @cpt-begin:cpt-cf-quota-enforcement-flow-return:p1:inst-rtn-request
        let admitted = self
            .admission
            .admit(
                &resources::OPERATION,
                actions::RETURN,
                AdmissionTarget::tenant(request.tenant_id),
            )
            .instrument(admission_span(1))
            .await?;
        // @cpt-end:cpt-cf-quota-enforcement-flow-return:p1:inst-rtn-request
        let ret = ChargeReturn {
            charge: IdempotencyScope {
                tenant_id: request.tenant_id,
                operation_type: request.operation.operation_type(),
                key: request.charge_key,
            },
            metric: request.metric,
            amount,
            key: request.idempotency_key,
        };
        let outcome = self
            .storage
            .apply_return(&admitted.actor, &admitted.access_scope, &ret, &[])
            .await
            .map_err(DomainError::from);
        let outcome = self.contended(OperationKind::Return, outcome)?;
        if matches!(outcome, TransitionOutcome::NoOp(_)) {
            self.metrics
                .record_idempotency_replay(OperationKind::Return);
        }
        // @cpt-begin:cpt-cf-quota-enforcement-flow-return:p1:inst-rtn-return
        Ok(())
        // @cpt-end:cpt-cf-quota-enforcement-flow-return:p1:inst-rtn-return
    }

    /// Evaluate without mutating anything and without occupying a key.
    ///
    /// # Errors
    ///
    /// The errors of [`Self::debit`], minus the idempotency ones.
    // @cpt-flow:cpt-cf-quota-enforcement-flow-evaluate-preview:p1
    #[tracing::instrument(name = "quota_enforcement.operations.preview", skip_all, fields(tenant_id = %request.attribution.tenant_id, metric = %request.attribution.metric.metric_type))]
    pub async fn preview(&self, request: PreviewRequest) -> Result<DecisionPreview, DomainError> {
        let started = Instant::now();
        let outcome = self.preview_inner(request).await;
        self.metrics
            .record_evaluation(OperationKind::Preview, started.elapsed());
        outcome
    }

    async fn preview_inner(&self, request: PreviewRequest) -> Result<DecisionPreview, DomainError> {
        // @cpt-begin:cpt-cf-quota-enforcement-flow-evaluate-preview:p1:inst-prv-request
        let amount = self.validate_amount(request.amount)?;
        let admitted = self
            .attribution()
            .admit_evaluation(&resources::OPERATION, actions::PREVIEW, request.attribution)
            .instrument(admission_span(1))
            .await?;
        // @cpt-end:cpt-cf-quota-enforcement-flow-evaluate-preview:p1:inst-prv-request
        ensure_quota_gated(&admitted.metric)?;
        let applicable = applicable_of(&admitted);
        // @cpt-begin:cpt-cf-quota-enforcement-flow-evaluate-preview:p1:inst-prv-read
        // @cpt-begin:cpt-cf-quota-enforcement-flow-evaluate-preview:p1:inst-prv-nopersist
        let request_value = Value::Object(admitted.request.clone());
        // Only the Quotas whose conditions hold apply, as in a debit (I15).
        let snapshots: Vec<_> = self
            .storage
            .read_quota_snapshot(&admitted.attribution.access_scope, &applicable)
            .await?
            .into_iter()
            .filter(|snapshot| {
                quota_enforcement_sdk::conditions_hold(&snapshot.metadata, &request_value)
            })
            .collect();
        // @cpt-end:cpt-cf-quota-enforcement-flow-evaluate-preview:p1:inst-prv-nopersist
        // @cpt-end:cpt-cf-quota-enforcement-flow-evaluate-preview:p1:inst-prv-read
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
                    metric: admitted.metric.id(),
                    metric_id: admitted.metric_id.as_deref(),
                    amount,
                    snapshots: &snapshots,
                    request: &request_value,
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
            metric_id,
            amount,
            snapshots,
            request,
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
        let quotas: Vec<quota_enforcement_sdk::engine::EvaluationQuota<'_>> = snapshots
            .iter()
            .map(|snapshot| quota_enforcement_sdk::engine::EvaluationQuota {
                snapshot,
                tier: quota_enforcement_sdk::engine::QuotaScopeTier::of(&snapshot.subject),
            })
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
            metric_id,
            budget,
        };
        // Use the mutation path's bounded artifact-preparation retry.
        // @cpt-begin:cpt-cf-quota-enforcement-flow-evaluate-preview:p1:inst-prv-engine
        let outcome = prepared
            .run(|| {
                std::future::ready(
                    evaluate(&context)
                        .map_err(|failure| storage_failure(&policy.engine_id, failure)),
                )
            })
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
    /// meaningless rather than absent, and an over-long one would fail in the
    /// storage index as a backend error rather than as the caller's.
    pub(super) fn validate_key(&self, field: &'static str, key: &str) -> Result<(), DomainError> {
        if let Some(reason) = key_fault(key) {
            self.metrics.record_denial(DenialReason::InvalidArgument);
            return Err(DomainError::InvalidArgument { field, reason });
        }
        Ok(())
    }

    pub(super) fn scope_of(
        attribution: &MappedAttribution,
        operation_type: OperationType,
        key: String,
    ) -> IdempotencyScope {
        IdempotencyScope {
            tenant_id: attribution.tenant_id,
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

    /// Count an operation the storage refused on the contention timeout.
    pub(super) fn contended<T>(
        &self,
        operation: OperationKind,
        outcome: Result<T, DomainError>,
    ) -> Result<T, DomainError> {
        if matches!(outcome, Err(DomainError::LeaseContentionTimeout)) {
            self.metrics.record_contention_rejected(operation);
        }
        outcome
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
        metric: admitted.metric.id().clone(),
    }
}

/// Debit and lease acquisition take capacity, so a metric whose enforcement
/// is `direct` refuses them.
pub(super) fn ensure_quota_gated(metric: &ServedMetric) -> Result<(), DomainError> {
    if metric.mode() == MetricMode::Direct {
        return Err(DomainError::MetricNotQuotaGated {
            metric: metric.id().to_string(),
        });
    }
    Ok(())
}

/// An evaluation failure as the storage contract reports it, so the shared
/// preparation driver can recognize a missing artifact.
fn storage_failure(
    engine_id: &str,
    failure: quota_enforcement_sdk::engine::EvaluationFailure,
) -> quota_enforcement_sdk::StorageError {
    match failure {
        quota_enforcement_sdk::engine::EvaluationFailure::PreparationRequired {
            policy_id,
            version,
        } => quota_enforcement_sdk::StorageError::PreparationRequired { policy_id, version },
        failure => quota_enforcement_sdk::StorageError::EvaluationFailed {
            engine_id: engine_id.to_owned(),
            failure,
        },
    }
}

/// Why an idempotency key is refused, if it is.
pub(crate) fn key_fault(key: &str) -> Option<&'static str> {
    if key.trim().is_empty() {
        Some(tokens::IDEMPOTENCY_KEY_REQUIRED)
    } else if key.chars().count() > crate::domain::quotas::validation::IDEMPOTENCY_KEY_MAX_LEN {
        Some(tokens::IDEMPOTENCY_KEY_TOO_LONG)
    } else {
        None
    }
}

pub(super) fn digest<T: Serialize>(payload: &T) -> Result<PayloadHash, DomainError> {
    PayloadHash::of_canonical(payload).map_err(|error| DomainError::Internal(error.to_string()))
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "service_tests.rs"]
mod tests;
