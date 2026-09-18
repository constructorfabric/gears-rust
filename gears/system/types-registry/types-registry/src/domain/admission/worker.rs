//! Admission worker entry point: [`run_operation`] (SPEC §8.1).
//!
//! Returns directly for deterministic tests; T21's outbox handler maps the result
//! to `Ok` / `Retry` / `Reject`. Infrastructure faults return [`WorkerError`];
//! candidate refusals are terminal [`ItemFailure`] outcomes.
//!
//! Process items in dependency order. Failed dependencies block their downstream;
//! independent candidates proceed. Stored preconditions select creation or revision.

use std::sync::Arc;
use std::time::Instant;

use time::OffsetDateTime;
use toolkit_db::secure::{AccessScope, ScopeError};
use toolkit_db::{DBProvider, DbError};
use tracing::{Instrument, Span};
use uuid::Uuid;

use super::batch::{self, order_deletions};
use super::deletion;
use super::dry_run;
pub use super::errors::{ItemFailure, WorkerError};
use super::graph::BatchOrder;
pub use super::outcome::{ItemOutcome, OperationOutcome};
use super::outcome::{read_operation, stored_outcome};
use super::revision::{CommittedUnit, RevisionCommit};
pub use super::tuning::Tuning;
use super::tuning::effective_force;
use super::unit::{CommitRequest, EvaluationTarget, PreparedUnit, commit_prepared_in, evaluate};
use super::vector::VectorDrift;
use crate::domain::admission::AdmissionFailureReason;
use crate::domain::admission::Precondition;
use crate::domain::enums::{OperationItemStatus, OperationKind, OperationStatus};
use crate::domain::ports::metrics::{AdmissionMetrics, RefusalStage, TerminalStatus};
use crate::domain::ports::{OperationItemRow, OperationRow, Stores, commit_write, snapshot_read};
use crate::observability;

/// Perform one full admission pass over an operation.
///
/// Each invocation rebuilds its transient store and re-reads the database.
///
/// # Errors
/// [`WorkerError`] for an infrastructure failure. A candidate-level refusal is
/// recorded on its item and reported in [`OperationOutcome`], not returned here.
pub async fn run_operation(
    stores: &Arc<dyn Stores>,
    db: &DBProvider<WorkerError>,
    scope: &AccessScope,
    tuning: Tuning<'_>,
    operation_id: Uuid,
    now: OffsetDateTime,
) -> Result<OperationOutcome, WorkerError> {
    // Open before the first read; populate operation fields after loading it.
    let span = observability::operation_span(operation_id);
    let started = Instant::now();
    let outcome = run_operation_inner(stores, db, scope, tuning, operation_id, now)
        .instrument(span)
        .await;
    // Include failed passes in the duration histogram.
    tuning.metrics.observe_operation_duration(started.elapsed());
    outcome
}

/// [`run_operation`]'s body, running inside the operation span.
async fn run_operation_inner(
    stores: &Arc<dyn Stores>,
    db: &DBProvider<WorkerError>,
    scope: &AccessScope,
    tuning: Tuning<'_>,
    operation_id: Uuid,
    now: OffsetDateTime,
) -> Result<OperationOutcome, WorkerError> {
    // Step 1: one snapshot keeps operation/items consistent. `mark_running` only
    // changes the operation, so items can be read first.
    let (operation, items) = read_operation(stores, db, scope, operation_id).await?;
    // Shared rather than copied from here on: both passes hand the slice to a
    // `'static` transaction closure, and every row carries its authored document.
    let items: Arc<[OperationItemRow]> = items.into();
    observability::record_operation_facts(&Span::current(), operation.kind, operation.dry_run);

    // T21 at-least-once delivery: terminal operations return stored outcomes without writes.
    if operation.status == OperationStatus::Completed {
        return already_terminal(operation_id, &items);
    }

    if !mark_running(stores, db, scope, operation_id, now).await? {
        tracing::warn!(
            %operation_id,
            "types_registry operation was already running; continuing with CAS-protected items"
        );
    }

    // A dry run of either kind is predicted whole — one snapshot, one overlay,
    // no entity-state write — and its outcomes are published afterwards.
    if operation.dry_run {
        return dry_run::run_batch(stores, db, scope, tuning, &operation, &items, now).await;
    }

    commit_pass(stores, db, scope, tuning, &operation, &items, now).await
}

