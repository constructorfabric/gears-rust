//! `run_integrity_check` and `repair_derivable_closure_violations`
//! dispatch, plus the lease-free helpers
//! [`run_check_under_lease`] and [`apply_repair_in_tx`] used by
//! [`crate::domain::integrity_check::coordinator::IntegrityCoordinator`].
//!
//! The trait-level entries acquire the AM-local `am_leases` row
//! keyed `"hierarchy_integrity"` for the duration of the work,
//! then release. The check phase runs read-only under REPEATABLE
//! READ; the repair phase runs under SERIALIZABLE retry with
//! fence-in-tx (see
//! [`crate::infra::lease::LeaseGuard::with_ack_in_tx`]). A peer
//! holding the lease surfaces as
//! [`DomainError::IntegrityCheckInProgress`]; a mid-flight steal
//! during repair surfaces as
//! [`DomainError::IntegrityCheckLeaseLost`].
//!
//! The production loop driver wires the coordinator, which unifies
//! check + repair under a single lease. The trait entries are the
//! `TenantRepo` surface kept for ad-hoc callers (integration tests)
//! that need a single-shot check or repair with its own lease.
//!
//! Visibility: `pub(super)` — only the trait `impl` in [`super`]
//! dispatches here.

use sea_orm::DbErr;
use sea_orm::sea_query::Expr;
use sea_orm::{ActiveValue, ColumnTrait, Condition, EntityTrait, QueryFilter};
use toolkit_db::secure::{
    DbTx, ScopeError, SecureDeleteExt, SecureInsertManyExt, SecureOnConflict, SecureUpdateExt,
    TxConfig, TxIsolationLevel,
};
use toolkit_security::AccessScope;

use crate::domain::error::DomainError;
use crate::domain::integrity_check::LEASE_TTL;
use crate::domain::tenant::integrity::{IntegrityCategory, RepairReport, Violation};
use crate::infra::canonical_mapping::classify_db_err_to_domain;
use crate::infra::lease::{AckError, CoordError, LeaseManager};
use crate::infra::storage::entity::tenant_closure;
use crate::infra::storage::integrity;
use crate::infra::storage::integrity::coordinator::HIERARCHY_INTEGRITY_KEY;

use super::TenantRepoImpl;
use super::helpers::{TxError, map_scope_to_tx};

pub(super) async fn run_integrity_check(
    repo: &TenantRepoImpl,
    scope: &AccessScope,
) -> Result<Vec<(IntegrityCategory, Violation)>, DomainError> {
    // Acquire the AM-local lease. `LeaseHeld` maps to the public
    // `IntegrityCheckInProgress` contract via `From<CoordError>`
    // so callers do not see a renamed error code.
    let mgr = LeaseManager::new(repo.provider().db());
    let guard = mgr
        .acquire(HIERARCHY_INTEGRITY_KEY, LEASE_TTL)
        .await
        .map_err(DomainError::from)?;

    // Same REPEATABLE-READ snapshot semantics as before — only the
    // serialization mechanism changed.
    let report_result = run_check_under_lease(repo, scope).await;

    // Always release. On failure, release_with_retry preserves
    // attempts as a forensic streak so a flapping holder is visible
    // to operators.
    log_release_failure(match &report_result {
        Ok(_) => guard.release().await,
        Err(_) => guard.release_with_retry().await,
    });

    report_result
}

/// Log a best-effort lease release that failed at the DB layer.
///
/// By the time release runs the integrity work has already finished, so
/// a release failure does **not** invalidate the outcome — the
/// `am_leases` row frees via its TTL fallback. But a DB-level release
/// error is surfaced here (rather than silently dropped) so the
/// slot-not-freed-promptly condition stays observable. The
/// already-stolen / zero-rows case is logged inside
/// [`crate::infra::lease::LeaseGuard::release`] itself and returns `Ok`.
fn log_release_failure(result: Result<(), CoordError>) {
    if let Err(err) = result {
        tracing::warn!(
            target: "am.integrity",
            error = %err,
            "hierarchy-integrity lease release failed at the DB layer; \
             relying on the TTL fallback to free the slot",
        );
    }
}

