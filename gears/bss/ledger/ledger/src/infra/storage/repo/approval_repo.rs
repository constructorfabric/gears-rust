//! `ApprovalRepo` — dual-control approval state (`bss.ledger_approval`) plus the
//! append-only comment thread (`bss.ledger_approval_comment`).
//!
//! The pending-create, the decision transitions (approve / reject /
//! request-changes / cancel / expire), and the resubmit run **inside the caller's
//! transaction** (the approval row locks in the pre-balance slot just before
//! `fiscal_period`, §4.3). State transitions use an **optimistic guard on the
//! expected current state** (mirroring [`DisputeRepo::dispute_advance`]): a
//! transition matched against the wrong state touches 0 rows, so a concurrent
//! decision (e.g. two approvers) leaves exactly one winner — the loser maps the
//! `0` to an invalid transition. Reads (queue + single + thread) run out-of-txn
//! through the PDP-compiled `AccessScope` (SQL-level BOLA — a foreign tenant
//! yields no row). Idempotency (DC13) is the partial-unique index on
//! `(tenant, kind, business_key) WHERE state IN ('PENDING','NEEDS_REWORK')`; the
//! service reads the active record before inserting.

use crate::domain::approval::policy::D2Thresholds;
use crate::infra::posting::retry::is_driver_contention;
use crate::infra::storage::money_text::{decode_money, decode_optional_money, encode_amount};
use bss_ledger_sdk::PostedMoney;
use sea_orm::DbBackend;
use sea_orm::sea_query::Expr;
use sea_orm::{ActiveValue::Set, ColumnTrait, Condition, EntityTrait, Order};
use serde_json::Value as JsonValue;
use toolkit_db::secure::ScopeError;
use toolkit_db::secure::{
    AccessScope, DBRunner, DbTx, SecureEntityExt, SecureInsertExt, SecureUpdateExt,
};
use toolkit_db::{DBProvider, DbError};
use uuid::Uuid;

use crate::domain::approval::ApprovalState;
use crate::domain::approval::policy::{DualControlPolicy, PolicyVersion};
use crate::domain::error::DomainError;
use crate::domain::model::RepoError;
use crate::infra::storage::entity::{
    dual_control_approval as approval, dual_control_comment as comment,
    dual_control_policy as policy, dual_control_policy_threshold as threshold,
};
use time::OffsetDateTime;

/// Static runner seams classify real backend contention before stringification.
fn scope_error(error: ScopeError) -> RepoError {
    if let ScopeError::Db(ref db) = error
        && [DbBackend::Sqlite, DbBackend::Postgres]
            .into_iter()
            .any(|backend| is_driver_contention(backend, db))
    {
        return RepoError::Conflict("approval database contention".into());
    }
    RepoError::Db(error.to_string())
}

/// Validated column money; opaque JSON still requires the R11 intent/snapshot codecs.
#[derive(Clone, Debug)]
pub struct ApprovalRow {
    pub approval_id: Uuid,
    pub tenant_id: Uuid,
    pub kind: String,
    pub state: String,
    pub revision: i32,
    pub business_key: String,
    pub intent: JsonValue,
    pub amount: Option<PostedMoney>,
    pub threshold_snapshot: JsonValue,
    pub reason_code: String,
    pub prepared_by: Uuid,
    pub prepared_at: OffsetDateTime,
    pub approved_by: Option<Uuid>,
    pub decided_at: Option<OffsetDateTime>,
    pub correlation_id: Uuid,
    pub expires_at: OffsetDateTime,
}
impl TryFrom<approval::Model> for ApprovalRow {
    type Error = RepoError;
    fn try_from(r: approval::Model) -> Result<Self, RepoError> {
        if r.revision < 0 {
            return Err(RepoError::InvalidStoredMoney(
                "negative approval revision".into(),
            ));
        }
        let amount =
            decode_optional_money(r.amount.as_deref(), r.currency.as_deref(), r.currency_scale)?;
        Ok(Self {
            approval_id: r.approval_id,
            tenant_id: r.tenant_id,
            kind: r.kind,
            state: r.state,
            revision: r.revision,
            business_key: r.business_key,
            intent: r.intent,
            amount,
            threshold_snapshot: r.threshold_snapshot,
            reason_code: r.reason_code,
            prepared_by: r.prepared_by,
            prepared_at: r.prepared_at,
            approved_by: r.approved_by,
            decided_at: r.decided_at,
            correlation_id: r.correlation_id,
            expires_at: r.expires_at,
        })
    }
}

