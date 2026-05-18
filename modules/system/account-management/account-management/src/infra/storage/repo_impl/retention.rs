//! Retention scanner + reaper scanner repo methods:
//! `scan_retention_due`, `clear_retention_claim`,
//! `scan_stuck_provisioning`. Both scans do atomic claim-and-go
//! inside a `READ COMMITTED` tx so two replicas cannot pick up the
//! same row inside one `RETENTION_CLAIM_TTL` window. The same
//! `tenants.claimed_by` / `claimed_at` columns back both pipelines —
//! `clear_retention_claim` (despite the name) is the shared release
//! primitive.

use std::time::Duration;

use modkit_db::secure::{
    SecureEntityExt, SecureUpdateExt, TxAccessMode, TxConfig, TxIsolationLevel,
};
use modkit_security::AccessScope;
use sea_orm::sea_query::Expr;
use sea_orm::{ColumnTrait, Condition, EntityTrait, Order, QueryFilter};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::tenant::model::TenantStatus;
use crate::domain::tenant::retention::{TenantProvisioningRow, TenantRetentionRow};
use crate::infra::storage::entity::tenants;

use super::TenantRepoImpl;
use super::helpers::{RETENTION_CLAIM_TTL, map_scope_err};

/// Apply the leaf-first ORDER BY chain used by `scan_retention_due`:
/// `(depth DESC, deleted_at ASC, id ASC)`.
///
/// Pinned via this helper (and the snapshot test below) so a future
/// refactor cannot accidentally re-introduce the starvation regression
/// where `deleted_at ASC` ran first and let an older parent with
/// surviving Deleted children monopolise the `LIMIT` window.
fn apply_retention_leaf_first_order<Q: sea_orm::QueryOrder>(q: Q) -> Q {
    q.order_by(tenants::Column::Depth, Order::Desc)
        .order_by(tenants::Column::DeletedAt, Order::Asc)
        .order_by(tenants::Column::Id, Order::Asc)
}