/// `repair_derivable_closure_violations` dispatch — runs the
/// pure-Rust [`integrity::repair::compute_repair_plan`] over a
/// snapshot loaded inside a `SERIALIZABLE` transaction with retry
/// AND fence-in-tx, applies the resulting closure-side INSERT /
/// UPDATE / DELETE ops in the same tx.
///
/// The single-flight gate is **shared** with [`run_integrity_check`]
/// — both serialize on the same `am_leases` key. Concurrent
/// check + repair is guaranteed to happen one-at-a-time. Contention
/// surfaces as [`DomainError::IntegrityCheckInProgress`]; a
/// mid-flight steal of the lease during the repair tx surfaces as
/// [`DomainError::IntegrityCheckLeaseLost`].
///
/// `with_ack_in_tx` is built on `transaction_with_retry` so 40001
/// aborts re-plan against a fresh snapshot — the SI cycle detector
/// catches saga races (status flip, hard-delete, `activate_tenant`)
/// before they can leave the repair tx with a stale plan.
pub(super) async fn repair_derivable_closure_violations(
    repo: &TenantRepoImpl,
    scope: &AccessScope,
) -> Result<RepairReport, DomainError> {
    let mgr = LeaseManager::new(repo.provider().db());
    let guard = mgr
        .acquire(HIERARCHY_INTEGRITY_KEY, LEASE_TTL)
        .await
        .map_err(DomainError::from)?;

    let scope_owned = scope.clone();
    let ack = guard
        .with_ack_in_tx::<_, RepairReport, TxError, _>(TxError::db_err, move |tx| {
            let scope = scope_owned.clone();
            Box::pin(async move { apply_repair_in_tx(tx, &scope).await })
        })
        .await;

    match ack {
        Ok(report) => {
            log_release_failure(guard.release().await);
            Ok(report)
        }
        Err(AckError::LeaseLost) => {
            // DO NOT release — TTL handles it; releasing now would
            // steal the slot back from whoever has it.
            Err(DomainError::IntegrityCheckLeaseLost)
        }
        Err(AckError::Work(TxError::Domain(d))) => {
            log_release_failure(guard.release_with_retry().await);
            Err(d)
        }
        Err(AckError::Work(TxError::Db(db_err))) => {
            log_release_failure(guard.release_with_retry().await);
            Err(classify_db_err_to_domain(db_err))
        }
        Err(AckError::Db(db_err)) => {
            log_release_failure(guard.release_with_retry().await);
            Err(DomainError::from(db_err))
        }
    }
}

/// Run the check phase against the caller's already-held lease.
///
/// The coordinator owns the AM-local lease (`am_leases`) for the
/// whole check + maybe-repair cycle, so this helper performs only
/// the REPEATABLE-READ snapshot + classifier work — no acquire,
/// no release. Same return shape as the trait-level
/// [`run_integrity_check`] so the caller can rebucket via the
/// existing `IntegrityReport` mapping without reshaping.
///
/// # Errors
///
/// Any DB error from the snapshot SELECTs is funnelled through
/// the canonical [`From<toolkit_db::DbError> for DomainError`] ladder.
pub async fn run_check_under_lease(
    repo: &TenantRepoImpl,
    scope: &AccessScope,
) -> Result<Vec<(IntegrityCategory, Violation)>, DomainError> {
    let cfg = TxConfig {
        isolation: Some(TxIsolationLevel::RepeatableRead),
        access_mode: None,
    };
    let scope_owned = scope.clone();
    let report = repo
        .db
        .transaction_with_config(cfg, move |tx| {
            Box::pin(async move { integrity::run_integrity_check(tx, &scope_owned).await })
        })
        .await?;
    Ok(report
        .violations_by_category
        .into_iter()
        .flat_map(|(cat, violations)| violations.into_iter().map(move |v| (cat, v)))
        .collect())
}

