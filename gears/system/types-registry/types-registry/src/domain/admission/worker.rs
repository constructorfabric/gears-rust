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
//! One acyclic, reference-free candidate per unit, each item its own unit,
//! processed in `item_no` order. Creations and content revisions both land here —
//! the item's stored precondition chooses which commit runs.

use std::sync::Arc;

use time::OffsetDateTime;
use toolkit_db::secure::{AccessScope, ScopeError};
use toolkit_db::{DBProvider, DbError, DbLockGuard, LockConfig};
use toolkit_macros::domain_model;
use uuid::Uuid;

pub use super::errors::{ItemFailure, WorkerError};
use super::unit::{
    CommittedUnit, EvaluatedUnit, RevisionCommit, commit_creation, commit_revision, evaluate,
};
use crate::config::WorkerSettings;
use crate::domain::admission::Precondition;
use crate::domain::enums::{OperationItemStatus, OperationStatus};
use crate::domain::family::{FamilyKey, lock_order};
use crate::domain::ports::{OperationItemRow, OperationRow, Stores, commit_write, snapshot_read};

/// The advisory-lock namespace every types-registry lock is taken under.
pub const LOCK_GEAR: &str = "types_registry";

/// Prefix on the family lock key, so a future lock on some other thing in this
/// gear cannot collide with a family key that happens to spell the same bytes.
const FAMILY_LOCK_PREFIX: &str = "family:";

/// The advisory-lock key one version family is serialized under.
///
/// Public so that a test can probe the key the worker actually takes. A test that
/// spelled the key itself would keep passing after this function changed it.
#[must_use]
pub fn family_lock_key(family_key: &FamilyKey) -> String {
    format!("{FAMILY_LOCK_PREFIX}{family_key}")
}

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
    settings: WorkerSettings,
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
        outcomes.push(process_item(stores, db, scope, operation_id, &item, now, settings).await?);
    }

    mark_completed(stores, db, scope, operation_id, now).await?;

    Ok(OperationOutcome {
        operation_id,
        already_terminal: false,
        items: outcomes,
    })
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

/// Take the advisory lock on each named family, in [`lock_order`]'s order.
///
/// The guards are returned rather than held here because they must outlive the
/// commit transaction: the window they close spans `create_or_get`, the three family
/// rules and the entity insert.
///
/// # Errors
/// [`WorkerError::FamilyLockUnavailable`] when the wait budget expires — contention,
/// not a statement about the candidate, so a redelivery re-drives it. Any guard
/// already taken is explicitly released before the error is returned.
async fn lock_families(
    db: &DBProvider<WorkerError>,
    family_keys: &[FamilyKey],
    operation_id: Uuid,
    operation_item_id: i64,
    max_wait: std::time::Duration,
) -> Result<Vec<DbLockGuard>, WorkerError> {
    let handle = db.db();
    let mut guards = Vec::with_capacity(family_keys.len());
    for key in lock_order(family_keys) {
        let lock_key = family_lock_key(&key);
        let config = LockConfig {
            max_wait: Some(max_wait),
            ..LockConfig::default()
        };
        match handle.try_lock(LOCK_GEAR, &lock_key, config).await {
            Ok(Some(guard)) => guards.push(guard),
            Ok(None) => {
                release_family_locks(guards, operation_id, operation_item_id).await;
                return Err(WorkerError::FamilyLockUnavailable {
                    family_key: key.as_str().to_owned(),
                    retry_after_seconds: max_wait.as_secs().max(1),
                });
            }
            Err(error) => {
                release_family_locks(guards, operation_id, operation_item_id).await;
                return Err(error.into());
            }
        }
    }
    Ok(guards)
}

/// Release every acquired family lock deterministically.
///
/// Used on the normal commit path and on partial acquisition failures. A release
/// error cannot change an already-decided commit or replace the acquisition error;
/// the session still bounds the lock lifetime, so the failure is logged.
async fn release_family_locks(
    guards: Vec<DbLockGuard>,
    operation_id: Uuid,
    operation_item_id: i64,
) {
    for guard in guards {
        if let Err(error) = guard.release().await {
            tracing::warn!(
                %operation_id,
                operation_item_id,
                %error,
                "types_registry could not release a version-family lock; it expires with the \
                 session"
            );
        }
    }
}

/// Groups the per-item commit inputs so the transaction boundary stays readable
/// without crossing Clippy's argument-count threshold.
struct CommitRequest<'a> {
    evaluated: &'a Arc<EvaluatedUnit>,
    item: &'a OperationItemRow,
    operation_id: Uuid,
    now: OffsetDateTime,
    family_lock_timeout: std::time::Duration,
}

