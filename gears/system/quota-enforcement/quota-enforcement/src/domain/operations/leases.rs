//! The three lease operations (`LeaseManager`): acquire, commit, and release.
//!
//! Acquisition is a debit that holds instead of charges: the same ingress, the
//! same evaluation inside the storage transaction, the same idempotency scope
//! derived from the authorized subject set, under the `reserve` operation kind.
//! Commit and release name a lease by its token and an explicit tenant; they
//! are admitted on the lease resource before storage is read, and storage
//! completes their idempotency scope from the lease's own acquisition subject
//! key, never from caller input.
//!
//! None of the three uses the in-process replay cache: an acquisition's replay
//! carries a token the cache's record cannot hold, and a settlement's scope is
//! only known inside the transaction. Replays are answered by storage.

use std::sync::Arc;
use std::time::{Duration, Instant};

use quota_enforcement_sdk::{
    AcquireLeaseOutcome, AcquireLeaseRequest, CommitLeaseRequest, Decision, EvaluatedLease,
    EvaluatedMutation, IdempotencyWrite, LeaseToken, OperationType, PartialIdempotencyWrite,
    ReleaseLeaseRequest, TenantId, TransitionOutcome,
};
use serde::Serialize;
use toolkit_macros::domain_model;

use super::service::{Operations, applicable_of, digest, policy_values};
use crate::domain::admission::AdmissionTarget;
use crate::domain::engines::PreparedEvaluation;
use crate::domain::error::DomainError;
use crate::domain::pep::{actions, resources};
use crate::domain::ports::metrics::{DenialReason, OperationKind};
use crate::domain::tokens;

/// The TTL window an acquisition must fall in (`[min_lease_ttl,
/// max_lease_ttl]`). A TTL outside it is refused, never clamped: the holder is
/// entitled to exactly the TTL it reserved.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LeaseLimits {
    /// Shortest TTL accepted.
    pub min_ttl: Duration,
    /// Longest TTL accepted.
    pub max_ttl: Duration,
}

impl Default for LeaseLimits {
    /// The platform defaults: one second to one hour.
    fn default() -> Self {
        Self {
            min_ttl: Duration::from_secs(1),
            max_ttl: Duration::from_hours(1),
        }
    }
}

/// What an acquisition's payload digest covers.
#[derive(Serialize)]
struct AcquirePayload<'a> {
    attribution: &'a quota_enforcement_sdk::EvaluationAttribution,
    amount: i64,
    ttl_secs: Option<u64>,
}

/// What a commit's payload digest covers.
#[derive(Serialize)]
struct CommitPayload {
    tenant_id: TenantId,
    token: LeaseToken,
    actual_amount: Option<i64>,
}

/// What a release's payload digest covers.
#[derive(Serialize)]
struct ReleasePayload {
    tenant_id: TenantId,
    token: LeaseToken,
}

