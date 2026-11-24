//! The atomic batch debit: several debits of one logical operation, admitted
//! or refused as a whole.
//!
//! The gear validates the envelope and its size, admits every item on its own
//! attribution, and answers a replay of the envelope before the `mode` check.
//! An item is a debit under its own key, in the key space of single debits: a
//! batch whose item key already names a record of a single debit fails as a
//! payload mismatch, and is replayed only as a whole, by its envelope key. The
//! size check comes before admission, so an oversized batch costs no PDP call,
//! and a lowered size limit applies to replays too. Evaluation, the batch
//! timer and the all-or-nothing write belong to the storage transaction.
//!
//! The batch path feeds no `operation`-labelled series: the feature claims no
//! `operation = batch_debit` label. Denials and engine instruments are
//! recorded as on every other path.

use crate::domain::admission::admission_span;
use futures::future::join_all;
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;
use tracing::Instrument as _;

use quota_enforcement_sdk::{
    BatchDebitItem, BatchDebitRequest, BatchDecision, BatchEntry, BatchMode, BatchRecord,
    BatchTimer, Decision, DecisionResult, EvaluatedBatch, IdempotencyScope, IdempotencyWrite,
    OperationType, PayloadHash, TenantId, TransitionOutcome, positive_amount,
};
use serde::Serialize;
use toolkit_macros::domain_model;

use super::service::{
    DebitPayload, Operations, applicable_of, digest, ensure_quota_gated, key_fault,
};
use crate::domain::attribution::AdmittedEvaluation;
use crate::domain::engines::PreparedEvaluation;
use crate::domain::error::DomainError;
use crate::domain::pep::{actions, resources};
use crate::domain::ports::metrics::{DenialReason, OperationKind};
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

/// How many items of one batch are admitted at a time, each with its own PDP
/// call.
const ADMISSION_CONCURRENCY: usize = 8;

/// What the envelope's payload digest covers: the mode, and each item's key
/// and its single-debit fingerprint, in item order.
#[derive(Serialize)]
struct BatchPayload<'a> {
    mode: BatchMode,
    items: Vec<BatchItemPayload<'a>>,
}

