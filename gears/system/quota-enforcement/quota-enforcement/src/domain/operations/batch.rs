//! The atomic batch debit: several debits of one logical operation, admitted
//! or refused as a whole.
//!
//! The gear validates the envelope, admits every item on its own attribution,
//! derives the envelope's idempotency scope from the union of the items'
//! subject sets, and answers a replay before the `mode` and size checks, so a
//! stored outcome survives a lowered size limit. Evaluation, the batch timer
//! and the all-or-nothing write belong to the storage transaction.
//!
//! The batch path feeds no `operation`-labelled series: the feature claims no
//! `operation = batch_debit` label. Denials and engine instruments are
//! recorded as on every other path.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use quota_enforcement_sdk::{
    BatchDebitItem, BatchDebitRequest, BatchDecision, BatchEntry, BatchMode, BatchRecord,
    BatchTimer, Decision, DecisionResult, EvaluatedBatch, IdempotencyScope, IdempotencySubjectKey,
    IdempotencyWrite, OperationType, PayloadHash, SubjectRef, TenantId, positive_amount,
};
use serde::Serialize;
use toolkit_macros::domain_model;

use super::service::{Operations, applicable_of, digest, policy_values};
use crate::domain::attribution::AdmittedEvaluation;
use crate::domain::engines::PreparedEvaluation;
use crate::domain::error::DomainError;
use crate::domain::pep::{actions, resources};
use crate::domain::ports::metrics::DenialReason;
use crate::domain::tokens;

/// The batch debit's bounds (`[quota-enforcement.operations]`).
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BatchLimits {
    /// Most items one batch may carry.
    pub max_items: usize,
    /// The batch-level evaluation timeout.
    pub timeout: Duration,
}

impl Default for BatchLimits {
    /// The platform defaults: 100 items, 250 ms.
    fn default() -> Self {
        Self {
            max_items: 100,
            timeout: Duration::from_millis(250),
        }
    }
}

/// What a batch's payload digest covers: everything but the envelope key.
#[derive(Serialize)]
struct BatchPayload<'a> {
    mode: BatchMode,
    items: &'a [quota_enforcement_sdk::BatchItemRequest],
}