pub(super) async fn scan_retention_due(
    repo: &TenantRepoImpl,
    scope: &AccessScope,
    now: OffsetDateTime,
    default_retention: Duration,
    limit: usize,
) -> Result<Vec<TenantRetentionRow>, DomainError> {
    // Push the per-row due-check into SQL so the `LIMIT` applies to
    // *due* rows only. The earlier implementation over-fetched
    // `4 × batch` rows ordered by `deleted_at ASC` and applied
    // `is_due` in Rust afterwards — but a backlog of older
    // not-yet-due rows (typical case: NULL `retention_window_secs`
    // → default 90 days) could fill the over-fetch window and
    // starve newer due rows (e.g. soft-deleted with explicit
    // `retention_window_secs = 0`). The reviewer flagged this as
    // an indefinite-delay class of bug; by filtering at the DB the
    // due-set is exact and starvation is impossible.
    //
    // The effective due predicate is
    //   `deleted_at + (retention_window_secs if non-negative else default) seconds <= now`.
    // `deleted_at` doubles as the retention-timer start: it is stamped
    // by `schedule_deletion` together with the `status = Deleted` flip
    // and is unique to soft-deleted rows. The `CASE WHEN >= 0` clamp
    // mirrors the Rust `is_due` fallback (`Some(secs) if secs >= 0 =>
    // secs else default`) byte-for-byte: NULL and negative
    // `retention_window_secs` both fall through to `default_secs`,
    // while a meaningful admin-set `0` (immediate hard-delete on next
    // tick) is preserved. Without the clamp a negative window would
    // compute `deleted_at + negative` and mark the row instantly due —
    // the Rust defense-in-depth check catches it but emits warn-spam
    // every tick. Both supported backends express the comparison
    // without engine-specific INTERVAL arithmetic exposed to Rust. The
    // MySQL backend is unsupported by AM migrations (see
    // `m0001_initial_schema`) so it errors here for symmetry with the
    // migration-set rejection.
    let engine = repo.db.db().db_engine();
    // Fail fast on overflow — clamping to `i64::MAX` would silently
    // make rows almost-never due and mask the misconfiguration.
    // Mirrors `schedule_deletion`'s treatment of retention overflow.
    let default_secs =
        i64::try_from(default_retention.as_secs()).map_err(|_| DomainError::Internal {
            diagnostic: format!(
                "scan_retention_due: default retention {} secs overflows i64; misconfiguration",
                default_retention.as_secs()
            ),
            cause: None,
        })?;
    let due_check = match engine {
        "postgres" => Expr::cust_with_values(
            "deleted_at + make_interval(secs => CASE WHEN retention_window_secs >= 0 THEN retention_window_secs ELSE $1 END) <= $2",
            vec![
                sea_orm::Value::from(default_secs),
                sea_orm::Value::from(now),
            ],
        ),
        "sqlite" => Expr::cust_with_values(
            // SQLite stores TIMESTAMP as TEXT (ISO-8601);
            // `julianday()` returns a numeric so the comparison
            // is monotonic regardless of the textual format
            // SeaORM uses for the bound `now`. Parens around the
            // `CASE` keep the `/ 86400.0` division scoped to the
            // chosen seconds value, not the comparison.
            "julianday(deleted_at) + (CASE WHEN retention_window_secs >= 0 THEN retention_window_secs ELSE $1 END) / 86400.0 <= julianday($2)",
            vec![
                sea_orm::Value::from(default_secs),
                sea_orm::Value::from(now),
            ],
        ),
        other => {
            return Err(DomainError::Internal {
                diagnostic: format!(
                    "scan_retention_due: backend '{other}' is not a supported AM backend"
                ),
                cause: None,
            });
        }
    };

    let cap = u64::try_from(limit).unwrap_or(u64::MAX);
    let worker_id = Uuid::new_v4();
    // Stale-claim cutoff: a row claimed before this instant whose
    // `clear_retention_claim` evidently never landed (else
    // `claimed_by` would be NULL) is up for re-claim by another
    // worker. Computed in Rust so the SQL stays portable across
    // the two supported engines.
    let stale_cutoff = match time::Duration::try_from(RETENTION_CLAIM_TTL) {
        Ok(d) => now - d,
        Err(_) => now,
    };
    let scope = scope.clone();
    let rows = repo
        .db
        .transaction_with_config(
            TxConfig {
                isolation: Some(TxIsolationLevel::ReadCommitted),
                access_mode: Some(TxAccessMode::ReadWrite),
            },
            move |tx| {
                Box::pin(async move {
                    // Claimable iff unclaimed OR the previous claim
                    // is older than `RETENTION_CLAIM_TTL`. The
                    // dedicated `claimed_at` column is the
                    // claim-age marker (see comment on
                    // `RETENTION_CLAIM_TTL`); decoupled from
                    // `updated_at` so any future patch path that
                    // bumps `updated_at` on a `Deleted`-status row
                    // does not inadvertently keep stale claims
                    // alive.
                    let claimable = Condition::any()
                        .add(tenants::Column::ClaimedBy.is_null())
                        .add(tenants::Column::ClaimedAt.lte(stale_cutoff));
                    // `due_check` is also re-asserted in the claim
                    // UPDATE below, so a peer transaction that
                    // extends `retention_window_secs` (or, in a
                    // hypothetical un-delete-then-re-delete flow,
                    // moves `deleted_at` forward) between this SELECT
                    // and the UPDATE cannot leave the row
                    // claim-eligible against the new (later) due
                    // instant. The clone is cheap (FNV-style byte
                    // duplication of the bound parameters); it
                    // avoids running the predicate-construction
                    // ladder twice while keeping both filter sites
                    // semantically identical.
                    // Filter out retention-pipeline-parked rows.
                    // `mark_retention_terminal_failure` stamps
                    // `terminal_failure_at` on rows whose
                    // `hard_delete_batch` step classified the
                    // outcome as non-recoverable (cascade hook
                    // returned `HookError::Terminal` / panicked, or
                    // IdP returned `IdpDeprovisionFailure::Terminal`).
                    // Without this predicate the same broken row
                    // would re-enter the scanner every tick and
                    // re-fail with the same terminal outcome, churning
                    // the IdP / hook stack until an operator
                    // intervenes. Symmetric to
                    // `scan_stuck_provisioning`'s
                    // `terminal_failure_at IS NULL` filter on the
                    // reaper side.
                    let scan_filter = Condition::all()
                        .add(tenants::Column::Status.eq(TenantStatus::Deleted.as_smallint()))
                        .add(tenants::Column::DeletedAt.is_not_null())
                        .add(tenants::Column::TerminalFailureAt.is_null())
                        .add(claimable.clone())
                        .add(due_check.clone());

                    // No `FOR UPDATE SKIP LOCKED` here: the
                    // claim-and-go correctness relies on the
                    // atomic UPDATE below — only one worker can
                    // satisfy the `claimable` predicate for any
                    // given row, the others' UPDATE simply
                    // affects 0 rows. Skipping the lock keeps the
                    // scan portable across the two supported
                    // backends; under high concurrency two workers
                    // may scan overlapping candidate sets and
                    // waste a round-trip on the losing UPDATE,
                    // but no row is double-claimed.
                    // Leaf-first SQL ordering applied via the shared
                    // helper before the secure wrapping so the test
                    // and the impl share the column-sequence source
                    // of truth. See `apply_retention_leaf_first_order`
                    // doc + the snapshot test that pins the sequence
                    // to prevent the starvation regression.
                    let candidates = apply_retention_leaf_first_order(tenants::Entity::find())
                        .secure()
                        .scope_with(&scope)
                        .filter(scan_filter)
                        .limit(cap)
                        .all(tx)
                        .await
                        .map_err(map_scope_err)?;

                    // Defense-in-depth Rust-side `is_due` re-check: the
                    // SQL filter is the source of truth, but a divergence
                    // (timezone normalisation, NULL handling) would
                    // otherwise let a not-yet-due row reach the caller.
                    // Re-checking BEFORE we stamp the claim guarantees
                    // mismatched rows are simply ignored this tick rather
                    // than being marked claimed and held invisible until
                    // RETENTION_CLAIM_TTL expires.
                    let candidate_ids: Vec<Uuid> = candidates
                        .iter()
                        .filter_map(|row| {
                            let deleted_at = row.deleted_at?;
                            let retention = match row.retention_window_secs {
                                Some(secs) if secs >= 0 => {
                                    Duration::from_secs(u64::try_from(secs).unwrap_or(0))
                                }
                                _ => default_retention,
                            };
                            if crate::domain::tenant::retention::is_due(
                                now,
                                deleted_at,
                                retention,
                            ) {
                                Some(row.id)
                            } else {
                                tracing::warn!(
                                    target: "am.tenant_retention",
                                    tenant_id = %row.id,
                                    "row matched SQL due-check but failed Rust is_due; skipping for this tick"
                                );
                                None
                            }
                        })
                        .collect();
                    if candidate_ids.is_empty() {
                        return Ok(Vec::new());
                    }

                    // Stamp `claimed_at` with `now` so the new
                    // claim's age can be aged out by the same TTL
                    // predicate above if `clear_retention_claim`
                    // later fails. `updated_at` is intentionally
                    // not touched: claim acquisition is a
                    // worker-side bookkeeping event, not a tenant
                    // mutation, and conflating the two columns
                    // would couple worker-liveness detection to
                    // any future patch path.
                    //
                    // Two-statement portable pattern (UPDATE then
                    // SELECT-by-claim-marker) instead of
                    // `UPDATE … RETURNING`: the latter is Postgres-
                    // and SQLite-only, but `modkit-db` is meant to
                    // stay backend-agnostic so MySQL deployments
                    // remain viable. We're inside the
                    // `with_serializable_retry` boundary, so the
                    // SELECT observes a snapshot consistent with
                    // the UPDATE we just issued — exactly the rows
                    // whose `claimed_by` is now `worker_id`
                    // restricted to the candidate window.
                    let candidate_ids_for_select = candidate_ids.clone();
                    // Re-assert the row-eligibility predicate inside
                    // the claim UPDATE: the SELECT above ran under
                    // READ COMMITTED, so a peer transaction could
                    // have flipped `status` away from `Deleted` (or
                    // cleared `deleted_at` via un-delete) between
                    // SELECT and UPDATE. The `claimable` predicate
                    // alone fences worker-vs-worker but not row-vs-
                    // state-change; without these the claim can land
                    // on a row that no longer satisfies the retention
                    // predicate, leaving the SELECT-by-marker below
                    // to surface a non-`Deleted` row to the
                    // hard-delete pipeline.
                    tenants::Entity::update_many()
                        .col_expr(tenants::Column::ClaimedBy, Expr::value(worker_id))
                        .col_expr(tenants::Column::ClaimedAt, Expr::value(now))
                        .filter(
                            Condition::all()
                                .add(tenants::Column::Id.is_in(candidate_ids))
                                .add(
                                    tenants::Column::Status
                                        .eq(TenantStatus::Deleted.as_smallint()),
                                )
                                .add(tenants::Column::DeletedAt.is_not_null())
                                // Re-assert the full due predicate here —
                                // not just `Status` / `DeletedAt` — so a
                                // concurrent transaction that extends
                                // `retention_window_secs` (or, in a
                                // hypothetical un-delete-then-re-delete
                                // flow, moves `deleted_at` forward)
                                // between the SELECT and this UPDATE
                                // cannot leave the row claim-eligible
                                // against the new (later) due instant.
                                // Without this, a tenant whose retention
                                // window was just extended could be
                                // hard-deleted on the very next reaper
                                // tick.
                                .add(due_check)
                                // Defense-in-depth re-assert of the
                                // retention-side terminal-failure
                                // exclusion. Under READ COMMITTED a
                                // peer worker that just classified the
                                // row as terminal could stamp
                                // `terminal_failure_at` between our
                                // SELECT and this UPDATE; the marker
                                // wins, this UPDATE affects 0 rows,
                                // SELECT-by-marker returns nothing.
                                // Mirrors the symmetric guard in
                                // `scan_stuck_provisioning`.
                                .add(tenants::Column::TerminalFailureAt.is_null())
                                .add(claimable),
                        )
                        .secure()
                        .scope_with(&scope)
                        .exec(tx)
                        .await
                        .map_err(map_scope_err)?;

                    tenants::Entity::find()
                        .filter(
                            Condition::all()
                                .add(tenants::Column::Id.is_in(candidate_ids_for_select))
                                .add(tenants::Column::ClaimedBy.eq(worker_id))
                                // Defense-in-depth — same posture as
                                // `scan_stuck_provisioning`'s SELECT-
                                // by-marker. Under correct single-tx
                                // execution the prior UPDATE's row-
                                // level lock already prevents a peer
                                // from flipping these columns before
                                // commit, but the extra predicate
                                // keeps the read aligned with the
                                // claim filter if a future refactor
                                // splits the UPDATE / SELECT across
                                // tx boundaries.
                                .add(tenants::Column::TerminalFailureAt.is_null()),
                        )
                        .secure()
                        .scope_with(&scope)
                        .all(tx)
                        .await
                        .map_err(map_scope_err)
                })
            },
        )
        .await?;

    // The Rust `is_due` re-check ran before claim acquisition inside
    // the transaction, so every row here is already verified due.
    let mut out = Vec::with_capacity(rows.len());
    for r in rows {
        let Some(deleted_at) = r.deleted_at else {
            continue;
        };
        let retention = match r.retention_window_secs {
            Some(secs) if secs >= 0 => Duration::from_secs(u64::try_from(secs).unwrap_or(0)),
            _ => default_retention,
        };
        out.push(TenantRetentionRow {
            id: r.id,
            depth: u32::try_from(r.depth).map_err(|_| DomainError::Internal {
                diagnostic: format!(
                    "tenants.depth negative for retention row {}: {}",
                    r.id, r.depth
                ),
                cause: None,
            })?,
            deleted_at,
            retention_window: retention,
            claimed_by: worker_id,
        });
    }
    // The SQL ordering is already leaf-first
    // (`depth DESC, deleted_at ASC, id ASC`); the post-TX re-sort
    // here is defensive against the `is_due` filter changing the
    // surviving subset's order (it wouldn't, since the filter is
    // boolean per-row, but the explicit sort makes the contract
    // local to this function rather than relying on the SELECT).
    out.sort_by(|a, b| {
        b.depth
            .cmp(&a.depth)
            .then_with(|| a.deleted_at.cmp(&b.deleted_at))
            .then_with(|| a.id.cmp(&b.id))
    });
    Ok(out)
}