/// Order, admit and complete a batch; [`run_operation_inner`] selects the pass.
async fn commit_pass(
    stores: &Arc<dyn Stores>,
    db: &DBProvider<WorkerError>,
    scope: &AccessScope,
    tuning: Tuning<'_>,
    operation: &OperationRow,
    items: &[OperationItemRow],
    now: OffsetDateTime,
) -> Result<OperationOutcome, WorkerError> {
    let operation_id = operation.id;
    // Steps 1–2: order the whole batch first. Registration visits dependencies
    // first; deletion visits dependants first so they cannot block their own batch's targets.
    let order = if operation.kind == OperationKind::Deletion {
        deletion_order(stores, db, scope, items).await?
    } else {
        batch::registration_order(items)
    };
    let outcomes = batch::run_ordered(
        items,
        &order,
        |index, refusal| async move {
            let item = &items[index];
            match refusal {
                Some(failure) => {
                    refuse_unevaluated(stores, db, scope, tuning, operation_id, item, failure, now)
                        .await
                }
                None => {
                    process_item(stores, db, scope, tuning, operation_id, item, now)
                        .instrument(unit_span(operation_id, item))
                        .await
                }
            }
        },
        |outcome| outcome.status,
    )
    .await?;

    mark_completed(stores, db, scope, operation_id, now).await?;

    Ok(OperationOutcome {
        operation_id,
        already_terminal: false,
        // `order_batch` partitions the candidate set into the ordered and the
        // cyclic, so every position is filled; the fallback reports what the store
        // holds rather than dropping an item the operation owes an outcome.
        items: items
            .iter()
            .zip(outcomes)
            .map(|(item, outcome)| outcome.map_or_else(|| stored_outcome(item), Ok))
            .collect::<Result<Vec<_>, _>>()?,
    })
}

/// Read deletion order in one snapshot: resolve candidate IDs, then their edges.
/// Missing entities contribute no edges; their commits fail `precondition_failed`.
async fn deletion_order(
    stores: &Arc<dyn Stores>,
    db: &DBProvider<WorkerError>,
    scope: &AccessScope,
    items: &[OperationItemRow],
) -> Result<BatchOrder, WorkerError> {
    let stores_tx = Arc::clone(stores);
    let scope_tx = scope.clone();
    // Only the identifiers cross into the closure: `order_deletions` reads
    // nothing else from an item, and the rows carry the authored documents.
    let gts_ids: Vec<String> = items.iter().map(|item| item.gts_id.clone()).collect();
    db.transaction_with_config(snapshot_read(&db.db()), move |tx| {
        Box::pin(async move { order_deletions(stores_tx.as_ref(), tx, &scope_tx, &gts_ids).await })
    })
    .await
}

fn unit_span(operation_id: Uuid, item: &OperationItemRow) -> Span {
    observability::unit_span(operation_id, &item.gts_id, item.kind, item.dry_run, item.id)
}

/// Terminalize a candidate the batch refused before it could be evaluated — a
/// cycle member, or one whose in-batch dependency failed.
///
/// An item an earlier pass already decided is left alone: `record_failure`'s CAS
/// reports the stored outcome instead, which is the same rule an evaluated
/// refusal follows.
#[expect(
    clippy::too_many_arguments,
    reason = "same context as `commit_pass`, plus the item and the failure it is being terminalized with"
)]
async fn refuse_unevaluated(
    stores: &Arc<dyn Stores>,
    db: &DBProvider<WorkerError>,
    scope: &AccessScope,
    tuning: Tuning<'_>,
    operation_id: Uuid,
    item: &OperationItemRow,
    failure: ItemFailure,
    now: OffsetDateTime,
) -> Result<ItemOutcome, WorkerError> {
    if item.status != OperationItemStatus::Pending && item.status != OperationItemStatus::Running {
        return stored_outcome(item);
    }
    record_failure(
        stores,
        db,
        scope,
        operation_id,
        item,
        failure,
        now,
        tuning.metrics,
    )
    .instrument(unit_span(operation_id, item))
    .await
}