// @cpt-flow:cpt-cf-quota-enforcement-flow-batch-debit:p1
// @cpt-dod:cpt-cf-quota-enforcement-dod-batch-debit-endpoint:p1
// @cpt-dod:cpt-cf-quota-enforcement-dod-batch-idempotency-timeout:p1
// @cpt-algo:cpt-cf-quota-enforcement-algo-batch-envelope-evaluation:p1
impl Operations<'_> {
    /// Debit several metrics as one logical operation.
    ///
    /// # Errors
    ///
    /// `InvalidArgument` for an empty batch, a non-positive amount or blank
    /// key (naming the item), a blank envelope key, duplicate item keys,
    /// mixed tenants, or more items than allowed; the admission error of any
    /// item; `MetricNotQuotaGated`; `NotYetImplemented` for `independent`
    /// mode; `BatchTimeout`; the storage error of a failed evaluation.
    pub async fn batch_debit(
        &self,
        ctx: &toolkit_security::SecurityContext,
        request: BatchDebitRequest,
    ) -> Result<BatchDecision, DomainError> {
        // @cpt-begin:cpt-cf-quota-enforcement-flow-batch-debit:p1:inst-bde-amount-if
        // @cpt-begin:cpt-cf-quota-enforcement-flow-batch-debit:p1:inst-bde-amount
        let (tenant, amounts) = self.validate_batch(&request)?;
        // @cpt-end:cpt-cf-quota-enforcement-flow-batch-debit:p1:inst-bde-amount
        // @cpt-end:cpt-cf-quota-enforcement-flow-batch-debit:p1:inst-bde-amount-if
        // @cpt-begin:cpt-cf-quota-enforcement-flow-batch-debit:p1:inst-bde-request
        let payload_hash = digest(&BatchPayload {
            mode: request.mode,
            items: &request.items,
        })?;
        let admitted = self.admit_items(ctx, &request).await?;
        // @cpt-end:cpt-cf-quota-enforcement-flow-batch-debit:p1:inst-bde-request
        // @cpt-begin:cpt-cf-quota-enforcement-flow-batch-debit:p1:inst-bde-resolve
        let scope = envelope_scope(tenant, &admitted, request.idempotency_key.clone());
        // @cpt-end:cpt-cf-quota-enforcement-flow-batch-debit:p1:inst-bde-resolve
        let keys: Vec<String> = request
            .items
            .iter()
            .map(|item| item.idempotency_key.clone())
            .collect();
        // @cpt-begin:cpt-cf-quota-enforcement-flow-batch-debit:p1:inst-bde-idem
        if let Some(decisions) = self.batch_replay(&scope, payload_hash).await? {
            return Ok(BatchDecision::of(keys, decisions));
        }
        // @cpt-end:cpt-cf-quota-enforcement-flow-batch-debit:p1:inst-bde-idem
        // @cpt-begin:cpt-cf-quota-enforcement-flow-batch-debit:p1:inst-bde-mode-if
        // @cpt-begin:cpt-cf-quota-enforcement-flow-batch-debit:p1:inst-bde-mode
        if request.mode == BatchMode::Independent {
            return Err(DomainError::NotYetImplemented {
                feature: "independent batch mode",
            });
        }
        // @cpt-end:cpt-cf-quota-enforcement-flow-batch-debit:p1:inst-bde-mode
        // @cpt-end:cpt-cf-quota-enforcement-flow-batch-debit:p1:inst-bde-mode-if
        // @cpt-begin:cpt-cf-quota-enforcement-flow-batch-debit:p1:inst-bde-size-if
        // @cpt-begin:cpt-cf-quota-enforcement-flow-batch-debit:p1:inst-bde-size
        if request.items.len() > self.batch.max_items {
            self.metrics.record_denial(DenialReason::InvalidArgument);
            return Err(DomainError::BulkTooLarge {
                items: request.items.len(),
                max: self.batch.max_items,
            });
        }
        // @cpt-end:cpt-cf-quota-enforcement-flow-batch-debit:p1:inst-bde-size
        // @cpt-end:cpt-cf-quota-enforcement-flow-batch-debit:p1:inst-bde-size-if
        // @cpt-begin:cpt-cf-quota-enforcement-flow-batch-debit:p1:inst-bde-envelope
        let decisions = self
            .run_batch(
                ctx,
                &request,
                &admitted,
                &amounts,
                IdempotencyWrite {
                    scope,
                    payload_hash,
                },
            )
            .await?;
        // @cpt-end:cpt-cf-quota-enforcement-flow-batch-debit:p1:inst-bde-envelope
        // @cpt-begin:cpt-cf-quota-enforcement-flow-batch-debit:p1:inst-bde-denied-if
        // @cpt-begin:cpt-cf-quota-enforcement-flow-batch-debit:p1:inst-bde-denied
        if let Some(denied) = decisions
            .iter()
            .find(|decision| matches!(decision.result, DecisionResult::Denied { .. }))
        {
            // Counted once per batch, under the first denied item's reason.
            self.count_denial(denied);
        }
        // @cpt-end:cpt-cf-quota-enforcement-flow-batch-debit:p1:inst-bde-denied
        // @cpt-end:cpt-cf-quota-enforcement-flow-batch-debit:p1:inst-bde-denied-if
        // @cpt-begin:cpt-cf-quota-enforcement-flow-batch-debit:p1:inst-bde-return
        Ok(BatchDecision::of(keys, decisions))
        // @cpt-end:cpt-cf-quota-enforcement-flow-batch-debit:p1:inst-bde-return
    }

    /// The envelope checks that need no PDP: not empty, every amount
    /// positive, every key present and item keys unique, one tenant.
    /// Returns that tenant and the amounts.
    fn validate_batch(
        &self,
        request: &BatchDebitRequest,
    ) -> Result<(TenantId, Vec<u64>), DomainError> {
        let refuse = |error: DomainError| {
            self.metrics.record_denial(DenialReason::InvalidArgument);
            error
        };
        let Some(first) = request.items.first() else {
            return Err(refuse(DomainError::InvalidArgument {
                field: "items",
                reason: tokens::BATCH_EMPTY,
            }));
        };
        let mut amounts = Vec::with_capacity(request.items.len());
        for (index, item) in request.items.iter().enumerate() {
            amounts.push(positive_amount(item.amount).ok_or_else(|| {
                refuse(DomainError::InvalidBatchItem {
                    index,
                    field: "amount",
                    reason: tokens::INVALID_AMOUNT,
                })
            })?);
        }
        self.validate_key(&request.idempotency_key)?;
        let mut seen = HashSet::with_capacity(request.items.len());
        for (index, item) in request.items.iter().enumerate() {
            if item.idempotency_key.trim().is_empty() {
                return Err(refuse(DomainError::InvalidBatchItem {
                    index,
                    field: "idempotency_key",
                    reason: tokens::IDEMPOTENCY_KEY_REQUIRED,
                }));
            }
            if !seen.insert(item.idempotency_key.as_str()) {
                return Err(refuse(DomainError::InvalidArgument {
                    field: "items",
                    reason: tokens::BATCH_ITEM_KEY_DUPLICATE,
                }));
            }
        }
        let tenant = first.attribution.tenant_id;
        if request
            .items
            .iter()
            .any(|item| item.attribution.tenant_id != tenant)
        {
            return Err(refuse(DomainError::InvalidArgument {
                field: "items",
                reason: tokens::BATCH_TENANT_MIXED,
            }));
        }
        Ok((tenant, amounts))
    }

    /// Admit every item on its own attribution and refuse a metric whose
    /// usage does not flow through Quota Enforcement.
    async fn admit_items(
        &self,
        ctx: &toolkit_security::SecurityContext,
        request: &BatchDebitRequest,
    ) -> Result<Vec<AdmittedEvaluation>, DomainError> {
        let mut admitted = Vec::with_capacity(request.items.len());
        for item in &request.items {
            let one = self
                .attribution
                .admit_evaluation(
                    ctx,
                    &resources::OPERATION,
                    actions::BATCH_DEBIT,
                    item.attribution.clone(),
                )
                .await?;
            // @cpt-begin:cpt-cf-quota-enforcement-algo-batch-envelope-evaluation:p1:inst-bev-gated
            self.classifications.ensure_quota_gated(&one.metric)?;
            // @cpt-end:cpt-cf-quota-enforcement-algo-batch-envelope-evaluation:p1:inst-bev-gated
            admitted.push(one);
        }
        Ok(admitted)
    }

    /// A stored envelope under `scope`: its decisions, or the payload
    /// mismatch when the key carried a different batch.
    async fn batch_replay(
        &self,
        scope: &IdempotencyScope,
        payload_hash: PayloadHash,
    ) -> Result<Option<Vec<Decision>>, DomainError> {
        let Some(record) = self.storage.lookup_idempotency(scope).await? else {
            return Ok(None);
        };
        if record.payload_hash != payload_hash {
            return Err(DomainError::IdempotencyPayloadMismatch);
        }
        let stored: BatchRecord = serde_json::from_value(record.decision_blob)
            .map_err(|error| DomainError::Internal(error.to_string()))?;
        Ok(Some(stored.decisions))
    }

    /// Hand the admitted items to storage under one timer, preparing a
    /// missing artifact within the same bounded budget a debit uses.
    async fn run_batch(
        &self,
        ctx: &toolkit_security::SecurityContext,
        request: &BatchDebitRequest,
        admitted: &[AdmittedEvaluation],
        amounts: &[u64],
        envelope: IdempotencyWrite,
    ) -> Result<Vec<Decision>, DomainError> {
        let items: Vec<BatchDebitItem> = request
            .items
            .iter()
            .zip(admitted)
            .zip(amounts)
            .map(|((item, admitted), amount)| batch_item(item, admitted, *amount))
            .collect();
        let projections: Vec<Option<gts::GtsTypeId>> = admitted
            .iter()
            .map(|admitted| self.user_projection(&admitted.metric))
            .collect();
        let entries: Vec<BatchEntry<'_>> = items
            .iter()
            .zip(admitted)
            .zip(&projections)
            .map(|((item, admitted), projection)| BatchEntry {
                item,
                scope: &admitted.attribution.access_scope,
                user_projection: projection.as_ref(),
            })
            .collect();
        let prepared = PreparedEvaluation::new(
            Arc::clone(&self.engines),
            Arc::clone(&self.artifacts),
            Arc::clone(&self.metrics),
            self.storage,
            self.preparation_max_attempts,
        );
        // One timer for every attempt, preparation retries included.
        let timer = Arc::new(BatchTimer::new(self.batch.timeout));
        let batch = EvaluatedBatch {
            envelope: &envelope,
            items: &entries,
            limits: self.evaluation,
            evaluate: prepared.evaluator(),
            timer: Arc::clone(&timer),
        };
        // Every item shares the tenant, so any item's scope covers the
        // envelope's own rows.
        let envelope_scope = &admitted
            .first()
            .ok_or_else(|| DomainError::Internal("an admitted batch has no items".to_owned()))?
            .attribution
            .access_scope;
        let outcome = prepared
            .run_within(Some(timer.as_ref()), || {
                self.storage
                    .apply_batch_debit(ctx, envelope_scope, &batch, &[])
            })
            .await?;
        Ok(outcome
            .into_inner()
            .into_iter()
            .map(|item| item.decision)
            .collect())
    }
}