/// Why [`ApprovalRepo::insert_pending`] did not insert.
#[derive(Debug, thiserror::Error)]
pub enum InsertPendingError {
    /// The DC13 partial-unique index already holds an active approval for
    /// `(tenant, kind, business_key)`: a concurrent preparer won. Deterministic,
    /// so a fresh attempt cannot succeed; the caller recovers by reading the
    /// winner at once instead of retrying.
    #[error("an active approval already exists for the business key")]
    ActiveExists,
    /// Any other repository failure, including retryable contention.
    #[error(transparent)]
    Repo(#[from] RepoError),
}

/// Owned seed for a fresh `PENDING` approval row (preparer step).
#[derive(Clone)]
pub struct NewPendingApproval {
    pub approval_id: Uuid,
    pub tenant: Uuid,
    pub kind: String,
    pub business_key: String,
    pub intent: JsonValue,
    pub amount: Option<PostedMoney>,
    pub threshold_snapshot: JsonValue,
    pub reason_code: String,
    pub prepared_by: Uuid,
    pub prepared_at: OffsetDateTime,
    pub correlation_id: Uuid,
    pub expires_at: OffsetDateTime,
}

/// Owned seed for a fresh effective-dated dual-control policy version (DC8). The
/// `version` is computed by the caller as `max(version) + 1` inside the same
/// serializable txn as the insert. The policy is validated by its type
/// ([`DualControlPolicy::try_new`]); the caller checks each threshold's scale
/// against the currency registry on that same txn before handing it here, while
/// stored reads deliberately never consult the registry.
#[derive(Clone)]
pub struct NewPolicyVersion {
    pub tenant: Uuid,
    pub version: i64,
    pub effective_from: OffsetDateTime,
    pub policy: DualControlPolicy,
    pub created_at_utc: OffsetDateTime,
}

/// SeaORM-backed dual-control approval repository.
#[derive(Clone)]
pub struct ApprovalRepo {
    db: DBProvider<DbError>,
}

impl ApprovalRepo {
    #[must_use]
    pub fn new(db: DBProvider<DbError>) -> Self {
        Self { db }
    }

    // --- In-txn writes (called by the ApprovalService) ---

    /// Insert a fresh `PENDING` row (`revision = 0`). The caller has already
    /// confirmed no active record exists for `(tenant, kind, business_key)`
    /// (DC13); a racing duplicate hits the partial-unique index and surfaces as
    /// [`InsertPendingError::ActiveExists`], never as retryable contention, so the
    /// caller rolls back and returns the winning approval at once.
    ///
    /// # Errors
    /// [`InsertPendingError::ActiveExists`] on the active-row uniqueness violation (a
    /// concurrent prepare of the same business key); [`RepoError::Conflict`] on
    /// classified database contention; [`RepoError::Db`] on any other scope or
    /// storage failure.
    pub async fn insert_pending(
        txn: &DbTx<'_>,
        scope: &AccessScope,
        row: NewPendingApproval,
    ) -> Result<(), InsertPendingError> {
        let am = approval::ActiveModel {
            currency: Set(row.amount.as_ref().map(|v| v.currency().code().to_owned())),
            currency_scale: Set(row.amount.as_ref().map(|v| i16::from(v.currency().scale()))),
            approval_id: Set(row.approval_id),
            tenant_id: Set(row.tenant),
            kind: Set(row.kind),
            state: Set(ApprovalState::Pending.as_str().to_owned()),
            revision: Set(0),
            business_key: Set(row.business_key),
            intent: Set(row.intent),
            amount: Set(row.amount.as_ref().map(encode_amount)),
            threshold_snapshot: Set(row.threshold_snapshot),
            reason_code: Set(row.reason_code),
            prepared_by: Set(row.prepared_by),
            prepared_at: Set(row.prepared_at),
            approved_by: Set(None),
            decided_at: Set(None),
            correlation_id: Set(row.correlation_id),
            expires_at: Set(row.expires_at),
        };
        approval::Entity::insert(am.clone())
            .secure()
            .scope_with_model(scope, &am)
            .map_err(scope_error)?
            .exec_with_returning(txn)
            .await
            .map_err(|e| {
                if e.is_unique_violation() {
                    InsertPendingError::ActiveExists
                } else {
                    scope_error(e).into()
                }
            })?;
        Ok(())
    }