pub(super) async fn clear_retention_claim(
    repo: &TenantRepoImpl,
    scope: &AccessScope,
    tenant_id: Uuid,
    worker_id: Uuid,
) -> Result<(), DomainError> {
    let conn = repo.db.conn()?;
    // `claimed_by = worker_id` predicate fences this UPDATE to the
    // worker that originally claimed the row. If the TTL elapsed
    // and a peer worker took over, the predicate fails and this
    // call is a no-op — the peer's live claim is preserved.
    // `claimed_at` is cleared together with `claimed_by` so the
    // claim-age column never lingers on an unclaimed row.
    tenants::Entity::update_many()
        .col_expr(
            tenants::Column::ClaimedBy,
            Expr::value(Option::<Uuid>::None),
        )
        .col_expr(
            tenants::Column::ClaimedAt,
            Expr::value(Option::<OffsetDateTime>::None),
        )
        .filter(
            Condition::all()
                .add(tenants::Column::Id.eq(tenant_id))
                .add(tenants::Column::ClaimedBy.eq(worker_id)),
        )
        .secure()
        .scope_with(scope)
        .exec(&conn)
        .await
        .map_err(map_scope_err)?;
    Ok(())
}

pub(super) async fn scan_stuck_provisioning(
    repo: &TenantRepoImpl,
    scope: &AccessScope,
    now: OffsetDateTime,
    older_than: OffsetDateTime,
    limit: usize,
) -> Result<Vec<TenantProvisioningRow>, DomainError> {
    // Atomic claim-and-go pattern symmetric to `scan_retention_due`:
    // two replicas cannot stamp `IdpPluginClient::deprovision_tenant`
    // calls onto the same row inside one `RETENTION_CLAIM_TTL` window.
    // Defense-in-depth on top of the
    // `IdpDeprovisionFailure::NotFound`-as-success-equivalent error
    // mapping — that mapping handles edge-case races (lost claim
    // after crash recovery, TTL-expired peer takeover); the claim
    // here prevents the routine concurrent-replica double-call.
    //
    // Two-statement portable pattern (UPDATE then SELECT-by-marker)
    // instead of `UPDATE … RETURNING`: the latter is Postgres- and
    // SQLite-only, but `modkit-db` is meant to stay backend-agnostic
    // so MySQL deployments remain viable.
    let cap = u64::try_from(limit).unwrap_or(u64::MAX);
    let worker_id = Uuid::new_v4();
    let stale_cutoff = match time::Duration::try_from(RETENTION_CLAIM_TTL) {
        Ok(d) => now - d,
        Err(_) => now,
    };
    let scope = scope.clone();
    let rows = repo
        .db
        .transaction_with_config(
            TxConfig {
                isolation: Some(TxIsolationLevel::ReadCommitted),
                access_mode: Some(TxAccessMode::ReadWrite),
            },
            move |tx| {
                Box::pin(async move {
                    let claimable = Condition::any()
                        .add(tenants::Column::ClaimedBy.is_null())
                        .add(tenants::Column::ClaimedAt.lte(stale_cutoff));
                    // Filter out rows the reaper previously stamped
                    // as terminal-failure (`mark_provisioning_terminal_failure`).
                    // The SDK contract says
                    // `IdpDeprovisionFailure::Terminal` is non-recoverable
                    // and operator-action-required, so re-issuing the
                    // deprovision call on every tick would loop
                    // forever without surfacing any new signal. The
                    // marker is cleared by an operator (manual SQL
                    // UPDATE or hard-delete of the row) once they
                    // resolve the vendor-side issue.
                    let scan_filter = Condition::all()
                        .add(tenants::Column::Status.eq(TenantStatus::Provisioning.as_smallint()))
                        .add(tenants::Column::CreatedAt.lte(older_than))
                        .add(tenants::Column::TerminalFailureAt.is_null())
                        .add(claimable.clone());

                    let candidates = tenants::Entity::find()
                        .secure()
                        .scope_with(&scope)
                        .filter(scan_filter)
                        .order_by(tenants::Column::CreatedAt, Order::Asc)
                        .order_by(tenants::Column::Id, Order::Asc)
                        .limit(cap)
                        .all(tx)
                        .await
                        .map_err(map_scope_err)?;

                    let candidate_ids: Vec<Uuid> = candidates.iter().map(|r| r.id).collect();
                    if candidate_ids.is_empty() {
                        return Ok(Vec::new());
                    }

                    // Two-statement portable claim (UPDATE then
                    // SELECT-by-marker) — same rationale as
                    // `scan_retention_due`: `UPDATE … RETURNING` is
                    // Postgres/SQLite-only, MySQL needs the split.
                    //
                    // Re-assert `Status = Provisioning` inside the
                    // claim UPDATE: under READ COMMITTED a peer
                    // finalizer can flip `Provisioning → Active`
                    // between the SELECT above and this UPDATE, and
                    // `claimable` alone (which fences worker-vs-
                    // worker) does not exclude the now-active row.
                    // Without this re-check the reaper would claim
                    // and then call `deprovision_tenant` on a live
                    // tenant.
                    let candidate_ids_for_select = candidate_ids.clone();
                    tenants::Entity::update_many()
                        .col_expr(tenants::Column::ClaimedBy, Expr::value(worker_id))
                        .col_expr(tenants::Column::ClaimedAt, Expr::value(now))
                        .filter(
                            Condition::all()
                                .add(tenants::Column::Id.is_in(candidate_ids))
                                .add(
                                    tenants::Column::Status
                                        .eq(TenantStatus::Provisioning.as_smallint()),
                                )
                                // Defense-in-depth: re-assert the
                                // terminal-failure exclusion in the
                                // claim UPDATE too. Under READ
                                // COMMITTED a peer reaper that just
                                // finished a different failure mode
                                // could mark the row terminal between
                                // our SELECT and UPDATE; the marker
                                // wins, this UPDATE affects 0 rows,
                                // SELECT-by-marker returns nothing.
                                .add(tenants::Column::TerminalFailureAt.is_null())
                                .add(claimable),
                        )
                        .secure()
                        .scope_with(&scope)
                        .exec(tx)
                        .await
                        .map_err(map_scope_err)?;

                    // Defense-in-depth: re-assert `Provisioning` and
                    // `terminal_failure_at IS NULL` on the SELECT-by-
                    // marker too. The UPDATE above takes a row-level
                    // lock that prevents a concurrent peer from flipping
                    // the row before our tx commits, so under correct
                    // single-tx execution this re-check is redundant —
                    // but if a future refactor splits the UPDATE and
                    // SELECT across tx boundaries, this guard keeps the
                    // reaper from picking up a row that's no longer
                    // reaper-eligible. Cost is one extra predicate on
                    // an already-narrowed read.
                    tenants::Entity::find()
                        .filter(
                            Condition::all()
                                .add(tenants::Column::Id.is_in(candidate_ids_for_select))
                                .add(tenants::Column::ClaimedBy.eq(worker_id))
                                .add(
                                    tenants::Column::Status
                                        .eq(TenantStatus::Provisioning.as_smallint()),
                                )
                                .add(tenants::Column::TerminalFailureAt.is_null()),
                        )
                        .secure()
                        .scope_with(&scope)
                        .all(tx)
                        .await
                        .map_err(map_scope_err)
                })
            },
        )
        .await?;

    Ok(rows
        .into_iter()
        .map(|r| TenantProvisioningRow {
            id: r.id,
            created_at: r.created_at,
            claimed_by: worker_id,
        })
        .collect())
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "retention_tests.rs"]
mod retention_tests;
