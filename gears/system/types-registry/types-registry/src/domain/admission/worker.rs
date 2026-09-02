//! The admission worker: a plain function of `(operation_id, database)`.
//!
//! **Not a task.** SPEC §8.1 puts it this way because §13's testing rules forbid a
//! test that polls: an entry point that returns a result makes every concurrency
//! case reachable in a plain `#[tokio::test]`. There is no `sleep`, no timer and no
//! channel anywhere in this module. T21's outbox handler is a thin shell that calls
//! [`run_operation`] and maps its return to `Ok` / `Retry` / `Reject`.
//!
//! # The error boundary is the retry boundary
//!
//! [`WorkerError`] is for **infrastructure** failures — a dropped connection, a
//! deadlock, a store that could not be built from committed rows. Those are worth
//! retrying, and the outbox will.
//!
//! A candidate that is simply *wrong* — an unresolvable reference, a schema that
//! fails its meta-schema, an identifier that already exists — is an
//! [`ItemFailure`]: an **outcome** on the operation item, not a fault of the
//! worker. Retrying it would burn the outbox's attempt budget on a decision that is
//! already final. So the two travel in different positions: `Err(WorkerError)`
//! versus `Ok(_)` with a failed item.
//!
//! # P0 scope
//!
//! One acyclic, reference-free Type Schema candidate per unit, each item its own
//! unit, processed in `item_no` order. Dependency-aware ordering and partial
//! admission are T19; compatibility is T17; deletion is T20; Instances are T10.

use std::sync::Arc;

use time::OffsetDateTime;
use toolkit_db::DBProvider;
use toolkit_db::secure::AccessScope;
use toolkit_macros::domain_model;
use uuid::Uuid;

pub use super::errors::{ItemFailure, WorkerError};
use super::unit::{commit_creation, evaluate};
use crate::domain::enums::{OperationItemStatus, OperationStatus};
use crate::domain::ports::{OperationItemRow, OperationRow, Stores, commit_write, snapshot_read};

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
/// Directly callable and returning a result: no `sleep`, no timer, no polling. The
/// transient store is built inside each unit and dropped with it, so nothing is
/// retained between invocations and a second pass re-reads the database.
///
/// # Errors
/// [`WorkerError`] for an infrastructure failure. A candidate-level refusal is
/// recorded on its item and reported in [`OperationOutcome`], not returned here.
pub async fn run_operation(
    stores: &Arc<dyn Stores>,
    db: &DBProvider<WorkerError>,
    scope: &AccessScope,
    operation_id: Uuid,
    now: OffsetDateTime,
) -> Result<OperationOutcome, WorkerError> {
    // Step 1: the operation and its items under one snapshot. `mark_running` below
    // touches only the operation row, so reading the items before it rather than
    // after changes nothing — and it makes the pair consistent, which two
    // separately-snapshotted reads would not be.
    let (operation, items) = read_operation(stores, db, scope, operation_id).await?;

    // A redelivered message finds the operation terminal and reports the stored
    // outcomes. Delivery is at-least-once (T21), so this is the shape that makes
    // duplicate delivery a no-op rather than a second admission.
    if operation.status == OperationStatus::Completed {
        return Ok(OperationOutcome {
            operation_id,
            already_terminal: true,
            items: items.iter().map(stored_outcome).collect(),
        });
    }

    if !mark_running(stores, db, scope, operation_id, now).await? {
        tracing::warn!(
            %operation_id,
            "types_registry operation was already running; continuing with CAS-protected items"
        );
    }

    let mut outcomes = Vec::with_capacity(items.len());
    for item in items {
        outcomes.push(process_item(stores, db, scope, operation_id, &item, now).await?);
    }

    mark_completed(stores, db, scope, operation_id, now).await?;

    Ok(OperationOutcome {
        operation_id,
        already_terminal: false,
        items: outcomes,
    })
}