    /// Read the approval row inside the decision transaction (the service checks
    /// `state` + `prepared_by` before executing the stored `intent`).
    ///
    /// # Errors
    /// [`RepoError::Db`] on a scope or storage failure.
    pub async fn read_in_txn(
        txn: &DbTx<'_>,
        scope: &AccessScope,
        tenant: Uuid,
        approval_id: Uuid,
    ) -> Result<Option<ApprovalRow>, RepoError> {
        approval::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(
                Condition::all()
                    .add(approval::Column::TenantId.eq(tenant))
                    .add(approval::Column::ApprovalId.eq(approval_id)),
            )
            .one(txn)
            .await
            .map_err(scope_error)?
            .map(ApprovalRow::try_from)
            .transpose()
    }

    /// Transition the row from `expected_state` to `new_state`, stamping the
    /// decider + decision time. Matched on `(tenant, approval_id, state =
    /// expected_state, revision = expected_revision)` — the in-txn optimistic backstop. Returns the rows
    /// affected: `0` means the row was not in `expected_state` (a concurrent
    /// decision won, or a stale request), which the caller maps to an invalid
    /// transition.
    ///
    /// # Errors
    /// [`RepoError::Db`] on a scope or storage failure.
    #[allow(clippy::too_many_arguments)] // a decision write is intrinsically wide
    pub async fn transition(
        txn: &DbTx<'_>,
        scope: &AccessScope,
        tenant: Uuid,
        approval_id: Uuid,
        expected_state: &str,
        expected_revision: i32,
        new_state: &str,
        decider: Option<Uuid>,
        decided_at: Option<OffsetDateTime>,
    ) -> Result<u64, RepoError> {
        let result = approval::Entity::update_many()
            .secure()
            .scope_with(scope)
            .col_expr(approval::Column::State, Expr::value(new_state))
            .col_expr(approval::Column::ApprovedBy, Expr::value(decider))
            .col_expr(approval::Column::DecidedAt, Expr::value(decided_at))
            .filter(
                Condition::all()
                    .add(approval::Column::TenantId.eq(tenant))
                    .add(approval::Column::ApprovalId.eq(approval_id))
                    .add(approval::Column::State.eq(expected_state))
                    .add(approval::Column::Revision.eq(expected_revision)),
            )
            .exec(txn)
            .await
            .map_err(scope_error)?;
        Ok(result.rows_affected)
    }

    /// Resubmit a `NEEDS_REWORK` row back to `PENDING` with the preparer's edited
    /// intent + re-snapshot threshold, bumping `revision`. Matched on `(tenant,
    /// approval_id, revision, state = NEEDS_REWORK)`. An observed ineligible state
    /// returns zero; a stale revision or lost CAS returns typed Conflict.
    ///
    /// # Errors
    /// [`RepoError::Db`] on a scope or storage failure.
    #[allow(clippy::too_many_arguments)] // a resubmit re-snapshots several columns
    pub async fn resubmit(
        txn: &DbTx<'_>,
        scope: &AccessScope,
        tenant: Uuid,
        approval_id: Uuid,
        new_intent: JsonValue,
        new_threshold_snapshot: JsonValue,
        new_amount: Option<PostedMoney>,
        expected_revision: i32,
    ) -> Result<u64, RepoError> {
        // Read and validate old money before replacement: never repair corruption.
        let Some(current) = Self::read_in_txn(txn, scope, tenant, approval_id).await? else {
            return Ok(0);
        };
        if current.state != ApprovalState::NeedsRework.as_str() {
            return Ok(0);
        }
        if current.revision != expected_revision {
            return Err(RepoError::Conflict("approval resubmit revision".into()));
        }
        let new_revision = expected_revision
            .checked_add(1)
            .filter(|_| expected_revision >= 0)
            .ok_or_else(|| {
                RepoError::InvalidStoredMoney("approval revision overflow/negative".into())
            })?;
        let result = approval::Entity::update_many()
            .secure()
            .scope_with(scope)
            .col_expr(
                approval::Column::State,
                Expr::value(ApprovalState::Pending.as_str()),
            )
            .col_expr(approval::Column::Intent, Expr::value(new_intent))
            .col_expr(
                approval::Column::ThresholdSnapshot,
                Expr::value(new_threshold_snapshot),
            )
            .col_expr(
                approval::Column::Amount,
                Expr::value(new_amount.as_ref().map(encode_amount)),
            )
            .col_expr(
                approval::Column::Currency,
                Expr::value(new_amount.as_ref().map(|v| v.currency().code().to_owned())),
            )
            .col_expr(
                approval::Column::CurrencyScale,
                Expr::value(new_amount.as_ref().map(|v| i16::from(v.currency().scale()))),
            )
            .col_expr(approval::Column::Revision, Expr::value(new_revision))
            .filter(
                Condition::all()
                    .add(approval::Column::TenantId.eq(tenant))
                    .add(approval::Column::ApprovalId.eq(approval_id))
                    .add(approval::Column::Revision.eq(expected_revision))
                    .add(approval::Column::State.eq(ApprovalState::NeedsRework.as_str())),
            )
            .exec(txn)
            .await
            .map_err(scope_error)?;
        if result.rows_affected != 1 {
            return Err(RepoError::Conflict(
                "approval resubmit revision/state".into(),
            ));
        }
        Ok(result.rows_affected)
    }

