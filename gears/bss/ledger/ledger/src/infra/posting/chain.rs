//! `ChainSealer` — the in-transaction tamper-evidence chain step. After the
//! journal header + lines are inserted (still NULL-sealed) and the balances
//! are projected, `seal` reads the tenant's chain tip, computes the entry's
//! `row_hash` over the canonical encoding linked to the tip's `prev_hash`,
//! writes the four chain columns onto the freshly inserted (not-yet-sealed)
//! header in one UPDATE, and advances the tip. It runs inside the caller's
//! posting transaction, so the seal commits atomically with the entry (or the
//! whole post rolls back).
//!
//! ## Append-only seal (migration 000007)
//!
//! The relaxed `journal_entry` trigger permits exactly ONE from-NULL UPDATE
//! that sets only `row_hash` / `prev_hash` / `prev_entry_id` /
//! `prev_period_id`; any other column change, a re-seal of an already-sealed
//! row, or a `DELETE` raises an `append-only` exception. This step is the only
//! writer of those columns; the [`crate::infra::storage::repo::JournalRepo`]
//! insert leaves them NULL.
//!
//! ## Concurrency
//!
//! `seal` reads then advances the tip via [`ChainStateRepo`], which takes no
//! row lock (see its module docs). Two concurrent seals onto the same tenant
//! tip overlap on the `chain_state` row they both read-then-write, so the
//! posting's `SERIALIZABLE` transaction makes Postgres SSI abort the loser
//! (which retries from a fresh tip) — yielding a single linear chain.
//!
//! **CONTRACT:** the tip read is lockless, so this
//! step is correct ONLY when it runs inside a **SERIALIZABLE** transaction —
//! SSI is what serializes two concurrent seals. The sole caller is
//! [`crate::infra::posting::service::PostingService::post`], which opens the txn
//! with `TxConfig::serializable()`. A future caller that advances the tip under
//! a weaker isolation level (e.g. `READ COMMITTED`) would let two seals read the
//! same tip and **fork the chain** (both link the same `prev_hash`), which the
//! daily Verifier then reports as a break → tenant-wide freeze. The design's
//! literal `FOR UPDATE` (§4.2/§7) is deferred until `SecureORM` exposes a locking
//! read; until then SERIALIZABLE is the load-bearing invariant. The
//! `concurrent_posts_form_linear_chain` integration test
//! (`tests/postgres_chain.rs`) locks this no-fork behavior in CI.

use sea_orm::sea_query::Expr;
use sea_orm::{ColumnTrait, Condition, EntityTrait};
use toolkit_db::DbError;
use toolkit_db::secure::{AccessScope, DbTx, ScopeError, SecureUpdateExt};

use crate::domain::chain::{chain_row_hash, genesis_prev_hash};
use crate::domain::model::{EntryRef, NewEntry, NewLine};
use crate::infra::posting::service::infra;
use crate::infra::storage::entity::journal_entry;
use crate::infra::storage::repo::{ChainStateRepo, TipRow};

/// Map a [`ScopeError`] to [`DbError`] **preserving the inner `sea_orm::DbErr`
/// variant** (mirrors `infra::audit::store::scope_to_db`). The seal UPDATE runs
/// inside the post's `SERIALIZABLE` txn, so a statement-time serialization
/// abort (SSI 40001) arrives as `ScopeError::Db(DbErr::Exec | DbErr::Query)`;
/// keeping that variant lets the `transaction_with_retry` contention classifier
/// retry the post from a fresh tip rather than burying it in a non-retryable
/// `DbErr::Custom` (the old `infra(format!(…))`) and 500-ing.
fn scope_to_db(e: ScopeError) -> DbError {
    match e {
        ScopeError::Db(db_err) => DbError::Sea(db_err),
        other => DbError::Other(anyhow::anyhow!("chain seal scope: {other}")),
    }
}

/// Seals a posted entry into the per-tenant tamper-evidence hash chain.
/// Stateless — every method runs inside the caller's posting transaction
/// (`txn`), holding only a stateless [`ChainStateRepo`] (mirrors
/// [`crate::infra::posting::idempotency::IdempotencyGate`]).
#[derive(Clone, Default)]
pub struct ChainSealer {
    chain_state: ChainStateRepo,
}

