//! Admission worker entry point: [`run_operation`] (SPEC §8.1).
//!
//! Returns directly for deterministic tests; T21's outbox handler maps the result
//! to `Ok` / `Retry` / `Reject`. Infrastructure faults return [`WorkerError`];
//! candidate refusals are terminal [`ItemFailure`] outcomes.
//!
//! Process items in dependency order. Failed dependencies block their downstream;
//! independent candidates proceed. Stored preconditions select creation or revision.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use time::OffsetDateTime;
use toolkit_db::secure::{AccessScope, ScopeError};
use toolkit_db::{DBProvider, DbError};
use toolkit_macros::domain_model;
use tracing::{Instrument, Span};
use uuid::Uuid;

use super::batch::{self, order_deletions};
use super::deletion;
pub use super::errors::{ItemFailure, WorkerError};
use super::graph::BatchOrder;
use super::publish;
use super::revision::{CommittedUnit, RevisionCommit};
use super::simulate::{self, Predicted};
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

/// What one pass over an operation produced.
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OperationOutcome {
    pub operation_id: Uuid,
    /// `true` when this pass found the operation already terminal and did nothing.
    /// A redelivered outbox message lands here.
    pub already_terminal: bool,
    pub items: Vec<ItemOutcome>,
}

/// One candidate's outcome.
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ItemOutcome {
    pub gts_id: String,
    pub status: OperationItemStatus,
    /// The Registry Reference of the admitted entity, on success.
    pub gts_uuid: Option<Uuid>,
    pub resource_version: Option<i64>,
    pub revision_no: Option<i32>,
    pub failure: Option<ItemFailure>,
}

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
    // Step 1: the operation and its items under one snapshot. `mark_running` below
    // touches only the operation row, so reading the items before it rather than
    // after changes nothing — and it makes the pair consistent, which two
    // separately-snapshotted reads would not be.
    let (operation, items) = read_operation(stores, db, scope, operation_id).await?;
    // Shared rather than copied from here on: both passes hand the slice to a
    // `'static` transaction closure, and every row carries its authored document.
    let items: Arc<[OperationItemRow]> = items.into();
    observability::record_operation_facts(&Span::current(), operation.kind, operation.dry_run);

    // A redelivered message finds the operation terminal and reports the stored
    // outcomes. Delivery is at-least-once (T21), so this is the shape that makes
    // duplicate delivery a no-op rather than a second admission.
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
        return dry_run_pass(stores, db, scope, tuning, &operation, &items, now).await;
    }

    commit_pass(stores, db, scope, tuning, &operation, &items, now).await
}

/// Order a batch, admit it candidate by candidate and complete the operation.
///
/// Split from [`run_operation_inner`], which now reads the operation, decides
/// which pass it is, and nothing else.
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
    // Steps 1–2: order the batch before touching any candidate. One candidate is
    // one unit, but *which* unit runs next is a property of the whole batch —
    // and the two kinds order by opposite relations. A registration puts what it
    // consumes first; a deletion puts what consumes **it** first, or the target
    // is refused for a dependant the same batch was about to remove.
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

