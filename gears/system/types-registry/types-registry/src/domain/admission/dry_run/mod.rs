//! Run a dry-run batch: predict against one snapshot and an [`AdmissionView`]
//! overlay, then publish the outcomes after releasing the snapshot.
//!
//! Reuse committing-path ordering, refusal helpers, evaluation and commit checks.
//! Successful virtual writes feed later candidates; publish outcomes only after
//! releasing the snapshot.
//!
//! No drift retries: the snapshot is fixed and the overlay has one writer.
//! A revision-vector mismatch indicates inconsistent view reads and propagates.

mod publish;
pub mod view;

use std::collections::HashMap;
use std::sync::Arc;

use time::OffsetDateTime;
use toolkit_db::DBProvider;
use toolkit_db::secure::AccessScope;
use toolkit_macros::domain_model;
use tracing::Instrument;
use uuid::Uuid;

use self::view::{AdmissionView, ItemOutcomeWrite};
use super::batch;
use super::deletion;
use super::errors::{ItemFailure, WorkerError};
use super::tuning::Tuning;
use super::unit::{CommitRequest, EvaluationTarget, PreparedUnit, commit_prepared_in, evaluate_in};
use super::worker::{ItemOutcome, OperationOutcome, read_operation, stored_outcome};
use crate::domain::admission::{AdmissionFailureReason, Precondition};
use crate::domain::enums::{OperationItemStatus, OperationKind};
use crate::domain::ports::{OperationItemRow, OperationRow, Stores, snapshot_read};
use crate::observability;

/// What the pass predicts for one candidate.
///
/// A success carries the commit path's **own** terminal item write rather than a
/// re-derivation of it: `commit_creation` already decides that a dry run records
/// neither revision nor resource version, because `ck_tr_operation_item_state`
/// says so, and deciding it a second time here is how the two come to disagree.
#[domain_model]
#[derive(Clone, Debug)]
pub enum Predicted {
    Terminal {
        gts_uuid: Uuid,
        write: ItemOutcomeWrite,
    },
    Refused(ItemFailure),
}

impl Predicted {
    /// The status this prediction will be published under — the input the
    /// dependency-blocking rule reads.
    pub(super) const fn status(&self) -> OperationItemStatus {
        match self {
            Self::Terminal {
                write: ItemOutcomeWrite::Succeeded(_),
                ..
            } => OperationItemStatus::Succeeded,
            Self::Terminal {
                write: ItemOutcomeWrite::Unchanged { .. },
                ..
            } => OperationItemStatus::Unchanged,
            Self::Refused(_) => OperationItemStatus::Failed,
        }
    }
}

/// One candidate's predicted commit.
///
/// `write` is `None` when the commit path recorded the item write itself — every
/// registration — and `Some` for a deletion, whose committing counterpart writes
/// the item outside its transaction and therefore not through the view.
struct PredictedCommit {
    gts_uuid: Uuid,
    write: Option<ItemOutcomeWrite>,
}

/// Simulate a batch in one snapshot, then atomically publish outcomes and completion.
pub(super) async fn run_batch(
    stores: &Arc<dyn Stores>,
    db: &DBProvider<WorkerError>,
    scope: &AccessScope,
    tuning: Tuning<'_>,
    operation: &OperationRow,
    items: &Arc<[OperationItemRow]>,
    now: OffsetDateTime,
) -> Result<OperationOutcome, WorkerError> {
    let operation_id = operation.id;
    let predictions =
        predict_batch(stores, db, scope, tuning, operation, Arc::clone(items), now).await?;
    let published =
        publish::publish(stores, db, scope, operation_id, items, &predictions, now).await?;
    // A concurrent pass won the CAS; keep its stored outcomes, loading all items once.
    let terminalized: HashMap<i64, OperationItemRow> = if published.recorded.contains(&false) {
        read_operation(stores, db, scope, operation_id)
            .await?
            .1
            .into_iter()
            .map(|row| (row.id, row))
            .collect()
    } else {
        HashMap::new()
    };
    let mut outcomes = Vec::with_capacity(items.len());
    for ((item, prediction), won) in items.iter().zip(&predictions).zip(published.recorded) {
        if !won {
            let row = terminalized
                .get(&item.id)
                .ok_or(WorkerError::OperationNotFound { operation_id })?;
            outcomes.push(stored_outcome(row)?);
            continue;
        }
        let reported = publish::published_outcome(operation_id, item, prediction, tuning.metrics);
        outcomes.push(ItemOutcome {
            gts_id: item.gts_id.clone(),
            status: reported.status,
            gts_uuid: reported.gts_uuid,
            resource_version: reported.resource_version,
            revision_no: reported.revision_no,
            failure: match prediction {
                Predicted::Refused(failure) => Some(failure.clone()),
                Predicted::Terminal { .. } => None,
            },
        });
    }
    Ok(OperationOutcome {
        operation_id,
        already_terminal: false,
        items: outcomes,
    })
}

