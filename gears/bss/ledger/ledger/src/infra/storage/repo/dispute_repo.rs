//! `DisputeRepo` — the chargeback dispute current-state table
//! (`bss.ledger_dispute`), keyed by `(tenant_id, dispute_id)`.
//!
//! The **write** (`dispute_upsert` — seed on `opened`; `dispute_advance` —
//! advance `last_phase`/`cycle` on a `won`/`lost`/re-open, Group C) runs inside
//! the passed-in posting transaction (the in-txn sidecar, decision M), mirroring
//! [`PaymentRepo`](super::PaymentRepo)'s `seed_settlement` / `add_allocated`
//! shape: a scoped insert via `.secure().scope_with_model`, a scoped
//! `update_many` via `.secure().scope_with`. The dispute row is **lock rank 0**
//! — taken BEFORE the rank-1 `ledger_payment_settlement` write, keeping the lock
//! order acyclic.
//!
//! The **read** (`read_dispute`) takes the PDP-compiled `AccessScope` and runs
//! out-of-txn through `.secure().scope_with(scope)` (SQL-level BOLA — a foreign
//! tenant yields no row); the service uses it for the pre-read that selects the
//! variant + validates the transition before opening the post transaction.

use sea_orm::sea_query::Expr;
use sea_orm::{ActiveValue::Set, ColumnTrait, Condition, EntityTrait};
use toolkit_db::odata::sea_orm_filter::{LimitCfg, paginate_odata};
use toolkit_db::secure::{
    AccessScope, DbTx, SecureEntityExt, SecureInsertExt, SecureOnConflict, SecureUpdateExt,
};
use toolkit_db::{DBProvider, DbError};
use toolkit_odata::{ODataQuery, Page, SortDir};
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::model::RepoError;
use crate::domain::payment::chargeback::{DisputePhase, DisputeVariant};
use crate::infra::storage::entity::dispute;
use crate::infra::storage::odata_mapping::DisputeODataMapper;
use crate::infra::storage::repo::journal_repo::{
    OdataPageError, map_odata_err, query_with_default_order,
};
use crate::odata::DisputeFilterField;

/// SeaORM-backed dispute current-state repository.
#[derive(Clone)]
pub struct DisputeRepo {
    db: DBProvider<DbError>,
}

impl DisputeRepo {
    #[must_use]
    pub fn new(db: DBProvider<DbError>) -> Self {
        Self { db }
    }

    // --- In-txn writes (called by the chargeback post sidecar) ---

    /// Upsert the `ledger_dispute` row for an `opened` dispute
    /// (`last_phase = OPENED`, the chosen `variant`, `cycle`,
    /// `disputed_amount_minor`, and the `cash_hold_minor` held at open). On a
    /// **fresh** dispute this seeds the row; on a
    /// **re-open** (a new cycle after a prior `won`/`lost`, allowed by the
    /// service's transition guard) the `(tenant, dispute_id)` PK already exists,
    /// so `ON CONFLICT DO UPDATE` advances it to the new cycle's OPENED state
    /// (variant / cycle / disputed re-set, `version + 1`). A same-`(dispute,
    /// cycle, phase)` replay never reaches here — the engine's idempotency gate
    /// short-circuits before the sidecar.
    ///
    /// # Errors
    /// [`RepoError::Db`] on a scope or storage failure.
    #[allow(clippy::too_many_arguments)] // a wide row seed; grouping into a struct adds churn
    pub async fn dispute_upsert(
        txn: &DbTx<'_>,
        scope: &AccessScope,
        tenant: Uuid,
        dispute_id: &str,
        payment_id: &str,
        currency: &str,
        variant: DisputeVariant,
        cycle: i32,
        disputed_amount_minor: i64,
        cash_hold_minor: i64,
    ) -> Result<(), RepoError> {
        let am = dispute::ActiveModel {
            tenant_id: Set(tenant),
            dispute_id: Set(dispute_id.to_owned()),
            payment_id: Set(payment_id.to_owned()),
            currency: Set(currency.to_owned()),
            variant: Set(variant.as_str().to_owned()),
            last_phase: Set(DisputePhase::Opened.as_str().to_owned()),
            cycle: Set(cycle),
            disputed_amount_minor: Set(disputed_amount_minor),
            cash_hold_minor: Set(cash_hold_minor),
            version: Set(0),
        };
        // Re-open (won/lost → opened, new cycle) lands on the existing PK: net the
        // row forward to the new cycle's OPENED state rather than colliding.
        let on_conflict = SecureOnConflict::<dispute::Entity>::columns([
            dispute::Column::TenantId,
            dispute::Column::DisputeId,
        ])
        .value(dispute::Column::Variant, Expr::value(variant.as_str()))
        .and_then(|oc| {
            oc.value(
                dispute::Column::LastPhase,
                Expr::value(DisputePhase::Opened.as_str()),
            )
        })
        .and_then(|oc| oc.value(dispute::Column::Cycle, Expr::value(cycle)))
        .and_then(|oc| {
            oc.value(
                dispute::Column::DisputedAmountMinor,
                Expr::value(disputed_amount_minor),
            )
        })
        .and_then(|oc| oc.value(dispute::Column::CashHoldMinor, Expr::value(cash_hold_minor)))
        .and_then(|oc| {
            oc.value(
                dispute::Column::Version,
                Expr::col((dispute::Entity, dispute::Column::Version)).add(1),
            )
        })
        .map_err(|e| RepoError::Db(format!("ledger_dispute on_conflict: {e}")))?;
        dispute::Entity::insert(am.clone())
            .secure()
            .scope_with_model(scope, &am)
            .map_err(|e| RepoError::Db(format!("ledger_dispute scope: {e}")))?
            .on_conflict(on_conflict)
            .exec_with_returning(txn)
            .await
            .map_err(|e| RepoError::Db(format!("upsert ledger_dispute: {e}")))?;
        Ok(())
    }