/// Extract `DbErr` from the two wrapping [`WorkerError`] variants; others return
/// `None` to stop retries (e.g. store-build failure or an already-terminal item).
/// `sea_orm` appears because `Db::transaction_with_retry` requires driver errors
/// to classify backend contention.
#[allow(unknown_lints)]
#[allow(de0301_no_infra_in_domain)]
const fn retryable_db_err(e: &WorkerError) -> Option<&sea_orm::DbErr> {
    match e {
        WorkerError::Storage(ScopeError::Db(inner)) | WorkerError::Db(DbError::Sea(inner)) => {
            Some(inner)
        }
        _ => None,
    }
}

async fn prepare(
    stores: &Arc<dyn Stores>,
    db: &DBProvider<WorkerError>,
    scope: &AccessScope,
    item: &OperationItemRow,
    payload: &str,
    tuning: Tuning<'_>,
) -> Result<Result<PreparedUnit, ItemFailure>, WorkerError> {
    let prepared = evaluate(
        stores,
        db,
        scope,
        EvaluationTarget {
            gts_id: &item.gts_id,
            canonical_body: payload,
            operation_item_id: item.id,
            precondition: item.precondition,
            force: effective_force(item.compat_forced, &tuning),
            labels: item.pass_labels(),
        },
        tuning.limits,
        tuning.metrics,
        Some(item),
    )
    .await?;
    let hit = matches!(&prepared, Ok(PreparedUnit::Unchanged(_)));
    tuning.metrics.unchanged_probe(hit);
    if hit {
        tracing::debug!(operation_item_id = item.id, gts_id = %item.gts_id, "types_registry unchanged probe hit");
    }
    Ok(prepared)
}

/// Run the serialized commit transaction (SPEC step 4b).
///
/// Its first statement claims `entity_write_order`, replacing the former family locks.
async fn commit_prepared(
    db: &DBProvider<WorkerError>,
    stores: &Arc<dyn Stores>,
    scope: &AccessScope,
    request: CommitRequest<'_>,
) -> Result<Result<RevisionCommit, ItemFailure>, WorkerError> {
    let CommitRequest {
        prepared,
        precondition,
        now,
        limits,
        metrics,
    } = request;
    // Short READ COMMITTED recheck/write transaction; `Arc` avoids cloning artifacts.
    // Retry lock contention: both paths re-read and rollback leaves nothing to undo.
    // Otherwise a CAS deadlock escapes `process_item` before `mark_completed`,
    // leaving the operation `running` and items `pending` until recovery.
    //
    // The stored precondition selects the commit, not candidate shape/declared kind.
    // Revisions bypass acceptance policy (SPEC §8.1 step 3), so their commit must
    // refuse absent identifiers to prevent creation through that bypass.
    let tx_scope = scope.clone();
    let tx_stores = Arc::clone(stores);
    // The `'static` retry closure owns each attempt's handles.
    let tx_metrics = Arc::clone(metrics);
    // Copy limits into the `'static` retry closure.
    let tx_limits = limits;
    db.db()
        .transaction_with_retry(commit_write(&db.db()), retryable_db_err, |tx| {
            let prepared = prepared.clone();
            let tx_scope = tx_scope.clone();
            let tx_stores = Arc::clone(&tx_stores);
            let tx_metrics = Arc::clone(&tx_metrics);
            Box::pin(async move {
                commit_prepared_in(
                    tx_stores.as_ref(),
                    tx,
                    &tx_scope,
                    CommitRequest {
                        prepared: &prepared,
                        precondition,
                        now,
                        limits: tx_limits,
                        metrics: &tx_metrics,
                    },
                )
                .await
            })
        })
        .await
}