    /// Batch-expire active rows past `expires_at` (sweep job; also runnable as a
    /// lazy pass). Returns the number expired.
    ///
    /// # Errors
    /// [`RepoError::Db`] on a scope or storage failure.
    pub async fn expire_due(
        txn: &DbTx<'_>,
        scope: &AccessScope,
        tenant: Uuid,
        now: OffsetDateTime,
    ) -> Result<u64, RepoError> {
        let result = approval::Entity::update_many()
            .secure()
            .scope_with(scope)
            .col_expr(
                approval::Column::State,
                Expr::value(ApprovalState::Expired.as_str()),
            )
            .filter(
                Condition::all()
                    .add(approval::Column::TenantId.eq(tenant))
                    .add(
                        approval::Column::State
                            .is_in(ApprovalState::ACTIVE.map(ApprovalState::as_str)),
                    )
                    // `<= now` (not `< now`): a row whose `expires_at` lands exactly
                    // on `now` is reclaimable in this same pass, closing the
                    // one-instant dead zone vs `read_active`'s `ExpiresAt > now`.
                    .add(approval::Column::ExpiresAt.lte(now)),
            )
            .exec(txn)
            .await
            .map_err(scope_error)?;
        Ok(result.rows_affected)
    }

    /// Append a comment to the thread (a free comment/question, or the mandatory
    /// reason on a `reject` / `request-changes` decision). Append-only — there is
    /// no update/delete path.
    ///
    /// # Errors
    /// [`RepoError::Db`] on a scope or storage failure.
    #[allow(clippy::too_many_arguments)] // a flat append row; a struct adds churn
    pub async fn append_comment(
        txn: &DbTx<'_>,
        scope: &AccessScope,
        comment_id: Uuid,
        approval_id: Uuid,
        tenant: Uuid,
        revision: i32,
        author_actor: Uuid,
        body: String,
        created_at: OffsetDateTime,
    ) -> Result<(), RepoError> {
        let am = comment::ActiveModel {
            comment_id: Set(comment_id),
            approval_id: Set(approval_id),
            tenant_id: Set(tenant),
            revision: Set(revision),
            author_actor: Set(author_actor),
            body: Set(body),
            created_at: Set(created_at),
        };
        comment::Entity::insert(am.clone())
            .secure()
            .scope_with_model(scope, &am)
            .map_err(scope_error)?
            .exec_with_returning(txn)
            .await
            .map_err(scope_error)?;
        Ok(())
    }

    /// Cross-tenant TTL sweep (DC12): flip every active (`PENDING`/`NEEDS_REWORK`)
    /// approval whose `expires_at` has passed to `EXPIRED`, across all tenants, in
    /// one statement — the system-context reaper pattern
    /// ([`AccessScope::allow_all`], like the tie-out / aged-alarm sweeps; expiry is
    /// platform maintenance, not a tenant-scoped action). `APPROVING` is excluded
    /// (an in-flight approve is not expirable). Complements the lazy per-tenant
    /// [`Self::expire_due`] pass in `create_pending`; returns the number expired.
    ///
    /// # Errors
    /// [`DomainError::Internal`] on a scope or storage failure.
    pub async fn expire_due_all(&self, now: OffsetDateTime) -> Result<u64, DomainError> {
        let conn = self
            .db
            .conn()
            .map_err(|e| DomainError::Internal(format!("conn: {e}")))?;
        let result = approval::Entity::update_many()
            .secure()
            .scope_with(&AccessScope::allow_all())
            .col_expr(
                approval::Column::State,
                Expr::value(ApprovalState::Expired.as_str()),
            )
            .filter(
                Condition::all()
                    .add(
                        approval::Column::State
                            .is_in(ApprovalState::ACTIVE.map(ApprovalState::as_str)),
                    )
                    .add(approval::Column::ExpiresAt.lte(now)),
            )
            .exec(&conn)
            .await
            .map_err(|e| DomainError::Internal(format!("expire_due_all ledger_approval: {e}")))?;
        Ok(result.rows_affected)
    }