    /// Advance an `OPENED` dispute to its `won`/`lost` outcome (`last_phase`,
    /// re-stating `cycle` + `disputed_amount_minor`), bumping `version`. A scoped
    /// UPDATE matched on `(tenant, dispute_id, last_phase = OPENED, cycle)`.
    ///
    /// The `(last_phase = OPENED, cycle)` predicate is the **in-txn race/stale
    /// backstop**, not redundant with the caller's out-of-txn transition guard:
    /// two different outcomes for the same dispute (a `won` and a `lost`, distinct
    /// dedup keys) can both clear that guard, and a `(tenant, dispute_id)`-only
    /// UPDATE would let the second writer overwrite the already-resolved row and
    /// commit a second journal entry. Matching on the OPENED phase + the exact
    /// cycle makes the loser — and any stale-cycle outcome — touch 0 rows and be
    /// rejected as an invalid transition. SSI + retry serialize the writers; this
    /// predicate decides the loser's fate cleanly.
    ///
    /// Re-open (`won`/`lost` → `opened`, a new cycle) does NOT come here — it
    /// lands on [`Self::dispute_upsert`]'s `ON CONFLICT` path. Only the outcome
    /// advance calls this, so the same-cycle match never blocks a legitimate
    /// re-open.
    ///
    /// # Errors
    /// [`RepoError::DisputeNotOpen`] when no `OPENED` row matched the requested
    /// cycle (a concurrent resolve or a stale outcome); [`RepoError::Db`] on any
    /// other scope / storage failure.
    pub async fn dispute_advance(
        txn: &DbTx<'_>,
        scope: &AccessScope,
        tenant: Uuid,
        dispute_id: &str,
        last_phase: DisputePhase,
        cycle: i32,
        disputed_amount_minor: i64,
    ) -> Result<(), RepoError> {
        let result = dispute::Entity::update_many()
            .secure()
            .scope_with(scope)
            .col_expr(dispute::Column::LastPhase, Expr::value(last_phase.as_str()))
            .col_expr(dispute::Column::Cycle, Expr::value(cycle))
            .col_expr(
                dispute::Column::DisputedAmountMinor,
                Expr::value(disputed_amount_minor),
            )
            .col_expr(
                dispute::Column::Version,
                Expr::col((dispute::Entity, dispute::Column::Version)).add(1),
            )
            // Only advance the row STILL `OPENED` at THIS cycle: the in-txn
            // backstop for a concurrent outcome race (a `won` + a `lost` both
            // clearing the out-of-txn guard) and for a stale-cycle outcome — the
            // loser / stale request matches 0 rows instead of overwriting an
            // already-resolved dispute with a second committed entry.
            .filter(
                Condition::all()
                    .add(dispute::Column::TenantId.eq(tenant))
                    .add(dispute::Column::DisputeId.eq(dispute_id))
                    .add(dispute::Column::LastPhase.eq(DisputePhase::Opened.as_str()))
                    .add(dispute::Column::Cycle.eq(cycle)),
            )
            .exec(txn)
            .await
            .map_err(|e| RepoError::Db(format!("advance ledger_dispute: {e}")))?;
        if result.rows_affected == 0 {
            // No `OPENED` row at this cycle: the dispute was concurrently resolved
            // (a `won`/`lost` race) or the outcome targets a stale cycle. A
            // non-retryable invalid transition, not an infra fault.
            return Err(RepoError::DisputeNotOpen(format!(
                "ledger_dispute ({tenant}, {dispute_id}) is not OPENED at cycle {cycle} \
                 — already resolved or stale outcome"
            )));
        }
        Ok(())
    }

    // --- Out-of-txn read (PDP In-scoped; SQL-level BOLA) ---