/// Commit deletion or record refusal. Retry contention safely: all decisions
/// re-read within the transaction and rollback undoes the attempt. No external
/// evaluation means no revalidation loop.
async fn process_deletion(
    stores: &Arc<dyn Stores>,
    db: &DBProvider<WorkerError>,
    scope: &AccessScope,
    tuning: Tuning<'_>,
    operation_id: Uuid,
    item: &OperationItemRow,
    now: OffsetDateTime,
) -> Result<ItemOutcome, WorkerError> {
    let Precondition::Version(expected) = item.precondition else {
        // Acceptance refuses an absent version for a deletion, so a stored item
        // in this shape disagrees with the rules that admitted it.
        return record_failure(
            stores,
            db,
            scope,
            operation_id,
            item,
            ItemFailure::new(
                AdmissionFailureReason::PreconditionFailed,
                format!(
                    "stored deletion item {} carries no expected_resource_version",
                    item.id
                ),
            ),
            now,
            tuning.metrics,
        )
        .await;
    };

    let tx_scope = scope.clone();
    let tx_stores = Arc::clone(stores);
    let tx_limits = *tuning.limits;
    let gts_id = item.gts_id.clone();
    let item_id = item.id;
    let dry_run = item.dry_run;
    let span = Span::current();
    let committed = db
        .db()
        .transaction_with_retry(commit_write(&db.db()), retryable_db_err, |tx| {
            let tx_scope = tx_scope.clone();
            let tx_stores = Arc::clone(&tx_stores);
            let gts_id = gts_id.clone();
            let span = span.clone();
            Box::pin(async move {
                let committed = deletion::commit_deletion(
                    tx_stores.as_ref(),
                    tx,
                    &tx_scope,
                    &gts_id,
                    expected,
                    &tx_limits,
                    &span,
                    now,
                )
                .await?;
                match committed {
                    Ok(commit) => {
                        // Tombstone and item outcome must commit together: if the item
                        // CAS loses (another pass already terminalized it), rolling back
                        // the whole transaction ensures the tombstone does not outlive
                        // the outcome that should accompany it.
                        let outcome = commit.item_outcome(dry_run);
                        let marked = tx_stores
                            .mark_item_succeeded(tx, &tx_scope, item_id, outcome, now)
                            .await?;
                        if marked {
                            Ok(Ok(commit))
                        } else {
                            Err(WorkerError::ItemAlreadyTerminal { item_id })
                        }
                    }
                    Err(failure) => Ok(Err(failure)),
                }
            })
        })
        .await;

    let committed = match committed {
        Ok(committed) => committed,
        // Another pass terminalized the item; this pass rolled back (including tombstone).
        Err(WorkerError::ItemAlreadyTerminal { item_id }) => {
            return stored_item(stores, db, scope, operation_id, item_id).await;
        }
        Err(error) => return Err(error),
    };

    match committed {
        Ok(commit) => {
            tracing::info!(
                %operation_id,
                operation_item_id = item.id,
                gts_id = %item.gts_id,
                resource_version = commit.resource_version,
                "types_registry entity deleted"
            );
            tuning
                .metrics
                .candidate_terminalized(TerminalStatus::Succeeded, item.pass_labels());
            // A deletion allocates no revision (ADR-0005); the version is the
            // one the tombstone now carries.
            let (revision_no, resource_version) = commit.item_outcome(item.dry_run).columns();
            Ok(ItemOutcome {
                gts_id: item.gts_id.clone(),
                status: OperationItemStatus::Succeeded,
                gts_uuid: Some(commit.gts_uuid),
                resource_version,
                revision_no,
                failure: None,
            })
        }
        Err(failure) => {
            record_failure(
                stores,
                db,
                scope,
                operation_id,
                item,
                failure,
                now,
                tuning.metrics,
            )
            .await
        }
    }
}