#[derive(Serialize)]
struct BatchItemPayload<'a> {
    idempotency_key: &'a str,
    #[serde(flatten)]
    debit: DebitPayload<'a>,
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
    #[tracing::instrument(name = "quota_enforcement.operations.batch_debit", skip_all, fields(item_count = request.items.len()))]
    pub async fn batch_debit(
        &self,
        request: BatchDebitRequest,
    ) -> Result<BatchDecision, DomainError> {
        // @cpt-begin:cpt-cf-quota-enforcement-flow-batch-debit:p1:inst-bde-amount-if
        // @cpt-begin:cpt-cf-quota-enforcement-flow-batch-debit:p1:inst-bde-amount
        let (tenant, amounts) = self.validate_batch(&request)?;
        // @cpt-end:cpt-cf-quota-enforcement-flow-batch-debit:p1:inst-bde-amount
        // @cpt-end:cpt-cf-quota-enforcement-flow-batch-debit:p1:inst-bde-amount-if
        // Before admission: every item costs a PDP call.
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
        // @cpt-begin:cpt-cf-quota-enforcement-flow-batch-debit:p1:inst-bde-request
        let admitted = self
            .admit_items(&request)
            .instrument(admission_span(request.items.len()))
            .await?;
        let payload_hash = digest(&BatchPayload {
            mode: request.mode,
            items: request
                .items
                .iter()
                .zip(&admitted)
                .map(|(item, admitted)| BatchItemPayload {
                    idempotency_key: &item.idempotency_key,
                    debit: DebitPayload::of(admitted, item.amount),
                })
                .collect(),
        })?;
        // @cpt-end:cpt-cf-quota-enforcement-flow-batch-debit:p1:inst-bde-request
        // @cpt-begin:cpt-cf-quota-enforcement-flow-batch-debit:p1:inst-bde-resolve
        let scope = IdempotencyScope {
            tenant_id: tenant,
            operation_type: OperationType::BatchDebit,
            key: request.idempotency_key.clone(),
        };
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
        let items = batch_items(&request, &admitted, &amounts)?;
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
        // @cpt-begin:cpt-cf-quota-enforcement-flow-batch-debit:p1:inst-bde-envelope
        let (decisions, replayed) = self
            .run_batch(
                &items,
                &admitted,
                IdempotencyWrite {
                    scope,
                    payload_hash,
                },
            )
            .await?;
        // @cpt-end:cpt-cf-quota-enforcement-flow-batch-debit:p1:inst-bde-envelope
        // @cpt-begin:cpt-cf-quota-enforcement-flow-batch-debit:p1:inst-bde-denied-if
        // @cpt-begin:cpt-cf-quota-enforcement-flow-batch-debit:p1:inst-bde-denied
        if !replayed
            && let Some(denied) = decisions
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
        self.validate_key("idempotency_key", &request.idempotency_key)?;
        let mut seen = HashSet::with_capacity(request.items.len());
        for (index, item) in request.items.iter().enumerate() {
            if let Some(reason) = key_fault(&item.idempotency_key) {
                return Err(refuse(DomainError::InvalidBatchItem {
                    index,
                    field: "idempotency_key",
                    reason,
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
    /// usage does not flow through Quota Enforcement. The items are admitted
    /// [`ADMISSION_CONCURRENCY`] at a time, each with its own PDP call; the
    /// error returned is that of the first item that fails, in request order.
    async fn admit_items(
        &self,
        request: &BatchDebitRequest,
    ) -> Result<Vec<AdmittedEvaluation>, DomainError> {
        let mut admitted = Vec::with_capacity(request.items.len());
        for group in request.items.chunks(ADMISSION_CONCURRENCY) {
            for one in join_all(group.iter().map(|item| self.admit_item(item))).await {
                admitted.push(one?);
            }
        }
        Ok(admitted)
    }

    async fn admit_item(
        &self,
        item: &quota_enforcement_sdk::BatchItemRequest,
    ) -> Result<AdmittedEvaluation, DomainError> {
        let one = self
            .attribution()
            .admit_evaluation(
                &resources::OPERATION,
                actions::BATCH_DEBIT,
                item.attribution.clone(),
            )
            .await?;
        // @cpt-begin:cpt-cf-quota-enforcement-algo-batch-envelope-evaluation:p1:inst-bev-gated
        ensure_quota_gated(&one.metric)?;
        // @cpt-end:cpt-cf-quota-enforcement-algo-batch-envelope-evaluation:p1:inst-bev-gated
        Ok(one)
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
        items: &[BatchDebitItem],
        admitted: &[AdmittedEvaluation],
        envelope: IdempotencyWrite,
    ) -> Result<(Vec<Decision>, bool), DomainError> {
        let entries: Vec<BatchEntry<'_>> = items
            .iter()
            .zip(admitted)
            .map(|(item, admitted)| BatchEntry {
                item,
                scope: &admitted.attribution.access_scope,
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
        let first = &admitted
            .first()
            .ok_or_else(|| DomainError::Internal("an admitted batch has no items".to_owned()))?
            .attribution;
        let outcome = prepared
            .run_within(Some(timer.as_ref()), || {
                self.storage
                    .apply_batch_debit(&first.actor, &first.access_scope, &batch, &[])
            })
            .await;
        let outcome = self.contended(OperationKind::Debit, outcome)?;
        let replayed = matches!(outcome, TransitionOutcome::NoOp(_));
        let decisions = outcome
            .into_inner()
            .into_iter()
            .map(|item| item.decision)
            .collect();
        Ok((decisions, replayed))
    }
}

/// Every admitted item as storage evaluates it: a debit under the item's own
/// key, with the fingerprint a single debit of the item's request has.
fn batch_items(
    request: &BatchDebitRequest,
    admitted: &[AdmittedEvaluation],
    amounts: &[u64],
) -> Result<Vec<BatchDebitItem>, DomainError> {
    request
        .items
        .iter()
        .zip(admitted)
        .zip(amounts)
        .map(|((item, admitted), amount)| {
            Ok(BatchDebitItem {
                applicable: applicable_of(admitted),
                amount: *amount,
                request: serde_json::Value::Object(admitted.request.clone()),
                metric_id: admitted.metric_id.clone(),
                authorized: admitted.authorized,
                idempotency: IdempotencyWrite {
                    scope: IdempotencyScope {
                        tenant_id: admitted.attribution.tenant_id,
                        operation_type: OperationType::Debit,
                        key: item.idempotency_key.clone(),
                    },
                    payload_hash: digest(&DebitPayload::of(admitted, item.amount))?,
                },
            })
        })
        .collect()
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "batch_tests.rs"]
mod tests;