    /// Read the `ledger_dispute` row for `(tenant, dispute_id)` (the current
    /// variant + cycle + last phase), or `None` when the dispute was never
    /// opened. SQL-level BOLA: a foreign tenant yields no row.
    ///
    /// # Errors
    /// [`DomainError::Internal`] on a scope or storage failure.
    pub async fn read_dispute(
        &self,
        scope: &AccessScope,
        tenant: Uuid,
        dispute_id: &str,
    ) -> Result<Option<dispute::Model>, DomainError> {
        let conn = self
            .db
            .conn()
            .map_err(|e| DomainError::Internal(format!("conn: {e}")))?;
        let row = dispute::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(
                Condition::all()
                    .add(dispute::Column::TenantId.eq(tenant))
                    .add(dispute::Column::DisputeId.eq(dispute_id)),
            )
            .one(&conn)
            .await
            .map_err(|e| DomainError::Internal(format!("read ledger_dispute: {e}")))?;
        Ok(row)
    }

    /// List the `ledger_dispute` current-state rows for `tenant` under `scope`,
    /// cursor-paginated via the canonical `query` (`$filter` over `payment_id` /
    /// `last_phase` / `variant`, `$orderby` / `limit` / `cursor`). The tenant
    /// predicate is pre-applied to the secured select; the user `$filter` is
    /// additive over it (SQL-level BOLA — a foreign value still ANDs the scope, so a
    /// cross-tenant dispute never leaks). A bare list defaults to `dispute_id ASC`.
    /// The `GET /disputes` read-surface source; out-of-txn on a fresh scoped
    /// connection. Mirrors `AdjustmentRepo::list_refunds`.
    ///
    /// # Errors
    /// [`OdataPageError::Db`] on a storage / connection failure;
    /// [`OdataPageError::Odata`] on a malformed `$filter` / `$orderby` / cursor
    /// (the caller projects it to a canonical 400).
    pub async fn list_disputes(
        &self,
        scope: &AccessScope,
        tenant: Uuid,
        query: &ODataQuery,
    ) -> Result<Page<dispute::Model>, OdataPageError> {
        let conn = self
            .db
            .conn()
            .map_err(|e| OdataPageError::Db(format!("conn: {e}")))?;
        // Pre-apply the tenant predicate to the secured select; the user `$filter`
        // is applied additively by `paginate_odata` (it never replaces this scope —
        // BOLA preserved).
        let base_select = dispute::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(Condition::all().add(dispute::Column::TenantId.eq(tenant)));
        let query = query_with_default_order(query, "dispute_id");
        paginate_odata::<
            DisputeFilterField,
            DisputeODataMapper,
            dispute::Entity,
            dispute::Model,
            _,
            _,
        >(
            base_select,
            &conn,
            &query,
            ("dispute_id", SortDir::Asc),
            LimitCfg {
                default: 25,
                max: 200,
            },
            |m| m,
        )
        .await
        .map_err(map_odata_err)
    }

    /// Read the OPEN (non-terminal) dispute on a PAYMENT, if any — the refund
    /// dispute-hold pre-read (Z5-2, design §5). A dispute is OPEN exactly while its
    /// `last_phase == OPENED`; the `won`/`lost` outcomes are terminal (the row stays
    /// at the latest cycle's outcome until a re-open seeds a new `OPENED` cycle). A
    /// refund MUST NOT move cash on a payment with an OPEN dispute (the disputed
    /// funds are sub judice — held in `DISPUTE_HOLD` for `CASH_HOLD`, or reclassed
    /// `DISPUTED` for `AR_RECLASS`), so the handler holds the cash leg until the
    /// dispute resolves.
    ///
    /// Keyed on `(tenant, payment_id, last_phase = OPENED)` (NOT `dispute_id` —
    /// the refund knows only the payment it unwinds). At most one OPEN dispute per
    /// `(tenant, payment_id)` exists at a time: a cycle must resolve (`won`/`lost`)
    /// before the same payment's dispute re-opens (the `opened` transition guard
    /// rejects an `opened` on a still-`OPENED` row), so an `OPENED` row is unique
    /// per payment. Returns the row (its `dispute_id` / `cycle` drive the held
    /// payload + the hold drain's re-read), or `None` when the payment has no open
    /// dispute. SQL-level BOLA: a foreign tenant yields no row.
    ///
    /// # Errors
    /// [`DomainError::Internal`] on a scope or storage failure.
    pub async fn read_open_dispute_for_payment(
        &self,
        scope: &AccessScope,
        tenant: Uuid,
        payment_id: &str,
    ) -> Result<Option<dispute::Model>, DomainError> {
        let conn = self
            .db
            .conn()
            .map_err(|e| DomainError::Internal(format!("conn: {e}")))?;
        let row = dispute::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(
                Condition::all()
                    .add(dispute::Column::TenantId.eq(tenant))
                    .add(dispute::Column::PaymentId.eq(payment_id))
                    .add(dispute::Column::LastPhase.eq(DisputePhase::Opened.as_str())),
            )
            .one(&conn)
            .await
            .map_err(|e| DomainError::Internal(format!("read open dispute for payment: {e}")))?;
        Ok(row)
    }
}