/// Evaluate and commit one non-terminal item.
/// Revision-vector drift triggers a fresh evaluation up to the configured attempt limit.
async fn process_item(
    stores: &Arc<dyn Stores>,
    db: &DBProvider<WorkerError>,
    scope: &AccessScope,
    tuning: Tuning<'_>,
    operation_id: Uuid,
    item: &OperationItemRow,
    now: OffsetDateTime,
) -> Result<ItemOutcome, WorkerError> {
    if item.status != OperationItemStatus::Pending && item.status != OperationItemStatus::Running {
        return stored_outcome(item);
    }

    // Deletion only commits: no document/store build, compatibility check,
    // revision vector or revalidation loop.
    if item.kind == OperationKind::Deletion {
        return process_deletion(stores, db, scope, tuning, operation_id, item, now).await;
    }

    let payload = item
        .request_payload
        .as_deref()
        .ok_or(WorkerError::MissingPayload { item_id: item.id })?;

    let attempts = tuning.worker.max_revalidation_attempts;
    let mut last_drift: Option<VectorDrift> = None;
    // Probe once in the initial evaluation snapshot. A miss stays on ordinary
    // evaluation even if a concurrent write makes the authored content identical.
    let mut initial = if attempts > 0 {
        Some(prepare(stores, db, scope, item, payload, tuning).await?)
    } else {
        None
    };
    // Log attempts using one-based numbering.
    for attempt in 1..=attempts {
        // Step 3: evaluation releases its snapshot before CPU-heavy validation.
        let prepared = match initial.take() {
            Some(prepared) => prepared,
            None => {
                evaluate(
                    stores,
                    db,
                    scope,
                    EvaluationTarget {
                        gts_id: &item.gts_id,
                        canonical_body: payload,
                        operation_item_id: item.id,
                        precondition: item.precondition,
                        force: effective_force(item.compat_forced, &tuning),
                        labels: item.pass_labels(),
                    },
                    tuning.limits,
                    tuning.metrics,
                    None,
                )
                .await?
            }
        };
        let prepared = match prepared {
            Ok(prepared) => prepared,
            Err(failure) => {
                return record_failure(
                    stores,
                    db,
                    scope,
                    operation_id,
                    item,
                    failure,
                    now,
                    tuning.metrics,
                )
                .await;
            }
        };

        let committed = match commit_prepared(
            db,
            stores,
            scope,
            CommitRequest {
                prepared: &prepared,
                precondition: item.precondition,
                now,
                limits: *tuning.limits,
                metrics: tuning.metrics,
            },
        )
        .await
        {
            Ok(committed) => committed,
            // Another pass terminalized the item; this pass rolled back.
            Err(WorkerError::ItemAlreadyTerminal { item_id }) => {
                return stored_item(stores, db, scope, operation_id, item_id).await;
            }
            // Terminalize a post-write refusal after its transaction rolls back.
            Err(WorkerError::RefusedAfterWrite(failure)) => {
                return record_failure(
                    stores,
                    db,
                    scope,
                    operation_id,
                    item,
                    failure,
                    now,
                    tuning.metrics,
                )
                .await;
            }
            // Guard or artifact CAS drift rolls the transaction back.
            Err(WorkerError::RevalidationRequired(drift)) => {
                tuning.metrics.revalidation_retried(&drift);
                tracing::info!(
                    %operation_id,
                    operation_item_id = item.id,
                    gts_id = %item.gts_id,
                    attempt,
                    max_attempts = attempts,
                    drift = %drift,
                    "types_registry revalidating a candidate whose evaluation went stale"
                );
                last_drift = Some(drift);
                continue;
            }
            Err(error) => return Err(error),
        };

        return match committed {
            Ok(commit) => Ok(committed_outcome(
                operation_id,
                item,
                commit,
                attempt,
                tuning.metrics,
            )),
            Err(failure) => {
                record_failure(
                    stores,
                    db,
                    scope,
                    operation_id,
                    item,
                    failure,
                    now,
                    tuning.metrics,
                )
                .await
            }
        };
    }

    // Every attempt drifted.
    let drift = last_drift.map_or_else(
        || "no attempt was made".to_owned(),
        |drift| drift.to_string(),
    );
    let failure = ItemFailure::new(
        AdmissionFailureReason::RevalidationExhausted,
        format!(
            "the state this candidate was validated against kept moving: {attempts} \
             revalidation attempts were exhausted, the last on {drift}"
        ),
    );
    record_failure(
        stores,
        db,
        scope,
        operation_id,
        item,
        failure,
        now,
        tuning.metrics,
    )
    .await
}