/// Evaluate and commit one non-terminal operation item, or report the durable
/// outcome an overlapping pass already established.
async fn process_item(
    stores: &Arc<dyn Stores>,
    db: &DBProvider<WorkerError>,
    scope: &AccessScope,
    operation_id: Uuid,
    item: &OperationItemRow,
    now: OffsetDateTime,
) -> Result<ItemOutcome, WorkerError> {
    if item.status != OperationItemStatus::Pending && item.status != OperationItemStatus::Running {
        return Ok(stored_outcome(item));
    }

    // Re-check the closed vocabulary admitted by P0. T11 replaces this refusal
    // with the real precondition enforced inside the commit transaction.
    if let Some(expected_resource_version) = item.precondition.expected_resource_version() {
        let failure = ItemFailure::new(
            "precondition_failed",
            format!(
                "'{}' names expected_resource_version {}; content revisions are not admitted yet",
                item.gts_id, expected_resource_version,
            ),
        );
        return record_failure(stores, db, scope, operation_id, item, failure, now).await;
    }

    let payload = item
        .request_payload
        .as_deref()
        .ok_or(WorkerError::MissingPayload { item_id: item.id })?;

    // Step 3: evaluation releases its snapshot before CPU-heavy validation.
    let evaluated = match evaluate(stores, db, scope, &item.gts_id, payload, item.id).await? {
        Ok(evaluated) => Arc::new(evaluated),
        Err(failure) => {
            return record_failure(stores, db, scope, operation_id, item, failure, now).await;
        }
    };

    // Step 4: a short READ COMMITTED transaction containing only rechecks and
    // writes. The `Arc` keeps transaction retries from cloning the artifacts.
    let tx_scope = scope.clone();
    let tx_stores = Arc::clone(stores);
    let committed = db
        .transaction_with_config(commit_write(&db.db()), |tx| {
            let unit = Arc::clone(&evaluated);
            let tx_scope = tx_scope.clone();
            let tx_stores = Arc::clone(&tx_stores);
            Box::pin(async move {
                commit_creation(tx_stores.as_ref(), tx, &tx_scope, unit.as_ref(), now).await
            })
        })
        .await;

    let committed = match committed {
        Ok(committed) => committed,
        // The competing pass's terminal item is authoritative; this pass rolled
        // its entity write back with `ItemAlreadyTerminal`.
        Err(WorkerError::ItemAlreadyTerminal { item_id }) => {
            return stored_item(stores, db, scope, operation_id, item_id).await;
        }
        Err(error) => return Err(error),
    };

    match committed {
        Ok(committed) => {
            tracing::info!(
                %operation_id,
                operation_item_id = item.id,
                gts_id = %item.gts_id,
                revision_no = committed.revision_no,
                resource_version = committed.resource_version,
                "types_registry candidate admitted"
            );
            Ok(ItemOutcome {
                gts_id: item.gts_id.clone(),
                status: OperationItemStatus::Succeeded,
                gts_uuid: Some(committed.gts_uuid),
                resource_version: Some(committed.resource_version),
                revision_no: Some(committed.revision_no),
                failure: None,
            })
        }
        Err(failure) => record_failure(stores, db, scope, operation_id, item, failure, now).await,
    }
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
async fn record_failure(
    stores: &Arc<dyn Stores>,
    db: &DBProvider<WorkerError>,
    scope: &AccessScope,
    operation_id: Uuid,
    item: &OperationItemRow,
    failure: ItemFailure,
    now: OffsetDateTime,
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
        .map(stored_outcome)
        .ok_or(WorkerError::OperationNotFound { operation_id })
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

/// Read an already-terminal item's stored outcome back out.
fn stored_outcome(item: &OperationItemRow) -> ItemOutcome {
    ItemOutcome {
        gts_id: item.gts_id.clone(),
        status: item.status,
        // `gts_uuid` is not stored on the item — it derives from `gts_id`
        // (`database.sql`), and deriving it here would duplicate a GTS rule. The
        // caller that needs it has the identifier.
        gts_uuid: None,
        resource_version: item.result_resource_version,
        revision_no: item.result_revision_no,
        failure: item.error_payload.as_deref().map(ItemFailure::from_payload),
    }
}
