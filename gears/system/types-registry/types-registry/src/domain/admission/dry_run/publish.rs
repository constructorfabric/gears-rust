//! Publish dry-run outcomes after releasing the read snapshot.
//!
//! Terminal item writes and operation completion share one transaction: item
//! terminalization clears `request_payload`, so partial publication would prevent
//! whole-batch recovery. Failure preserves payloads for redelivery to re-predict.
//!
//! Release the snapshot first to avoid self-deadlock on `SQLite's` shared connection.
//! Real commits can recover item-by-item because their entity writes are durable.

use std::sync::Arc;

use time::OffsetDateTime;
use toolkit_db::DBProvider;
use toolkit_db::secure::AccessScope;
use uuid::Uuid;

use super::super::errors::{ItemFailure, WorkerError};
use super::Predicted;
use super::view::ItemOutcomeWrite;
use crate::domain::enums::OperationItemStatus;
use crate::domain::ports::metrics::{AdmissionMetrics, RefusalStage, TerminalStatus};
use crate::domain::ports::{ItemSuccess, OperationItemRow, Stores};

/// One item's terminal write, as the publication transaction issues it.
///
/// Three arms rather than a `Result`, because the storage port has three calls
/// and `ck_tr_operation_item_state` gives each a different column shape.
#[derive(Clone)]
enum PublishWrite {
    Succeeded(ItemSuccess),
    Unchanged { resource_version: i64 },
    Failed { error_payload: String },
}

impl PublishWrite {
    fn of(prediction: &Predicted) -> Result<Self, WorkerError> {
        Ok(match prediction {
            Predicted::Terminal {
                write: ItemOutcomeWrite::Succeeded(outcome),
                ..
            } => Self::Succeeded(*outcome),
            Predicted::Terminal {
                write: ItemOutcomeWrite::Unchanged { resource_version },
                ..
            } => Self::Unchanged {
                resource_version: *resource_version,
            },
            Predicted::Refused(failure) => Self::Failed {
                error_payload: failure
                    .to_payload()
                    .map_err(WorkerError::FailureUnencodable)?,
            },
        })
    }
}

/// Which items this pass won, in submission order.
///
/// A `false` is the ordinary overlapping-pass outcome: the item was already
/// terminal, so its stored outcome stands and this pass reports that instead.
pub(super) struct Published {
    pub recorded: Vec<bool>,
}

/// Write every outcome and complete the operation, atomically.
///
/// # Errors
/// [`WorkerError`] for an infrastructure failure. Nothing is written when one
/// occurs, including the completion.
pub(super) async fn publish(
    stores: &Arc<dyn Stores>,
    db: &DBProvider<WorkerError>,
    scope: &AccessScope,
    operation_id: Uuid,
    items: &[OperationItemRow],
    predictions: &[Predicted],
    now: OffsetDateTime,
) -> Result<Published, WorkerError> {
    let writes: Vec<(i64, PublishWrite)> = items
        .iter()
        .zip(predictions)
        .map(|(item, prediction)| Ok((item.id, PublishWrite::of(prediction)?)))
        .collect::<Result<_, WorkerError>>()?;
    let tx_stores = Arc::clone(stores);
    let tx_scope = scope.clone();
    let recorded = db
        .transaction(move |tx| {
            // The retry closure owns each attempt's inputs. A retried attempt
            // issues the same writes against the same rows, so it is the same
            // publication rather than a second one.
            let writes = writes.clone();
            let tx_scope = tx_scope.clone();
            let tx_stores = Arc::clone(&tx_stores);
            Box::pin(async move {
                let mut recorded = Vec::with_capacity(writes.len());
                for (item_id, write) in &writes {
                    let won = match write {
                        PublishWrite::Succeeded(outcome) => {
                            tx_stores
                                .mark_item_succeeded(tx, &tx_scope, *item_id, *outcome, now)
                                .await?
                        }
                        PublishWrite::Unchanged { resource_version } => {
                            tx_stores
                                .mark_item_unchanged(
                                    tx,
                                    &tx_scope,
                                    *item_id,
                                    *resource_version,
                                    now,
                                )
                                .await?
                        }
                        PublishWrite::Failed { error_payload } => {
                            tx_stores
                                .mark_item_failed(
                                    tx,
                                    &tx_scope,
                                    *item_id,
                                    error_payload.clone(),
                                    now,
                                )
                                .await?
                        }
                    };
                    recorded.push(won);
                }
                // In this transaction, not after it: see the module header.
                // A `false` means an overlapping pass completed the operation
                // first, which its own publication already recorded.
                tx_stores
                    .mark_completed(tx, &tx_scope, operation_id, now)
                    .await?;
                Ok(recorded)
            })
        })
        .await?;
    Ok(Published { recorded })
}

/// Report, log and count one published prediction.
///
/// Counted only for the pass that won the item's compare-and-swap, exactly as a
/// committing pass's refusal is.
pub(super) fn published_outcome(
    operation_id: Uuid,
    item: &OperationItemRow,
    prediction: &Predicted,
    metrics: &Arc<dyn AdmissionMetrics>,
) -> PublishedOutcome {
    match prediction {
        Predicted::Terminal {
            gts_uuid,
            write: ItemOutcomeWrite::Succeeded(outcome),
        } => {
            let (revision_no, resource_version) = outcome.columns();
            tracing::info!(
                %operation_id,
                operation_item_id = item.id,
                gts_id = %item.gts_id,
                "types_registry predicted a candidate would be admitted"
            );
            metrics.candidate_terminalized(TerminalStatus::Succeeded, item.pass_labels());
            PublishedOutcome {
                status: OperationItemStatus::Succeeded,
                gts_uuid: Some(*gts_uuid),
                resource_version,
                revision_no,
            }
        }
        Predicted::Terminal {
            gts_uuid,
            write: ItemOutcomeWrite::Unchanged { resource_version },
        } => {
            tracing::info!(
                %operation_id,
                operation_item_id = item.id,
                gts_id = %item.gts_id,
                resource_version,
                "types_registry predicted a candidate's content is already current"
            );
            metrics.candidate_terminalized(TerminalStatus::Unchanged, item.pass_labels());
            PublishedOutcome {
                status: OperationItemStatus::Unchanged,
                gts_uuid: Some(*gts_uuid),
                resource_version: Some(*resource_version),
                revision_no: None,
            }
        }
        Predicted::Refused(failure) => published_refusal(operation_id, item, failure, metrics),
    }
}

/// The refusal half, which counts one more series than a success does.
fn published_refusal(
    operation_id: Uuid,
    item: &OperationItemRow,
    failure: &ItemFailure,
    metrics: &Arc<dyn AdmissionMetrics>,
) -> PublishedOutcome {
    metrics.candidate_terminalized(TerminalStatus::Failed, item.pass_labels());
    metrics.refused(
        RefusalStage::Admission,
        failure.reason.metric_label(),
        item.pass_labels(),
    );
    tracing::warn!(
        %operation_id,
        operation_item_id = item.id,
        gts_id = %item.gts_id,
        reason = %failure.reason,
        "types_registry predicted a candidate would be refused"
    );
    PublishedOutcome {
        status: OperationItemStatus::Failed,
        gts_uuid: None,
        resource_version: None,
        revision_no: None,
    }
}

/// The reportable half of a published prediction, without the failure the caller
/// already holds.
pub(super) struct PublishedOutcome {
    pub status: OperationItemStatus,
    pub gts_uuid: Option<Uuid>,
    pub resource_version: Option<i64>,
    pub revision_no: Option<i32>,
}