// @cpt-flow:cpt-cf-quota-enforcement-flow-lease-acquire:p1
// @cpt-flow:cpt-cf-quota-enforcement-flow-lease-commit:p1
// @cpt-flow:cpt-cf-quota-enforcement-flow-lease-release:p1
// @cpt-dod:cpt-cf-quota-enforcement-dod-lease-endpoints:p1
impl Operations<'_> {
    /// Hold `amount` against every applicable Quota for `ttl_secs`.
    ///
    /// # Errors
    ///
    /// `InvalidArgument` for a non-positive amount, a TTL missing or outside
    /// the configured window, a missing key, or a metric that is not
    /// quota-gated; the admission error; `LeaseInflightLimitExceeded` at the
    /// active-lease cap; `LeaseContentionTimeout` when the contention budget
    /// runs out; the storage error of a failed evaluation or hold.
    pub async fn acquire_lease(
        &self,
        ctx: &toolkit_security::SecurityContext,
        request: AcquireLeaseRequest,
    ) -> Result<AcquireLeaseOutcome, DomainError> {
        let started = Instant::now();
        let outcome = self.acquire_lease_inner(ctx, request).await;
        self.metrics
            .record_evaluation(OperationKind::Reserve, started.elapsed());
        outcome
    }

    async fn acquire_lease_inner(
        &self,
        ctx: &toolkit_security::SecurityContext,
        request: AcquireLeaseRequest,
    ) -> Result<AcquireLeaseOutcome, DomainError> {
        // @cpt-begin:cpt-cf-quota-enforcement-flow-lease-acquire:p1:inst-lac-amount-if
        // @cpt-begin:cpt-cf-quota-enforcement-flow-lease-acquire:p1:inst-lac-amount
        let amount = self.validate_amount(request.amount)?;
        // @cpt-end:cpt-cf-quota-enforcement-flow-lease-acquire:p1:inst-lac-amount
        // @cpt-end:cpt-cf-quota-enforcement-flow-lease-acquire:p1:inst-lac-amount-if
        // @cpt-begin:cpt-cf-quota-enforcement-flow-lease-acquire:p1:inst-lac-ttl-if
        // @cpt-begin:cpt-cf-quota-enforcement-flow-lease-acquire:p1:inst-lac-ttl
        let ttl = self.validate_ttl(request.ttl_secs)?;
        // @cpt-end:cpt-cf-quota-enforcement-flow-lease-acquire:p1:inst-lac-ttl
        // @cpt-end:cpt-cf-quota-enforcement-flow-lease-acquire:p1:inst-lac-ttl-if
        self.validate_key(&request.idempotency_key)?;
        // @cpt-begin:cpt-cf-quota-enforcement-flow-lease-acquire:p1:inst-lac-request
        let payload_hash = digest(&AcquirePayload {
            attribution: &request.attribution,
            amount: request.amount,
            ttl_secs: request.ttl_secs,
        })?;
        let admitted = self
            .attribution
            .admit_evaluation(
                ctx,
                &resources::OPERATION,
                actions::RESERVE,
                request.attribution,
            )
            .await?;
        // @cpt-end:cpt-cf-quota-enforcement-flow-lease-acquire:p1:inst-lac-request
        // @cpt-begin:cpt-cf-quota-enforcement-flow-lease-acquire:p1:inst-lac-pipeline
        self.classifications.ensure_quota_gated(&admitted.metric)?;
        let scope = Self::scope_of(&admitted, OperationType::Reserve, request.idempotency_key);
        if let Some(outcome) = self.lease_replay(&scope, payload_hash).await? {
            return Ok(outcome);
        }
        // @cpt-end:cpt-cf-quota-enforcement-flow-lease-acquire:p1:inst-lac-pipeline

        let applicable = applicable_of(&admitted);
        let user_projection = self.user_projection(&admitted.metric);
        let (request_value, resource_value) = policy_values(&admitted);
        let write = IdempotencyWrite {
            scope,
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
        // The wait is the storage call as a whole: lock waits and the
        // evaluation inside the transaction, on every path out of it.
        // @cpt-begin:cpt-cf-quota-enforcement-flow-lease-acquire:p1:inst-lac-wait
        let waited = Instant::now();
        let outcome = prepared
            .run(|| {
                self.storage
                    .acquire_lease(ctx, &admitted.attribution.access_scope, &mutation, ttl)
            })
            .await;
        let label = self.classifications.label(&admitted.metric);
        if let Some(label) = &label {
            self.metrics
                .record_lease_acquisition_wait(label, waited.elapsed());
            match &outcome {
                // @cpt-begin:cpt-cf-quota-enforcement-flow-lease-acquire:p1:inst-lac-contention-if
                // @cpt-begin:cpt-cf-quota-enforcement-flow-lease-acquire:p1:inst-lac-contention
                Err(DomainError::LeaseContentionTimeout) => {
                    self.metrics.record_lease_contention_rejected(label);
                }
                // @cpt-end:cpt-cf-quota-enforcement-flow-lease-acquire:p1:inst-lac-contention
                // @cpt-end:cpt-cf-quota-enforcement-flow-lease-acquire:p1:inst-lac-contention-if
                Err(DomainError::LeaseInflightLimitExceeded) => {
                    self.metrics.record_lease_inflight_limit_exceeded(label);
                }
                _ => {}
            }
        }
        // @cpt-end:cpt-cf-quota-enforcement-flow-lease-acquire:p1:inst-lac-wait
        let outcome = outcome?;
        let replayed = matches!(outcome, TransitionOutcome::NoOp(_));
        let evaluated = outcome.into_inner();
        if replayed {
            self.metrics
                .record_idempotency_replay(OperationKind::Reserve);
        } else {
            // @cpt-begin:cpt-cf-quota-enforcement-flow-lease-acquire:p1:inst-lac-denied-if
            // @cpt-begin:cpt-cf-quota-enforcement-flow-lease-acquire:p1:inst-lac-denied
            self.count_denial(&evaluated.decision);
            // @cpt-end:cpt-cf-quota-enforcement-flow-lease-acquire:p1:inst-lac-denied
            // @cpt-end:cpt-cf-quota-enforcement-flow-lease-acquire:p1:inst-lac-denied-if
        }
        // @cpt-begin:cpt-cf-quota-enforcement-flow-lease-acquire:p1:inst-lac-return
        outcome_of(evaluated)
        // @cpt-end:cpt-cf-quota-enforcement-flow-lease-acquire:p1:inst-lac-return
    }

    /// Settle an active lease, keeping `actual_amount` of what it holds and
    /// returning the rest; no amount keeps all of it.
    ///
    /// # Errors
    ///
    /// `InvalidArgument` for a negative amount or a missing key; the admission
    /// error; `NotFound` for a token the authorized tenant does not hold;
    /// `LeaseNotActive` for an expired or resolved lease;
    /// `OverCommitNotAuthorized` above the reserved amount;
    /// `LeaseContentionTimeout` when the contention budget runs out.
    pub async fn commit_lease(
        &self,
        ctx: &toolkit_security::SecurityContext,
        request: CommitLeaseRequest,
    ) -> Result<Decision, DomainError> {
        let started = Instant::now();
        let outcome = self.commit_lease_inner(ctx, request).await;
        self.metrics
            .record_evaluation(OperationKind::Commit, started.elapsed());
        outcome
    }

    async fn commit_lease_inner(
        &self,
        ctx: &toolkit_security::SecurityContext,
        request: CommitLeaseRequest,
    ) -> Result<Decision, DomainError> {
        let actual = self.validate_actual(request.actual_amount)?;
        self.validate_key(&request.idempotency_key)?;
        // @cpt-begin:cpt-cf-quota-enforcement-flow-lease-commit:p1:inst-lcm-request
        let payload_hash = digest(&CommitPayload {
            tenant_id: request.tenant_id,
            token: request.token,
            actual_amount: request.actual_amount,
        })?;
        let admitted = self
            .admission
            .admit(
                ctx,
                &resources::LEASE,
                actions::COMMIT,
                AdmissionTarget::resource(request.tenant_id, request.token.as_uuid()),
            )
            .await?;
        // @cpt-end:cpt-cf-quota-enforcement-flow-lease-commit:p1:inst-lcm-request
        // Storage completes the scope from the lease's acquisition subject key.
        let partial = PartialIdempotencyWrite {
            tenant_id: request.tenant_id,
            key: request.idempotency_key,
            payload_hash,
        };
        let outcome = self
            .storage
            .commit_lease(
                ctx,
                &admitted.access_scope,
                request.token,
                actual,
                &partial,
                &[],
            )
            .await?;
        // @cpt-begin:cpt-cf-quota-enforcement-flow-lease-commit:p1:inst-lcm-return
        Ok(self.applied(outcome, OperationKind::Commit))
        // @cpt-end:cpt-cf-quota-enforcement-flow-lease-commit:p1:inst-lcm-return
    }

    /// Return everything an active lease holds.
    ///
    /// # Errors
    ///
    /// `InvalidArgument` for a missing key; the admission error; `NotFound`
    /// for a token the authorized tenant does not hold; `LeaseNotActive` for an
    /// expired or resolved lease; `LeaseContentionTimeout` when the contention
    /// budget runs out.
    pub async fn release_lease(
        &self,
        ctx: &toolkit_security::SecurityContext,
        request: ReleaseLeaseRequest,
    ) -> Result<Decision, DomainError> {
        let started = Instant::now();
        let outcome = self.release_lease_inner(ctx, request).await;
        self.metrics
            .record_evaluation(OperationKind::Release, started.elapsed());
        outcome
    }

    async fn release_lease_inner(
        &self,
        ctx: &toolkit_security::SecurityContext,
        request: ReleaseLeaseRequest,
    ) -> Result<Decision, DomainError> {
        self.validate_key(&request.idempotency_key)?;
        // @cpt-begin:cpt-cf-quota-enforcement-flow-lease-release:p1:inst-lrl-request
        let payload_hash = digest(&ReleasePayload {
            tenant_id: request.tenant_id,
            token: request.token,
        })?;
        let admitted = self
            .admission
            .admit(
                ctx,
                &resources::LEASE,
                actions::RELEASE,
                AdmissionTarget::resource(request.tenant_id, request.token.as_uuid()),
            )
            .await?;
        // @cpt-end:cpt-cf-quota-enforcement-flow-lease-release:p1:inst-lrl-request
        let partial = PartialIdempotencyWrite {
            tenant_id: request.tenant_id,
            key: request.idempotency_key,
            payload_hash,
        };
        let outcome = self
            .storage
            .release_lease(ctx, &admitted.access_scope, request.token, &partial, &[])
            .await?;
        // @cpt-begin:cpt-cf-quota-enforcement-flow-lease-release:p1:inst-lrl-return
        Ok(self.applied(outcome, OperationKind::Release))
        // @cpt-end:cpt-cf-quota-enforcement-flow-lease-release:p1:inst-lrl-return
    }

    // --- lease-specific steps ----------------------------------------------

    /// A TTL inside the configured window, or the closed token that says why
    /// not. Missing is out of bounds too: there is no default to clamp to.
    fn validate_ttl(&self, ttl_secs: Option<u64>) -> Result<Duration, DomainError> {
        ttl_secs
            .map(Duration::from_secs)
            .filter(|ttl| (self.leases.min_ttl..=self.leases.max_ttl).contains(ttl))
            .ok_or_else(|| {
                self.metrics.record_denial(DenialReason::InvalidArgument);
                DomainError::InvalidArgument {
                    field: "ttl",
                    reason: tokens::TTL_OUT_OF_BOUNDS,
                }
            })
    }

    /// A commit's kept amount: absent keeps everything, zero keeps nothing and
    /// returns every hold, a negative amount is refused.
    fn validate_actual(&self, actual: Option<i64>) -> Result<Option<u64>, DomainError> {
        match actual {
            None => Ok(None),
            Some(amount) => u64::try_from(amount).map(Some).map_err(|_| {
                self.metrics.record_denial(DenialReason::InvalidArgument);
                DomainError::InvalidArgument {
                    field: "actual_amount",
                    reason: tokens::INVALID_AMOUNT,
                }
            }),
        }
    }

    /// A stored acquisition under `scope`: its original outcome, or the
    /// payload mismatch when the same key carried a different request.
    async fn lease_replay(
        &self,
        scope: &quota_enforcement_sdk::IdempotencyScope,
        payload_hash: quota_enforcement_sdk::PayloadHash,
    ) -> Result<Option<AcquireLeaseOutcome>, DomainError> {
        let Some(record) = self.storage.lookup_idempotency(scope).await? else {
            return Ok(None);
        };
        if record.payload_hash != payload_hash {
            return Err(DomainError::IdempotencyPayloadMismatch);
        }
        let evaluated: EvaluatedLease = serde_json::from_value(record.decision_blob)
            .map_err(|error| DomainError::Internal(error.to_string()))?;
        self.metrics
            .record_idempotency_replay(OperationKind::Reserve);
        outcome_of(evaluated).map(Some)
    }
}

/// The caller-facing answer of a stored or fresh acquisition.
fn outcome_of(evaluated: EvaluatedLease) -> Result<AcquireLeaseOutcome, DomainError> {
    AcquireLeaseOutcome::of(evaluated).map_err(|_| {
        DomainError::Internal("an allowed acquisition carried no token or expiry".to_owned())
    })
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "leases_tests.rs"]
mod tests;