/// Steps 4a and 4b: take the family lock a creation needs, run the commit
/// transaction, and release the lock however the commit ended.
///
/// Its own function so the lock's lifetime is a single lexical scope: the guard is
/// taken before the transaction and released after it on every path.
async fn commit_evaluated(
    db: &DBProvider<WorkerError>,
    stores: &Arc<dyn Stores>,
    scope: &AccessScope,
    request: CommitRequest<'_>,
) -> Result<Result<RevisionCommit, ItemFailure>, WorkerError> {
    let CommitRequest {
        evaluated,
        item,
        operation_id,
        now,
        family_lock_timeout,
    } = request;
    // Step 4a: serialize the family rules, for a **creation** only.
    //
    // `create_or_get`'s unique key decides which of two concurrent admissions founds
    // a family, and nothing more: the kind, minor-shape and minor-contiguity rules
    // are check-then-act reads inside a READ COMMITTED transaction, so two admissions
    // of two *different* new members of one family — `…v1~` and `…v1.0~`, or a Type
    // Schema beside an Instance — can each pass and both commit an invariant
    // violation nothing later repairs. The lock is taken here rather than in the
    // repository because it lives on the `Db` handle and must be held **across** the
    // transaction, which a repository inside one cannot do.
    //
    // A revision takes nothing: it adds no member, so it asks none of the rules.
    let precondition = item.precondition;
    let family_lock = match precondition {
        Precondition::MustNotExist => {
            lock_families(
                db,
                std::slice::from_ref(&evaluated.family_key),
                operation_id,
                item.id,
                family_lock_timeout,
            )
            .await?
        }
        Precondition::Version(_) => Vec::new(),
    };

    // Step 4b: a short READ COMMITTED transaction containing only rechecks and
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
    let committed = db
        .db()
        .transaction_with_retry(commit_write(&db.db()), retryable_db_err, |tx| {
            let unit = Arc::clone(evaluated);
            let tx_scope = tx_scope.clone();
            let tx_stores = Arc::clone(&tx_stores);
            Box::pin(async move {
                match precondition {
                    Precondition::MustNotExist => {
                        commit_creation(tx_stores.as_ref(), tx, &tx_scope, unit.as_ref(), now)
                            .await
                            .map(|r| r.map(RevisionCommit::Admitted))
                    }
                    Precondition::Version(expected) => {
                        commit_revision(
                            tx_stores.as_ref(),
                            tx,
                            &tx_scope,
                            unit.as_ref(),
                            expected,
                            now,
                        )
                        .await
                    }
                }
            })
        })
        .await;

    // Released deterministically rather than on `Drop`, which the toolkit documents
    // as best-effort: a sibling admission is waiting on this key. A failed release is
    // logged, not propagated — the commit above is already decided.
    release_family_locks(family_lock, operation_id, item.id).await;

    committed
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
    settings: WorkerSettings,
) -> Result<ItemOutcome, WorkerError> {
    if item.status != OperationItemStatus::Pending && item.status != OperationItemStatus::Running {
        return Ok(stored_outcome(item));
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

    let committed = commit_evaluated(
        db,
        stores,
        scope,
        CommitRequest {
            evaluated: &evaluated,
            item,
            operation_id,
            now,
            family_lock_timeout: settings.family_lock_timeout,
        },
    )
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
        Ok(RevisionCommit::Admitted(CommittedUnit {
            gts_uuid,
            revision_no,
            resource_version,
        })) => {
            tracing::info!(
                %operation_id,
                operation_item_id = item.id,
                gts_id = %item.gts_id,
                revision_no,
                resource_version,
                "types_registry candidate admitted"
            );
            Ok(ItemOutcome {
                gts_id: item.gts_id.clone(),
                status: OperationItemStatus::Succeeded,
                gts_uuid: Some(gts_uuid),
                resource_version: Some(resource_version),
                revision_no: Some(revision_no),
                failure: None,
            })
        }
        // Terminal and successful, and deliberately not `Succeeded`: no revision
        // number was allocated, so reporting one would name a revision that does
        // not exist (ADR-0005).
        Ok(RevisionCommit::Unchanged {
            gts_uuid,
            resource_version,
        }) => {
            tracing::info!(
                %operation_id,
                operation_item_id = item.id,
                gts_id = %item.gts_id,
                resource_version,
                "types_registry candidate content already current"
            );
            Ok(ItemOutcome {
                gts_id: item.gts_id.clone(),
                status: OperationItemStatus::Unchanged,
                gts_uuid: Some(gts_uuid),
                resource_version: Some(resource_version),
                revision_no: None,
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