/// Report, log, and count a successful commit.
fn committed_outcome(
    operation_id: Uuid,
    item: &OperationItemRow,
    commit: RevisionCommit,
    attempt: u32,
    metrics: &Arc<dyn AdmissionMetrics>,
) -> ItemOutcome {
    match commit {
        RevisionCommit::Admitted(CommittedUnit {
            gts_uuid,
            revision_no,
            resource_version,
        }) => {
            tracing::info!(
                %operation_id,
                operation_item_id = item.id,
                gts_id = %item.gts_id,
                revision_no,
                resource_version,
                attempt,
                "types_registry candidate admitted"
            );
            metrics.candidate_terminalized(TerminalStatus::Succeeded, item.pass_labels());
            ItemOutcome {
                gts_id: item.gts_id.clone(),
                status: OperationItemStatus::Succeeded,
                gts_uuid: Some(gts_uuid),
                // A dry run moved no version and allocated no revision, so
                // naming either would name something that does not exist. The
                // storage CHECK says the same thing about the columns.
                resource_version: (!item.dry_run).then_some(resource_version),
                revision_no: (!item.dry_run).then_some(revision_no),
                failure: None,
            }
        }
        // Terminal and successful, and deliberately not `Succeeded`: no revision
        // number was allocated, so reporting one would name a revision that does
        // not exist (ADR-0005).
        RevisionCommit::Unchanged {
            gts_uuid,
            resource_version,
        } => {
            tracing::info!(
                %operation_id,
                operation_item_id = item.id,
                gts_id = %item.gts_id,
                resource_version,
                attempt,
                "types_registry candidate content already current"
            );
            metrics.candidate_terminalized(TerminalStatus::Unchanged, item.pass_labels());
            ItemOutcome {
                gts_id: item.gts_id.clone(),
                status: OperationItemStatus::Unchanged,
                gts_uuid: Some(gts_uuid),
                resource_version: Some(resource_version),
                revision_no: None,
                failure: None,
            }
        }
    }
}

/// The `reason` label a refusal counts under.
#[must_use]
pub fn reason_label(reason: &AdmissionFailureReason) -> &'static str {
    reason.metric_label()
}