/// Predict a batch without entity writes; return outcomes in submission order.
/// Release the shared read-only snapshot before the caller publishes results.
///
/// # Errors
/// [`WorkerError`] for infrastructure failure; [`Predicted::Refused`] for refusal.
async fn predict_batch(
    stores: &Arc<dyn Stores>,
    db: &DBProvider<WorkerError>,
    scope: &AccessScope,
    tuning: Tuning<'_>,
    operation: &OperationRow,
    items: Arc<[OperationItemRow]>,
    now: OffsetDateTime,
) -> Result<Vec<Predicted>, WorkerError> {
    // Owned handles for the `'static` transaction closure. The items are shared
    // rather than copied: a full batch's authored documents run to megabytes and
    // the caller holds them for the whole pass anyway.
    let stores = Arc::clone(stores);
    let scope = scope.clone();
    let limits = *tuning.limits;
    let metrics = Arc::clone(tuning.metrics);
    let allow_force = tuning.allow_compatibility_force;
    let operation_id = operation.id;
    // The operation row, not the first item: the committing pass reads the same
    // field, and an empty batch has no first item to read a kind from.
    let kind = operation.kind;

    db.transaction_with_config(snapshot_read(&db.db()), move |tx| {
        Box::pin(async move {
            let view = AdmissionView::new(Arc::clone(&stores));
            // The two kinds order by opposite relations, exactly as the
            // committing pass orders them — and a deletion's edges are read
            // through this batch's own snapshot rather than a second one.
            let order = if kind == OperationKind::Deletion {
                let gts_ids: Vec<String> = items.iter().map(|item| item.gts_id.clone()).collect();
                batch::order_deletions(&view, tx, &scope, &gts_ids).await?
            } else {
                batch::registration_order(&items)
            };
            let predictions = batch::run_ordered(
                &items,
                &order,
                |index, refusal| {
                    let view = &view;
                    let scope = &scope;
                    let limits = &limits;
                    let metrics = &metrics;
                    let item = &items[index];
                    async move {
                        match refusal {
                            Some(failure) => Ok(Predicted::Refused(failure)),
                            None => {
                                predict_item(
                                    view,
                                    tx,
                                    scope,
                                    limits,
                                    metrics,
                                    allow_force,
                                    item,
                                    now,
                                )
                                .instrument(observability::unit_span(
                                    operation_id,
                                    &item.gts_id,
                                    item.kind,
                                    item.dry_run,
                                    item.id,
                                ))
                                .await
                            }
                        }
                    }
                },
                Predicted::status,
            )
            .await?;

            // `order_batch` partitions the batch into the ordered and the cyclic,
            // so every position is filled.
            items
                .iter()
                .zip(predictions)
                .map(|(item, prediction)| {
                    prediction.ok_or(WorkerError::MissingPrediction { item_id: item.id })
                })
                .collect::<Result<Vec<_>, _>>()
        })
    })
    .await
}

/// Predict in a tentative layer; retain it on success and discard it on any
/// refusal or error, including failures after virtual writes.
#[expect(
    clippy::too_many_arguments,
    reason = "mirrors `predict_commit`'s parameter list one for one; a shared context struct would have to be threaded through both and saves nothing"
)]
async fn predict_item(
    view: &AdmissionView,
    tx: &toolkit_db::DbTx<'_>,
    scope: &AccessScope,
    limits: &crate::config::Limits,
    metrics: &Arc<dyn crate::domain::ports::metrics::AdmissionMetrics>,
    allow_compatibility_force: bool,
    item: &OperationItemRow,
    now: OffsetDateTime,
) -> Result<Predicted, WorkerError> {
    let layer = view.begin_candidate().await;
    let committed = predict_commit(
        view,
        tx,
        scope,
        limits,
        metrics,
        allow_compatibility_force,
        item,
        now,
    )
    .await;
    let refusal = match committed {
        Ok(Ok(commit)) => {
            let recorded = match commit.write {
                Some(write) => Some(write),
                None => view.item_write(item.id).await,
            };
            let Some(write) = recorded else {
                view.discard_candidate(layer).await;
                return Err(WorkerError::MissingItemWrite { item_id: item.id });
            };
            view.keep_candidate(layer);
            tracing::debug!(
                operation_item_id = item.id,
                gts_id = %item.gts_id,
                "types_registry predicted a candidate would be admitted"
            );
            return Ok(Predicted::Terminal {
                gts_uuid: commit.gts_uuid,
                write,
            });
        }
        // The second arm is a refusal the commit path found *after* it had
        // begun writing. Its real counterpart rolls the transaction back; here
        // the layer is discarded, which must also undo the dependent refresh
        // that ran before it — so the two are one case.
        Ok(Err(failure)) | Err(WorkerError::RefusedAfterWrite(failure)) => failure,
        Err(error) => {
            view.discard_candidate(layer).await;
            return Err(error);
        }
    };
    view.discard_candidate(layer).await;
    Ok(Predicted::Refused(refusal))
}