    /// Count `ledger_approval` rows currently in the transient `APPROVING` latch,
    /// across all tenants (Z8-1). A healthy approve clears the latch within one txn
    /// (latch → execute → mark), so a non-zero result observed by the maintenance
    /// sweep is a crash-stranded approve — excluded from the TTL sweep
    /// ([`Self::expire_due_all`]) and still holding the active-uniqueness slot — that
    /// needs a manual re-approve. System-context reaper read
    /// ([`AccessScope::allow_all`], like the TTL sweep).
    ///
    /// # Errors
    /// [`DomainError::Internal`] on a scope or storage failure.
    pub async fn count_approving_all(&self) -> Result<u64, DomainError> {
        let conn = self
            .db
            .conn()
            .map_err(|e| DomainError::Internal(format!("conn: {e}")))?;
        approval::Entity::find()
            .secure()
            .scope_with(&AccessScope::allow_all())
            // Tie the query to the domain enum's wire token (the single source of
            // truth, mirrored by the migration CHECK) rather than a bare literal.
            .filter(
                Condition::all().add(approval::Column::State.eq(ApprovalState::Approving.as_str())),
            )
            .count(&conn)
            .await
            .map_err(|e| DomainError::Internal(format!("count_approving_all ledger_approval: {e}")))
    }

    /// The greatest existing policy `version` for `tenant`, or `None` when the
    /// tenant has no policy row yet — the seed for the next version number
    /// (`max + 1`), read inside the same serializable txn as the insert so two
    /// concurrent writers cannot mint the same version (the PK `(tenant, version)`
    /// is the backstop).
    ///
    /// # Errors
    /// [`RepoError::Db`] on a scope or storage failure.
    pub async fn max_policy_version(
        txn: &DbTx<'_>,
        scope: &AccessScope,
        tenant: Uuid,
    ) -> Result<Option<i64>, RepoError> {
        let row = policy::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(Condition::all().add(policy::Column::TenantId.eq(tenant)))
            .order_by(policy::Column::Version, Order::Desc)
            .one(txn)
            .await
            .map_err(scope_error)?;
        Ok(row.map(|r| r.version))
    }

    /// Insert one effective-dated dual-control policy version (DC8). Append-only —
    /// a new threshold set is a new `(tenant, version)` row, never an update; the
    /// resolver picks the latest `effective_from` (highest `version` on a tie). A
    /// plain write: the policy is valid by its type, the caller has checked every
    /// threshold's scale against the currency registry on this transaction, and the
    /// D2/A6/TTL CHECK ranges are the DB backstop.
    ///
    /// # Errors
    /// [`RepoError::Conflict`] on a `(tenant, version)` PK collision from a concurrent
    /// writer (retryable: the fresh attempt recomputes the version) or classified
    /// database contention; [`RepoError::Db`] on any other scope or storage failure.
    pub async fn insert_policy_row(
        txn: &DbTx<'_>,
        scope: &AccessScope,
        row: NewPolicyVersion,
    ) -> Result<(), RepoError> {
        let am = policy::ActiveModel {
            tenant_id: Set(row.tenant),
            version: Set(row.version),
            effective_from: Set(row.effective_from),

            a6_backdating_biz_days: Set(row.policy.a6_backdating_biz_days()),
            pending_ttl_seconds: Set(row.policy.pending_ttl_seconds()),
            created_at_utc: Set(row.created_at_utc),
        };
        policy::Entity::insert(am.clone())
            .secure()
            .scope_with_model(scope, &am)
            .map_err(scope_error)?
            .exec_with_returning(txn)
            .await
            .map_err(|e| {
                if e.is_unique_violation() {
                    RepoError::Conflict("policy version insertion".into())
                } else {
                    scope_error(e)
                }
            })?;
        for value in row.policy.d2_thresholds().iter() {
            let child = threshold::ActiveModel {
                tenant_id: Set(row.tenant),
                version: Set(row.version),
                currency: Set(value.currency().code().into()),
                currency_scale: Set(i16::from(value.currency().scale())),
                amount: Set(encode_amount(value)),
            };
            threshold::Entity::insert(child.clone())
                .secure()
                .scope_with_model(scope, &child)
                .map_err(scope_error)?
                .exec(txn)
                .await
                .map_err(scope_error)?;
        }
        Ok(())
    }

