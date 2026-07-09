//! `ChainStateRepo` — read and advance the per-tenant tamper-evidence chain
//! tip (`bss.chain_state`). `read_tip` returns the last sealed
//! `(row_hash, entry_id, period_id, seq)` for a tenant, or `None` at genesis
//! (no row yet). `advance` upserts the tip to the new sealed values
//! (`INSERT … ON CONFLICT (tenant_id) DO UPDATE`). Both run inside the
//! caller's posting transaction; tenant isolation runs through the `SecureORM`
//! layer (`.secure().scope_with(scope)` for reads, `.scope_with_model(scope,
//! &am)` for the tip upsert — the validating variant rejects a mismatched
//! `(scope, tenant)` rather than writing cross-tenant).
//!
//! ## Concurrency: no row-level `FOR UPDATE` — SERIALIZABLE is the invariant
//!
//! The tip read is a plain scoped `.one(txn)` — it takes no row lock. The
//! `SecureORM` query builder exposes no `FOR UPDATE`/`lock_exclusive`, and a
//! gear cannot issue a raw locking `SELECT` against the in-flight transaction
//! (the `DBRunner` runner type is sealed and hands out no raw
//! `DatabaseTransaction`). This mirrors the rest of the gear: the
//! [`crate::infra::posting::period::FiscalPeriodGuard`] and the
//! [`crate::infra::posting::projector::BalanceProjector`] both read lockless
//! and rely on the posting's `SERIALIZABLE` transaction for serialization.
//! Two concurrent seals onto the same tenant tip overlap on the `chain_state`
//! row they both read-then-write, so Postgres SSI detects the conflict and
//! aborts the loser (which retries) — and the `tenant_id` primary key makes
//! the `ON CONFLICT` upsert itself atomic regardless.
//!
//! **CONTRACT:** because the read takes no lock,
//! [`ChainStateRepo::read_tip`] + [`ChainStateRepo::advance`] are correct ONLY
//! inside a **SERIALIZABLE** transaction. The `ON CONFLICT` upsert keeps the
//! row internally consistent under any isolation, but a weaker level would let
//! two concurrent seals read the same tip and **fork the chain** (both compute
//! `prev_hash` from the same predecessor). Callers MUST run under
//! `TxConfig::serializable()`. The literal `FOR UPDATE` lock the design names
//! (§4.2/§7) is deferred until `SecureORM` exposes a locking read; swap it in
//! here when it lands and this isolation dependency goes away.

use sea_orm::sea_query::OnConflict;
use sea_orm::{ActiveValue::Set, ColumnTrait, Condition, EntityTrait};
use toolkit_db::DbError;
use toolkit_db::secure::{AccessScope, DbTx, ScopeError, SecureEntityExt, SecureInsertExt};
use uuid::Uuid;

use crate::infra::storage::entity::chain_state;

/// Map a [`ScopeError`] to [`DbError`] **preserving the inner `sea_orm::DbErr`
/// variant** (mirrors `infra::audit::store::scope_to_db` /
/// `period_close::scope_to_db`). This is load-bearing for retry: a
/// statement-time serialization failure (SSI 40001) on the lockless tip
/// read or the tip-advance UPDATE surfaces as `ScopeError::Db(DbErr::Exec |
/// DbErr::Query)`; keeping that variant lets the `transaction_with_retry`
/// contention classifier recognise it and retry the post from a fresh tip.
/// Stringifying it (the old `RepoError::Db(format!(…))`) buried it in a
/// `DbErr::Custom`, which the classifier treats as the NON-retryable business
/// sentinel — so two concurrent posts on one tenant's chain tip surfaced a
/// statement-time abort as a 500 instead of retrying.
fn scope_to_db(e: ScopeError) -> DbError {
    match e {
        ScopeError::Db(db_err) => DbError::Sea(db_err),
        other => DbError::Other(anyhow::anyhow!("chain-state scope: {other}")),
    }
}

/// The per-tenant chain tip: the last sealed row's hash, entry id, period, and
/// sequence. Mirrors the `chain_state` columns minus `tenant_id` (the key).
#[derive(Clone, Debug, PartialEq, Eq)]
#[allow(
    clippy::struct_field_names,
    reason = "fields mirror the chain_state tip columns (last_*)"
)]
pub struct TipRow {
    pub last_row_hash: Vec<u8>,
    pub last_entry_id: Uuid,
    pub last_period_id: String,
    pub last_seq: i64,
}

/// Chain-state repository. Stateless — every method runs inside the caller's
/// posting transaction (`txn`), so it holds no `DBProvider` (mirrors
/// [`crate::infra::posting::idempotency::IdempotencyGate`]).
#[derive(Clone, Default)]
pub struct ChainStateRepo;

impl ChainStateRepo {
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Read the chain tip for `tenant` inside `txn` under `scope`. Returns
    /// `None` when no tip row exists yet (genesis — the next seal is the first
    /// link). The read takes no row lock (see the module docs); the posting's
    /// `SERIALIZABLE` transaction is the concurrency backstop.
    ///
    /// # Errors
    /// [`DbError`] on a storage / scope failure, with the inner `sea_orm::DbErr`
    /// variant preserved (see [`scope_to_db`]) so a serialization abort stays
    /// retryable.
    pub async fn read_tip(
        &self,
        txn: &DbTx<'_>,
        scope: &AccessScope,
        tenant: Uuid,
    ) -> Result<Option<TipRow>, DbError> {
        let row = chain_state::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(Condition::all().add(chain_state::Column::TenantId.eq(tenant)))
            .one(txn)
            .await
            .map_err(scope_to_db)?;

        Ok(row.map(|r| TipRow {
            last_row_hash: r.last_row_hash,
            last_entry_id: r.last_entry_id,
            last_period_id: r.last_period_id,
            last_seq: r.last_seq,
        }))
    }

    /// Advance (upsert) the chain tip for `tenant` to `tip` inside `txn`:
    /// `INSERT … ON CONFLICT (tenant_id) DO UPDATE SET` the four tip columns.
    /// A fresh tenant inserts the first tip; a subsequent seal overwrites it.
    ///
    /// **MUST run inside a `SERIALIZABLE` txn** — the matching [`Self::read_tip`]
    /// is lockless, so SSI is the only thing preventing a forked chain under
    /// concurrent seals (see the module docs).
    ///
    /// # Errors
    /// [`DbError`] on a storage / scope failure, with the inner `sea_orm::DbErr`
    /// variant preserved (see [`scope_to_db`]) so a serialization abort on the
    /// tip-advance UPDATE stays retryable.
    pub async fn advance(
        &self,
        txn: &DbTx<'_>,
        scope: &AccessScope,
        tenant: Uuid,
        tip: &TipRow,
    ) -> Result<(), DbError> {
        let am = chain_state::ActiveModel {
            tenant_id: Set(tenant),
            last_row_hash: Set(tip.last_row_hash.clone()),
            last_entry_id: Set(tip.last_entry_id),
            last_period_id: Set(tip.last_period_id.clone()),
            last_seq: Set(tip.last_seq),
        };
        let on_conflict = OnConflict::column(chain_state::Column::TenantId)
            .update_columns([
                chain_state::Column::LastRowHash,
                chain_state::Column::LastEntryId,
                chain_state::Column::LastPeriodId,
                chain_state::Column::LastSeq,
            ])
            .to_owned();

        chain_state::Entity::insert(am.clone())
            .secure()
            .scope_with_model(scope, &am)
            .map_err(scope_to_db)?
            .on_conflict_raw(on_conflict)
            .exec(txn)
            .await
            .map_err(scope_to_db)?;
        Ok(())
    }
}