/// Evaluate and commit one candidate against the view.
///
/// Evaluation stays in the batch snapshot; `commit_prepared_in` then selects
/// the same commit as the persistent path from the stored precondition.
#[expect(
    clippy::too_many_arguments,
    reason = "the evaluation context the committing `process_item` also takes; the two are read side by side and keeping the shapes identical is the point"
)]
async fn predict_commit(
    view: &AdmissionView,
    tx: &toolkit_db::DbTx<'_>,
    scope: &AccessScope,
    limits: &crate::config::Limits,
    metrics: &Arc<dyn crate::domain::ports::metrics::AdmissionMetrics>,
    allow_compatibility_force: bool,
    item: &OperationItemRow,
    now: OffsetDateTime,
) -> Result<Result<PredictedCommit, ItemFailure>, WorkerError> {
    if item.kind == OperationKind::Deletion {
        return predict_deletion(view, tx, scope, limits, item, now).await;
    }
    let payload = item
        .request_payload
        .as_deref()
        .ok_or(WorkerError::MissingPayload { item_id: item.id })?;

    let prepared = evaluate_in(
        view,
        tx,
        scope,
        EvaluationTarget {
            gts_id: &item.gts_id,
            canonical_body: payload,
            operation_item_id: item.id,
            precondition: item.precondition,
            // The deployment waiver applies to a prediction exactly as it does to
            // the commit it predicts; a dry run waives nothing of its own.
            force: item.compat_forced && allow_compatibility_force,
            labels: item.pass_labels(),
        },
        limits,
        metrics,
        Some(item),
    )
    .await?;
    let prepared = match prepared {
        Ok(prepared) => prepared,
        Err(failure) => return Ok(Err(failure)),
    };
    let hit = matches!(&prepared, PreparedUnit::Unchanged(_));
    metrics.unchanged_probe(hit);

    let committed = commit_prepared_in(
        view,
        tx,
        scope,
        CommitRequest {
            prepared: &prepared,
            precondition: item.precondition,
            now,
            limits: *limits,
            metrics,
        },
    )
    .await?;
    // The commit path recorded the item write itself; a registration's terminal
    // values are its to decide.
    Ok(committed.map(|commit| PredictedCommit {
        gts_uuid: commit.gts_uuid(),
        write: None,
    }))
}

/// Run `commit_deletion` against the view without document evaluation.
/// Carry the values that `terminalize_deletion` would persist on the real path.
async fn predict_deletion(
    view: &AdmissionView,
    tx: &toolkit_db::DbTx<'_>,
    scope: &AccessScope,
    limits: &crate::config::Limits,
    item: &OperationItemRow,
    now: OffsetDateTime,
) -> Result<Result<PredictedCommit, ItemFailure>, WorkerError> {
    let Precondition::Version(expected) = item.precondition else {
        // Acceptance refuses an absent version for a deletion, so a stored item
        // in this shape disagrees with the rules that admitted it.
        return Ok(Err(ItemFailure::new(
            AdmissionFailureReason::PreconditionFailed,
            format!(
                "stored deletion item {} carries no expected_resource_version",
                item.id
            ),
        )));
    };
    let span = tracing::Span::current();
    let committed =
        deletion::commit_deletion(view, tx, scope, &item.gts_id, expected, limits, &span, now)
            .await?;
    Ok(committed.map(|commit| PredictedCommit {
        gts_uuid: commit.gts_uuid,
        write: Some(ItemOutcomeWrite::Succeeded(
            commit.item_outcome(item.dry_run),
        )),
    }))
}