    // --- Out-of-txn reads (PDP In-scoped; SQL-level BOLA) ---

    /// Read a single approval for `(tenant, approval_id)`, or `None`. A foreign
    /// tenant yields no row.
    ///
    /// # Errors
    /// [`DomainError::Internal`] on a scope or storage failure.
    pub async fn read(
        &self,
        scope: &AccessScope,
        tenant: Uuid,
        approval_id: Uuid,
    ) -> Result<Option<ApprovalRow>, DomainError> {
        let conn = self
            .db
            .conn()
            .map_err(|e| DomainError::Internal(format!("conn: {e}")))?;
        approval::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(
                Condition::all()
                    .add(approval::Column::TenantId.eq(tenant))
                    .add(approval::Column::ApprovalId.eq(approval_id)),
            )
            .one(&conn)
            .await
            .map_err(|e| DomainError::Internal(format!("read ledger_approval: {e}")))?
            .map(ApprovalRow::try_from)
            .transpose()
            .map_err(|e| DomainError::Internal(e.to_string()))
    }

    /// Read all effective-dated dual-control policy versions for a tenant (the
    /// threshold resolver picks the one in effect). Empty ⇒ ratified defaults.
    ///
    /// # Errors
    /// [`DomainError::Internal`] on a scope or storage failure.
    pub async fn read_policy_versions(
        &self,
        scope: &AccessScope,
        tenant: Uuid,
    ) -> Result<Vec<PolicyVersion>, DomainError> {
        let conn = self
            .db
            .conn()
            .map_err(|e| DomainError::Internal(format!("conn: {e}")))?;
        Self::read_policy_versions_in(&conn, scope, tenant)
            .await
            .map_err(|e| DomainError::Internal(e.to_string()))
    }

    /// Assemble parent and ordered children on the caller's snapshot/transaction.
    ///
    /// # Errors
    /// [`RepoError::Db`] on a scope or storage failure; [`RepoError::Conflict`] on classified
    /// database contention; [`RepoError::InvalidStoredMoney`] when a stored threshold amount is
    /// malformed or a stored policy version fails `validate_config`.
    pub async fn read_policy_versions_in<R: DBRunner>(
        runner: &R,
        scope: &AccessScope,
        tenant: Uuid,
    ) -> Result<Vec<PolicyVersion>, RepoError> {
        let rows = policy::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(Condition::all().add(policy::Column::TenantId.eq(tenant)))
            .all(runner)
            .await
            .map_err(scope_error)?;
        // Every version's thresholds in one query, grouped by version in memory:
        // the gate runs this on every refund, note and adjustment.
        let mut children_by_version: std::collections::HashMap<i64, Vec<threshold::Model>> =
            std::collections::HashMap::new();
        for child in threshold::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(Condition::all().add(threshold::Column::TenantId.eq(tenant)))
            .order_by(threshold::Column::Version, Order::Asc)
            .order_by(threshold::Column::Currency, Order::Asc)
            .all(runner)
            .await
            .map_err(scope_error)?
        {
            children_by_version
                .entry(child.version)
                .or_default()
                .push(child);
        }
        let mut result = Vec::new();
        for r in rows {
            let children = children_by_version.remove(&r.version).unwrap_or_default();
            let d2_thresholds = children
                .into_iter()
                .map(|c| decode_money(&c.amount, &c.currency, c.currency_scale))
                .collect::<Result<Vec<_>, _>>()?;
            let policy = D2Thresholds::try_new(d2_thresholds)
                .and_then(|d2| {
                    DualControlPolicy::try_new(d2, r.a6_backdating_biz_days, r.pending_ttl_seconds)
                })
                .map_err(|e| RepoError::InvalidStoredMoney(format!("policy: {e:?}")))?;
            result.push(PolicyVersion {
                effective_from: r.effective_from,
                version: r.version,
                policy,
            });
        }
        Ok(result)
    }