/// Predict one dry-run batch and record what it predicted.
///
/// Simulation uses the shared batch traversal inside one snapshot; publication
/// then records all outcomes and completion atomically (see [`publish`]).
async fn dry_run_pass(
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
        simulate::simulate_batch(stores, db, scope, tuning, operation, Arc::clone(items), now)
            .await?;
    let published =
        publish::publish(stores, db, scope, operation_id, items, &predictions, now).await?;
    // A lost compare-and-swap means an overlapping pass terminalized the item
    // first; its stored outcome stands, as it does on the real path. All of them
    // are in the same post-publication state, so read the items once instead of
    // once per lost item, which was one transaction and one full item scan each.
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

/// The `DbErr` inside a [`WorkerError`], for the transaction retry helper.
///
/// Only the two arms that actually wrap one. Everything else — a store that would
/// not build, an item another pass terminalized — is `None`, which short-circuits
/// the retry loop: those answers do not change on a second attempt.
///
/// The `sea_orm` type in the signature is `Db::transaction_with_retry`'s contract,
/// not a persistence choice this layer is making: the helper classifies contention
/// per backend and needs the driver error to do it.
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
    // A short READ COMMITTED transaction containing only rechecks and
    // writes. The `Arc` keeps transaction retries from cloning the artifacts.
    //
    // Retried on lock contention: every statement in both commit paths re-reads
    // inside the transaction, so an attempt that rolled back leaves nothing to undo.
    // Without the retry, a deadlock on the entity compare-and-swap propagates out of
    // `process_item` before `mark_completed`, stranding the operation row in
    // `running` with its items `pending` and nothing to re-drive it.
    //
    // The item's stored precondition — never the candidate's shape and never a
    // caller-declared kind — chooses the commit. Acceptance skips the policy gate
    // for a revision (SPEC §8.1 step 3), so the claim "this is a revision" has to be
    // *enforced* here, by a commit that refuses an absent identifier.
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

/// Commit one deletion, or record why it could not be.
///
/// Retried on lock contention like every other commit: the transaction re-reads
/// everything it decides on, so an attempt that rolled back leaves nothing to
/// undo. There is no revalidation loop, because there is no evaluation to
/// revalidate — every question the transaction asks is asked inside it.
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
    let span = Span::current();
    let committed = db
        .db()
        .transaction_with_retry(commit_write(&db.db()), retryable_db_err, |tx| {
            let tx_scope = tx_scope.clone();
            let tx_stores = Arc::clone(&tx_stores);
            let gts_id = gts_id.clone();
            let span = span.clone();
            Box::pin(async move {
                deletion::commit_deletion(
                    tx_stores.as_ref(),
                    tx,
                    &tx_scope,
                    &gts_id,
                    expected,
                    &tx_limits,
                    &span,
                    now,
                )
                .await
            })
        })
        .await;

    let committed = match committed {
        Ok(committed) => committed,
        // Another pass terminalized the item; this pass rolled back.
        Err(WorkerError::ItemAlreadyTerminal { item_id }) => {
            return stored_item(stores, db, scope, operation_id, item_id).await;
        }
        Err(error) => return Err(error),
    };

    match committed {
        Ok(commit) => {
            if !terminalize_deletion(stores, db, scope, item, &commit, now).await? {
                return stored_item(stores, db, scope, operation_id, item.id).await;
            }
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

/// Record a committed deletion on its item, in its own statement.
///
/// Separate from the deletion transaction rather than folded into it: the
/// tombstone is already committed, and a `false` here means another pass won the
/// item — whose stored outcome then stands. Registration writes the item inside
/// its transaction because it has a revision to roll back; a deletion has none.
async fn terminalize_deletion(
    stores: &Arc<dyn Stores>,
    db: &DBProvider<WorkerError>,
    scope: &AccessScope,
    item: &OperationItemRow,
    commit: &deletion::DeletionCommit,
    now: OffsetDateTime,
) -> Result<bool, WorkerError> {
    let tx_stores = Arc::clone(stores);
    let tx_scope = scope.clone();
    let item_id = item.id;
    let outcome = commit.item_outcome(item.dry_run);
    db.transaction(move |tx| {
        Box::pin(async move {
            Ok(tx_stores
                .mark_item_succeeded(tx, &tx_scope, item_id, outcome, now)
                .await?)
        })
    })
    .await
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

    // A deletion has no document, so it has nothing to evaluate: no store build,
    // no compatibility check, no revision vector and therefore no revalidation
    // loop. It is a commit transaction and nothing else.
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

/// Record a candidate-level failure and return the outcome to report for it.
///
/// Its own statement rather than part of the commit transaction: the commit rolled
/// back, and the outcome must survive that.
///
/// The write is a CAS on the item's status. `false` means an overlapping pass
/// terminalized the item first — its outcome stands, so the stored row is re-read
/// and reported instead of the failure this pass computed. For a deterministic
/// refusal the two agree; where they do not, the store is right and this pass is
/// the duplicate.
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

/// Read the operation and its items under one snapshot.
///
/// # Errors
/// [`WorkerError::OperationNotFound`] when the id names no row — an unknown
/// operation is an infrastructure fault, not a candidate outcome.
async fn read_operation(
    stores: &Arc<dyn Stores>,
    db: &DBProvider<WorkerError>,
    scope: &AccessScope,
    operation_id: Uuid,
) -> Result<(OperationRow, Vec<OperationItemRow>), WorkerError> {
    let stores_tx = Arc::clone(stores);
    let scope_tx = scope.clone();
    let found = db
        .transaction_with_config(snapshot_read(&db.db()), move |tx| {
            Box::pin(async move {
                let Some(operation) = stores_tx.find_by_id(tx, &scope_tx, operation_id).await?
                else {
                    return Ok(None);
                };
                let items = stores_tx.find_items(tx, &scope_tx, operation_id).await?;
                Ok(Some((operation, items)))
            })
        })
        .await?;
    found.ok_or(WorkerError::OperationNotFound { operation_id })
}

/// Move the operation to `running`.
///
/// The CAS result is deliberately discarded, and that is now a choice rather than a
/// gap. `false` means the operation is already `running` — either another pass owns
/// it, or an earlier pass died mid-flight. P0 has no lease to tell those apart
/// (`worker.operation_timeout` is unread until T21), and treating `false` as
/// "someone else owns it" would strand every operation whose pass died: there is no
/// outbox to redeliver it, so the retry that arrives under the same
/// `Idempotency-Key` is the only driver there is. Proceeding is therefore the
/// recovering behaviour, and overlap is made **safe** instead of prevented: both
/// item writes are CAS on the item's status, and `commit_creation` rolls its
/// transaction back when it loses (`WorkerError::ItemAlreadyTerminal`). The cost is
/// duplicated evaluation work, never a wrong outcome.
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

/// Reconstruct a terminal outcome, deriving `gts_uuid` via `GtsId::to_uuid`.
/// ADR-0012 requires the Registry Reference on success, including replay;
/// refusals carry none.
fn stored_outcome(item: &OperationItemRow) -> Result<ItemOutcome, WorkerError> {
    let terminal_success = matches!(
        item.status,
        OperationItemStatus::Succeeded | OperationItemStatus::Unchanged
    );
    // Not `.ok()`: a terminal success owes its Registry Reference, so an
    // identifier that no longer parses is a corrupt row and has to say so.
    // Swallowing it answered `gts_uuid: None`, which is the one shape ADR-0012
    // rules out, and left no trace of why.
    let gts_uuid = if terminal_success {
        Some(
            gts::GtsId::try_new(&item.gts_id)
                .map_err(|reason| WorkerError::StoredIdentifierUnparsable {
                    item_id: item.id,
                    gts_id: item.gts_id.clone(),
                    reason: reason.to_string(),
                })?
                .to_uuid(),
        )
    } else {
        None
    };
    Ok(ItemOutcome {
        gts_id: item.gts_id.clone(),
        status: item.status,
        gts_uuid,
        resource_version: item.result_resource_version,
        revision_no: item.result_revision_no,
        failure: item.error_payload.as_deref().map(ItemFailure::from_payload),
    })
}

#[cfg(test)]
#[path = "worker_tests.rs"]
mod worker_tests;