/// Record refusal separately so it survives commit rollback. Status CAS `false`
/// means another pass terminalized the item: re-read and report its stored outcome,
/// which wins even if this duplicate pass computed a different refusal.
#[allow(clippy::too_many_arguments)]
async fn record_failure(
    stores: &Arc<dyn Stores>,
    db: &DBProvider<WorkerError>,
    scope: &AccessScope,
    operation_id: Uuid,
    item: &OperationItemRow,
    failure: ItemFailure,
    now: OffsetDateTime,
    metrics: &Arc<dyn AdmissionMetrics>,
) -> Result<ItemOutcome, WorkerError> {
    let tx_stores = Arc::clone(stores);
    let tx_scope = scope.clone();
    let payload = failure.to_payload();
    let item_id = item.id;
    let recorded = db
        .transaction(move |tx| {
            Box::pin(async move {
                let recorded = tx_stores
                    .mark_item_failed(tx, &tx_scope, item_id, payload, now)
                    .await?;
                Ok(recorded)
            })
        })
        .await?;

    if recorded {
        // Count only the pass that won the item CAS.
        metrics.candidate_terminalized(TerminalStatus::Failed, item.pass_labels());
        metrics.refused(
            RefusalStage::Admission,
            reason_label(&failure.reason),
            item.pass_labels(),
        );
        tracing::warn!(
            %operation_id,
            operation_item_id = item.id,
            gts_id = %item.gts_id,
            reason = %failure.reason,
            "types_registry candidate refused"
        );
        return Ok(ItemOutcome {
            gts_id: item.gts_id.clone(),
            status: OperationItemStatus::Failed,
            gts_uuid: None,
            resource_version: None,
            revision_no: None,
            failure: Some(failure),
        });
    }
    stored_item(stores, db, scope, operation_id, item_id).await
}

/// The outcome a redelivered pass reports: every stored item, nothing written.
fn already_terminal(
    operation_id: Uuid,
    items: &[OperationItemRow],
) -> Result<OperationOutcome, WorkerError> {
    tracing::debug!(
        %operation_id,
        "types_registry operation was already terminal; the redelivered pass reports \
         the stored outcomes"
    );
    Ok(OperationOutcome {
        operation_id,
        already_terminal: true,
        items: items
            .iter()
            .map(stored_outcome)
            .collect::<Result<Vec<_>, _>>()?,
    })
}

/// The outcome the store holds for one item, re-read outside any transaction this
/// pass opened. Reached only when an overlapping pass won a CAS.
async fn stored_item(
    stores: &Arc<dyn Stores>,
    db: &DBProvider<WorkerError>,
    scope: &AccessScope,
    operation_id: Uuid,
    item_id: i64,
) -> Result<ItemOutcome, WorkerError> {
    let (_, fresh) = read_operation(stores, db, scope, operation_id).await?;
    fresh
        .iter()
        .find(|row| row.id == item_id)
        .ok_or(WorkerError::OperationNotFound { operation_id })
        .and_then(stored_outcome)
}

/// Move to `running`, ignoring CAS `false`: it cannot distinguish an active pass
/// from an interrupted one. Before T21's lease/`worker.operation_timeout`, same-key
/// `Idempotency-Key` replay was the only recovery driver. Proceeding permits duplicate
/// evaluation but preserves outcomes: item writes use status CAS and a losing
/// `commit_creation` rolls back with `WorkerError::ItemAlreadyTerminal`.
///
/// TODO(T21): with the outbox and a lease built on `worker.operation_timeout`,
/// honour `false` for an operation whose lease is live and re-take one whose lease
/// has expired — which removes the duplicated work as well.
async fn mark_running(
    stores: &Arc<dyn Stores>,
    db: &DBProvider<WorkerError>,
    scope: &AccessScope,
    operation_id: Uuid,
    now: OffsetDateTime,
) -> Result<bool, WorkerError> {
    let stores = Arc::clone(stores);
    let scope = scope.clone();
    db.transaction(move |tx| {
        Box::pin(async move {
            stores
                .mark_running(tx, &scope, operation_id, now)
                .await
                .map_err(WorkerError::from)
        })
    })
    .await
}

/// Move the operation to `completed`.
async fn mark_completed(
    stores: &Arc<dyn Stores>,
    db: &DBProvider<WorkerError>,
    scope: &AccessScope,
    operation_id: Uuid,
    now: OffsetDateTime,
) -> Result<(), WorkerError> {
    let stores = Arc::clone(stores);
    let scope = scope.clone();
    db.transaction(move |tx| {
        Box::pin(async move {
            stores.mark_completed(tx, &scope, operation_id, now).await?;
            Ok(())
        })
    })
    .await
}

#[cfg(test)]
#[path = "worker_tests.rs"]
mod worker_tests;