impl ChainSealer {
    #[must_use]
    pub fn new() -> Self {
        Self {
            chain_state: ChainStateRepo::new(),
        }
    }

    /// Seal `entry` (already inserted with NULL chain columns) into the tenant's
    /// chain inside `txn`: read the tip, compute `row_hash` linked to the tip's
    /// `prev_hash` (or [`genesis_prev_hash`] at genesis), write the four chain
    /// columns onto the header, then advance the tip.
    ///
    /// # Errors
    /// [`DbError`] on any storage / scope failure (an infrastructure fault — the
    /// seal carries no business rejection): a tip read/advance failure, a seal
    /// UPDATE that the append-only trigger rejects, or a tip `row_hash` that is
    /// not 32 bytes. The `?`/`Err` rolls the whole post back.
    pub async fn seal(
        &self,
        txn: &DbTx<'_>,
        scope: &AccessScope,
        entry: &NewEntry,
        lines: &[NewLine],
        entry_ref: &EntryRef,
    ) -> Result<(), DbError> {
        let tenant = entry.tenant_id;

        // 1. Read the current tip (None at genesis).
        let tip = self.chain_state.read_tip(txn, scope, tenant).await?;

        // 2. The prev_hash is the tip's row_hash, or the tenant genesis seed.
        let prev_hash: [u8; 32] = match &tip {
            Some(t) => t.last_row_hash[..]
                .try_into()
                .map_err(|_| infra("chain tip row_hash not 32 bytes"))?,
            None => genesis_prev_hash(tenant),
        };

        // 3. Compute this entry's row_hash over the canonical encoding.
        let row_hash = chain_row_hash(entry, lines, &prev_hash);

        // 4. Seal the four chain columns onto the freshly inserted header. The
        // from-NULL, chain-columns-only UPDATE is the single mutation the
        // relaxed append-only trigger permits; the prev pointers are NULL at
        // genesis (no tip) and the tip's id/period otherwise.
        //
        // Hardened: the filter requires `row_hash IS NULL` so the
        // seal can only ever fire on a not-yet-sealed row, and we assert exactly
        // one row matched. In production the append-only trigger (migration
        // 000007) already enforces from-NULL, but the SQLite test backend has no
        // trigger — so without these guards a re-seal (0 rows) would silently
        // advance the tip to a NULL-chained entry → a self-inflicted freeze on
        // the next verify. Now a re-seal / missing row is a hard error here.
        let sealed = journal_entry::Entity::update_many()
            .secure()
            .scope_with(scope)
            .col_expr(
                journal_entry::Column::RowHash,
                Expr::value(Some(row_hash.to_vec())),
            )
            .col_expr(
                journal_entry::Column::PrevHash,
                Expr::value(Some(prev_hash.to_vec())),
            )
            .col_expr(
                journal_entry::Column::PrevEntryId,
                Expr::value(tip.as_ref().map(|t| t.last_entry_id)),
            )
            .col_expr(
                journal_entry::Column::PrevPeriodId,
                Expr::value(tip.as_ref().map(|t| t.last_period_id.clone())),
            )
            .filter(
                Condition::all()
                    .add(journal_entry::Column::TenantId.eq(tenant))
                    .add(journal_entry::Column::PeriodId.eq(entry.period_id.clone()))
                    .add(journal_entry::Column::EntryId.eq(entry.entry_id))
                    .add(journal_entry::Column::RowHash.is_null()),
            )
            .exec(txn)
            .await
            .map_err(scope_to_db)?;
        if sealed.rows_affected != 1 {
            return Err(infra(format!(
                "chain seal affected {} rows (expected exactly 1 from-NULL seal of \
                 entry {} period {}); a 0-row result means the row is missing or \
                 already sealed",
                sealed.rows_affected, entry.entry_id, entry.period_id
            )));
        }

        // 5. Advance the tip to this entry's sealed values.
        self.chain_state
            .advance(
                txn,
                scope,
                tenant,
                &TipRow {
                    last_row_hash: row_hash.to_vec(),
                    last_entry_id: entry.entry_id,
                    last_period_id: entry.period_id.clone(),
                    last_seq: entry_ref.created_seq,
                },
            )
            .await?;

        Ok(())
    }
}