/// The envelope's scope: the tenant every item shares, the sorted,
/// deduplicated union of every item's subject set, and the envelope key.
fn envelope_scope(
    tenant_id: TenantId,
    admitted: &[AdmittedEvaluation],
    key: String,
) -> IdempotencyScope {
    let subjects: Vec<SubjectRef> = admitted
        .iter()
        .flat_map(|item| item.attribution.subjects.iter().cloned())
        .collect();
    IdempotencyScope {
        tenant_id,
        subject_key: IdempotencySubjectKey::of(&subjects),
        operation_type: OperationType::BatchDebit,
        key,
    }
}

/// One admitted item as storage evaluates it; its own key rides as the item
/// scope, for identification only.
fn batch_item(
    item: &quota_enforcement_sdk::BatchItemRequest,
    admitted: &AdmittedEvaluation,
    amount: u64,
) -> BatchDebitItem {
    let (request, resource) = policy_values(admitted);
    BatchDebitItem {
        applicable: applicable_of(admitted),
        amount,
        request,
        resource,
        authorized: admitted.authorized,
        item_scope: Some(IdempotencyScope {
            tenant_id: admitted.attribution.tenant_id,
            subject_key: IdempotencySubjectKey::of(&admitted.attribution.subjects),
            operation_type: OperationType::BatchDebit,
            key: item.idempotency_key.clone(),
        }),
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "batch_tests.rs"]
mod tests;