/// Tx-bound body of [`repair_derivable_closure_violations`] sans
/// lock and sans SERIALIZABLE-retry. Loads a fresh snapshot inside
/// the caller's `tx`, runs the pure-Rust classifier + planner, and
/// applies the closure-side INSERT / UPDATE / DELETE ops.
///
/// Designed to be invoked inside
/// [`crate::infra::lease::LeaseGuard::with_ack_in_tx`] — that
/// helper owns the SERIALIZABLE retry budget AND appends the
/// `am_leases` fence SELECT to the same tx, so a peer steal that
/// invalidates the snapshot is caught at commit time and the whole
/// repair rolls back.
///
/// # Errors
///
/// * `TxError::Db` — raw `DbErr` carried up to the retry helper for
///   contention classification.
/// * `TxError::Domain` — typed domain failure from the snapshot
///   loader or apply pass.
pub async fn apply_repair_in_tx(
    tx: &DbTx<'_>,
    scope: &AccessScope,
) -> Result<RepairReport, TxError> {
    let snapshot = integrity::loader::load_snapshot(tx, scope)
        .await
        .map_err(TxError::Domain)?;
    let report = integrity::run_classifiers(&snapshot);
    let plan = integrity::repair::compute_repair_plan(&snapshot, &report);
    apply_repair_plan(tx, &plan).await?;
    Ok(plan.into_report())
}