    /// Read the single active (`PENDING`/`NEEDS_REWORK`) approval for
    /// `(tenant, kind, business_key)`, if any — the idempotency lookup (DC13) the
    /// service does before creating a fresh pending record. The partial-unique
    /// index guarantees at most one.
    ///
    /// # Errors
    /// [`DomainError::Internal`] on a scope or storage failure.
    pub async fn read_active(
        &self,
        scope: &AccessScope,
        tenant: Uuid,
        kind: &str,
        business_key: &str,
        now: OffsetDateTime,
    ) -> Result<Option<ApprovalRow>, DomainError> {
        let conn = self
            .db
            .conn()
            .map_err(|e| DomainError::Internal(format!("conn: {e}")))?;
        approval::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(
                Condition::all()
                    .add(approval::Column::TenantId.eq(tenant))
                    .add(approval::Column::Kind.eq(kind))
                    .add(approval::Column::BusinessKey.eq(business_key))
                    // `APPROVING` (the H2 execute latch) is also active — an
                    // idempotent re-prepare while an approve is mid-flight returns
                    // the in-flight record rather than colliding on the partial
                    // unique with no row to surface.
                    .add(approval::Column::State.is_in([
                        ApprovalState::Pending.as_str(),
                        ApprovalState::NeedsRework.as_str(),
                        ApprovalState::Approving.as_str(),
                    ]))
                    // A lapsed approval (past its TTL) is no longer active: it must
                    // not win the idempotent short-circuit, and a fresh prepare must
                    // be able to replace it (the lazy `expire_due` pass in
                    // `create_pending` flips it to EXPIRED inside the insert txn).
                    .add(approval::Column::ExpiresAt.gt(now)),
            )
            .one(&conn)
            .await
            .map_err(|e| DomainError::Internal(format!("read_active ledger_approval: {e}")))?
            .map(ApprovalRow::try_from)
            .transpose()
            .map_err(|e| DomainError::Internal(e.to_string()))
    }

    /// List the approval queue for a tenant, optionally filtered by `state` and
    /// `kind`. Newest-first by `prepared_at` (sorted in memory).
    ///
    /// # Errors
    /// [`DomainError::Internal`] on a scope or storage failure.
    pub async fn list(
        &self,
        scope: &AccessScope,
        tenant: Uuid,
        state: Option<&str>,
        kind: Option<&str>,
    ) -> Result<Vec<ApprovalRow>, DomainError> {
        let conn = self
            .db
            .conn()
            .map_err(|e| DomainError::Internal(format!("conn: {e}")))?;
        let mut predicate = Condition::all().add(approval::Column::TenantId.eq(tenant));
        if let Some(s) = state {
            predicate = predicate.add(approval::Column::State.eq(s));
        }
        if let Some(k) = kind {
            predicate = predicate.add(approval::Column::Kind.eq(k));
        }
        let mut rows = approval::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(predicate)
            .all(&conn)
            .await
            .map_err(|e| DomainError::Internal(format!("list ledger_approval: {e}")))?;
        rows.sort_by_key(|r| std::cmp::Reverse(r.prepared_at));
        rows.into_iter()
            .map(ApprovalRow::try_from)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| DomainError::Internal(e.to_string()))
    }

    /// Read the full comment thread for an approval, oldest-first.
    ///
    /// # Errors
    /// [`DomainError::Internal`] on a scope or storage failure.
    pub async fn read_thread(
        &self,
        scope: &AccessScope,
        tenant: Uuid,
        approval_id: Uuid,
    ) -> Result<Vec<comment::Model>, DomainError> {
        let conn = self
            .db
            .conn()
            .map_err(|e| DomainError::Internal(format!("conn: {e}")))?;
        let mut rows = comment::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(
                Condition::all()
                    .add(comment::Column::TenantId.eq(tenant))
                    .add(comment::Column::ApprovalId.eq(approval_id)),
            )
            .all(&conn)
            .await
            .map_err(|e| DomainError::Internal(format!("read thread: {e}")))?;
        rows.sort_by_key(|r| r.created_at);
        Ok(rows)
    }
}

#[cfg(test)]
#[path = "approval_repo_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "approval_repo_contract_tests.rs"]
mod contract_tests;