/// Apply pass — issue the INSERT / DELETE / UPDATE ops the planner
/// produced. Each pass uses the `SecureORM` bulk extensions so a
/// single statement covers all rows of one shape, keeping the apply
/// window short and SI-conflict surface bounded.
///
/// Ordering: DELETE → UPDATE → INSERT. The planner does not emit
/// overlapping `(a, d)` keys across passes for one snapshot, so the
/// order is operational only — this fixed order keeps future
/// extensions (e.g. an additional UPDATE category) from racing
/// against an INSERT against the same key.
async fn apply_repair_plan(
    tx: &DbTx<'_>,
    plan: &integrity::repair::RepairPlan,
) -> Result<(), TxError> {
    // DELETE stale closure rows in chunks. The OR-of-equalities filter
    // grows linearly in the violation count; chunking caps the per-
    // statement predicate size so a large repair (hundreds of stale
    // rows after a corruption incident) does not produce a multi-KB
    // SQL string that risks falling off the index path or hitting
    // backend statement-length limits. Matches the chunking pattern
    // used by `hard_delete_batch` in the retention path.
    const DELETE_CHUNK_SIZE: usize = 500;
    // Chunk size for the INSERT pass below. Caps the per-statement
    // parameter count at 2k (4 columns × 500 rows) so a corrupted-
    // tree rebuild that emits hundreds of thousands of inserts
    // cannot bump into the Postgres 65k bind-parameter limit and
    // turn a recoverable repair into a hard failure.
    const INSERT_CHUNK_SIZE: usize = 500;

    let allow_all = AccessScope::allow_all();

    if !plan.deletes.is_empty() {
        for chunk in plan.deletes.chunks(DELETE_CHUNK_SIZE) {
            let mut cond = Condition::any();
            for (a, d) in chunk {
                cond = cond.add(
                    Condition::all()
                        .add(tenant_closure::Column::AncestorId.eq(*a))
                        .add(tenant_closure::Column::DescendantId.eq(*d)),
                );
            }
            tenant_closure::Entity::delete_many()
                .filter(cond)
                .secure()
                .scope_with(&allow_all)
                .exec(tx)
                .await
                .map_err(map_scope_to_tx)?;
        }
    }

    // UPDATE barrier per (a, d). Issued one statement per row — the
    // ANSI SQL `CASE` form is dialect-fragile via `sea_query`, and
    // barrier divergences are rare enough in practice that
    // per-row dispatch is cheaper than building a `CASE` expression.
    for upd in &plan.barrier_updates {
        tenant_closure::Entity::update_many()
            .col_expr(
                tenant_closure::Column::Barrier,
                Expr::value(upd.new_barrier),
            )
            .filter(
                Condition::all()
                    .add(tenant_closure::Column::AncestorId.eq(upd.ancestor_id))
                    .add(tenant_closure::Column::DescendantId.eq(upd.descendant_id)),
            )
            .secure()
            .scope_with(&allow_all)
            .exec(tx)
            .await
            .map_err(map_scope_to_tx)?;
    }

    // UPDATE descendant_status — one bulk statement per affected
    // tenant. Every row whose `descendant_id = upd.descendant_id`
    // takes the same target status (closure denormalises
    // `tenants.status` for the descendant), so a single
    // `WHERE descendant_id = X` covers the whole row set.
    for upd in &plan.status_updates {
        tenant_closure::Entity::update_many()
            .col_expr(
                tenant_closure::Column::DescendantStatus,
                Expr::value(upd.new_status.as_smallint()),
            )
            .filter(tenant_closure::Column::DescendantId.eq(upd.descendant_id))
            .secure()
            .scope_with(&allow_all)
            .exec(tx)
            .await
            .map_err(map_scope_to_tx)?;
    }

    // INSERT missing self-rows + strict-ancestor edges in chunks.
    // `tenant_closure` is `no_tenant, no_resource`, so insert_many
    // takes `scope_unchecked` (matches the activation-path insert in
    // `repo_impl/lifecycle.rs::activate_tenant`).
    //
    // ON CONFLICT DO NOTHING on the composite PK
    // `(ancestor_id, descendant_id)`: the repair plan was computed
    // from a snapshot taken at tx start, but a concurrent lifecycle
    // write (e.g. an `activate_tenant` finalising a sibling subtree)
    // can commit the same closure row before this apply pass runs.
    // SERIALIZABLE isolation catches read-set conflicts and triggers
    // the retry helper, but it does not prevent unique-constraint
    // violations on rows committed before this tx began. Making the
    // insert idempotent at the storage layer keeps a benign self-
    // healing race from aborting the whole repair.
    //
    // The Secure `Insert::exec` returns `DbErr::RecordNotInserted`
    // when ON CONFLICT DO NOTHING skips every row in the chunk; we
    // treat that as success because the rows we wanted are already
    // there.
    if !plan.inserts.is_empty() {
        let mut on_conflict = SecureOnConflict::<tenant_closure::Entity>::columns([
            tenant_closure::Column::AncestorId,
            tenant_closure::Column::DescendantId,
        ]);
        on_conflict.inner_mut().do_nothing();

        for chunk in plan.inserts.chunks(INSERT_CHUNK_SIZE) {
            let active_models = chunk.iter().map(|ins| tenant_closure::ActiveModel {
                ancestor_id: ActiveValue::Set(ins.ancestor_id),
                descendant_id: ActiveValue::Set(ins.descendant_id),
                barrier: ActiveValue::Set(ins.barrier),
                descendant_status: ActiveValue::Set(ins.descendant_status.as_smallint()),
            });
            let res = tenant_closure::Entity::insert_many(active_models)
                .secure()
                .scope_unchecked(&allow_all)
                .map_err(map_scope_to_tx)?
                .on_conflict(on_conflict.clone())
                .exec(tx)
                .await;
            match res {
                // `Ok(_)` is the normal apply path; `RecordNotInserted`
                // means the whole chunk no-op'd because a concurrent
                // writer already produced every row — the repair
                // invariant is satisfied either way.
                Ok(_) | Err(ScopeError::Db(DbErr::RecordNotInserted)) => {}
                Err(err) => return Err(map_scope_to_tx(err)),
            }
        }
    }

    Ok(())
}
