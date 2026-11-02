//! `IdempotencyGate` — the at-most-once posting gate over
//! `bss.ledger_idempotency_dedup`. `claim` performs an `INSERT … ON CONFLICT
//! (tenant_id, flow, business_id) DO NOTHING` inside the caller's
//! transaction, then reads the row back: a fresh insert yields
//! [`ClaimOutcome::Claimed`], a conflict yields [`ClaimOutcome::Replay`]
//! carrying the stored row (incl. `payload_hash`, so the caller can map a
//! differing hash to `IDEMPOTENCY_PAYLOAD_CONFLICT`). `finalize` stamps the
//! result entry + sequence and flips the status to `POSTED` before COMMIT,
//! so a concurrent replay reads a complete reference.
//!
//! `payload_hash` uses the toolkit-security SHA-256 facade with the platform
//! backend selected for this build.

use sea_orm::sea_query::{Expr, OnConflict};
use sea_orm::{ActiveValue::Set, ColumnTrait, Condition, DbErr, EntityTrait};
use toolkit_db::secure::{
    AccessScope, DbTx, ScopeError, SecureEntityExt, SecureInsertExt, SecureUpdateExt,
};
use toolkit_security::crypto::hash::Sha256;
use uuid::Uuid;

use crate::domain::model::{NewEntry, NewLine, RepoError};
use crate::infra::storage::entity::idempotency_dedup;
use time::OffsetDateTime;

/// Classify actual backend contention before reducing a repository diagnostic.
fn scope_error(error: ScopeError) -> RepoError {
    if let ScopeError::Db(ref cause) = error
        && [sea_orm::DbBackend::Postgres, sea_orm::DbBackend::Sqlite]
            .into_iter()
            .any(|backend| super::retry::is_driver_contention(backend, cause))
    {
        return RepoError::Conflict("idempotency database contention".to_owned());
    }
    RepoError::Db(error.to_string())
}

/// Dedup flow of a recognition schedule change: the claim, the result stamp and
/// the replay read must name the same row.
pub(crate) const FLOW_SCHEDULE_CHANGE: &str = "SCHEDULE_CHANGE";

/// Status literal written when a posting first claims a key.
const STATUS_CLAIMED: &str = "CLAIMED";
/// Status literal written once the posting is durably recorded.
pub(crate) const STATUS_POSTED: &str = "POSTED";
/// Idempotency-dedup status for a request whose effect was durably enqueued
/// onto `ledger_pending_event_queue` for a later (separate-transaction) apply,
/// rather than posted inline. The dedup column is `text` with no CHECK, so this
/// literal needs no migration. Consumed by `claim_queued` (this module) and the
/// queue repo's intake insert.
pub(crate) const STATUS_QUEUED: &str = "QUEUED";

/// The stored dedup row, returned on a replay so the caller can compare the
/// recorded `payload_hash` and read the prior posting reference.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PostingRefRow {
    pub payload_hash: String,
    pub result_entry_id: Option<Uuid>,
    pub status: String,
}

/// Result of a [`IdempotencyGate::claim`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClaimOutcome {
    /// This call won the race and now owns the posting.
    Claimed,
    /// The key was already present; the caller must compare `payload_hash`
    /// and, if it matches, return the prior reference as a replay.
    Replay(PostingRefRow),
}

/// At-most-once posting gate over `idempotency_dedup`.
#[derive(Clone, Default)]
pub struct IdempotencyGate;

impl IdempotencyGate {
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Claim `(tenant, flow, business_id)` inside `txn` for an **inline post**
    /// (seed status `CLAIMED`) via an `INSERT … ON CONFLICT DO NOTHING`, then
    /// read the row back. Delegates to [`Self::claim_with_status`].
    ///
    /// # Errors
    /// [`RepoError::Db`] on a storage failure, or [`RepoError::RowVanished`]
    /// if the dedup row cannot be read back inside the same transaction.
    pub async fn claim(
        &self,
        txn: &DbTx<'_>,
        tenant: Uuid,
        flow: &str,
        business_id: &str,
        payload_hash: &str,
    ) -> Result<ClaimOutcome, RepoError> {
        self.claim_with_status(txn, tenant, flow, business_id, payload_hash, STATUS_CLAIMED)
            .await
    }

    /// Claim `(tenant, flow, business_id)` inside `txn` for a **deferred-apply
    /// enqueue** (seed status `QUEUED`) — the work is durably queued onto
    /// `ledger_pending_event_queue` for a later, separate-transaction apply
    /// rather than posted inline. Same `INSERT … ON CONFLICT DO NOTHING` +
    /// read-back as [`Self::claim`]; only the seeded status differs. On a
    /// replay the returned [`PostingRefRow::status`] lets the caller tell a
    /// still-`QUEUED` intake from a `CLAIMED` (in-flight inline) or `POSTED`
    /// (finalized) one. Delegates to [`Self::claim_with_status`].
    ///
    /// # Errors
    /// [`RepoError::Db`] on a storage failure, or [`RepoError::RowVanished`]
    /// if the dedup row cannot be read back inside the same transaction.
    pub async fn claim_queued(
        &self,
        txn: &DbTx<'_>,
        tenant: Uuid,
        flow: &str,
        business_id: &str,
        payload_hash: &str,
    ) -> Result<ClaimOutcome, RepoError> {
        self.claim_with_status(txn, tenant, flow, business_id, payload_hash, STATUS_QUEUED)
            .await
    }

    /// Shared body for [`Self::claim`] / [`Self::claim_queued`]: an
    /// `INSERT … ON CONFLICT (tenant_id, flow, business_id) DO NOTHING` inside
    /// `txn` seeding `status = seed_status`, then a read-back. A fresh insert
    /// yields [`ClaimOutcome::Claimed`]; a conflict yields
    /// [`ClaimOutcome::Replay`] carrying the stored row (the caller compares
    /// `payload_hash` and reads `status` / `result_entry_id`).
    ///
    /// # Errors
    /// [`RepoError::Db`] on a storage failure, or [`RepoError::RowVanished`]
    /// if the dedup row cannot be read back inside the same transaction.
    async fn claim_with_status(
        &self,
        txn: &DbTx<'_>,
        tenant: Uuid,
        flow: &str,
        business_id: &str,
        payload_hash: &str,
        seed_status: &str,
    ) -> Result<ClaimOutcome, RepoError> {
        let scope = AccessScope::for_tenant(tenant);

        let am = idempotency_dedup::ActiveModel {
            tenant_id: Set(tenant),
            flow: Set(flow.to_owned()),
            business_id: Set(business_id.to_owned()),
            payload_hash: Set(payload_hash.to_owned()),
            result_entry_id: Set(None),
            result_schedule_id: Set(None),
            posted_at_utc: Set(None),
            status: Set(seed_status.to_owned()),
            retain_until: Set(None),
        };
        let on_conflict = OnConflict::columns([
            idempotency_dedup::Column::TenantId,
            idempotency_dedup::Column::Flow,
            idempotency_dedup::Column::BusinessId,
        ])
        .do_nothing()
        .to_owned();

        let inserted = match idempotency_dedup::Entity::insert(am.clone())
            .secure()
            .scope_with_model(&scope, &am)
            .map_err(scope_error)?
            .on_conflict_raw(on_conflict)
            .exec(txn)
            .await
        {
            Ok(_) => true,
            // The key already existed; the conflict swallowed the insert.
            Err(ScopeError::Db(DbErr::RecordNotInserted)) => false,
            Err(e) => return Err(scope_error(e)),
        };

        if inserted {
            return Ok(ClaimOutcome::Claimed);
        }

        let row = idempotency_dedup::Entity::find()
            .secure()
            .scope_with(&scope)
            .filter(
                Condition::all()
                    .add(idempotency_dedup::Column::TenantId.eq(tenant))
                    .add(idempotency_dedup::Column::Flow.eq(flow))
                    .add(idempotency_dedup::Column::BusinessId.eq(business_id)),
            )
            .one(txn)
            .await
            .map_err(scope_error)?
            .ok_or_else(|| {
                RepoError::RowVanished(format!("idempotency_dedup {tenant}/{flow}/{business_id}"))
            })?;

        Ok(ClaimOutcome::Replay(PostingRefRow {
            payload_hash: row.payload_hash,
            result_entry_id: row.result_entry_id,
            status: row.status,
        }))
    }

    /// In-transaction scoped read of the dedup row for `(tenant, flow,
    /// business_id)`, or `None` when absent — the read-back half of
    /// [`Self::claim_with_status`] WITHOUT the preceding insert. The
    /// queued-apply post path ([`crate::infra::posting::service::PostingService`]
    /// `ClaimMode::QueuedApply`) calls this instead of `claim`: the dedup row was
    /// already claimed `QUEUED` at intake, so a re-claim here would either be a
    /// no-op (the `INSERT … ON CONFLICT DO NOTHING` collides) and waste a round
    /// trip, or — worse — mis-read the row as a fresh `Claimed`. Reading lets the
    /// engine distinguish a still-`QUEUED` row (proceed to post + finalize) from
    /// an already-`POSTED` one (idempotent replay) inside the same serializable
    /// transaction that will finalize it. Scoped (`.secure().scope_with`) for
    /// SQL-level BOLA, exactly like the `claim` read-back.
    ///
    /// # Errors
    /// [`RepoError::Db`] on a scope or storage failure.
    pub async fn read(
        &self,
        txn: &DbTx<'_>,
        tenant: Uuid,
        flow: &str,
        business_id: &str,
    ) -> Result<Option<PostingRefRow>, RepoError> {
        let scope = AccessScope::for_tenant(tenant);
        let row = idempotency_dedup::Entity::find()
            .secure()
            .scope_with(&scope)
            .filter(
                Condition::all()
                    .add(idempotency_dedup::Column::TenantId.eq(tenant))
                    .add(idempotency_dedup::Column::Flow.eq(flow))
                    .add(idempotency_dedup::Column::BusinessId.eq(business_id)),
            )
            .one(txn)
            .await
            .map_err(scope_error)?;
        Ok(row.map(|row| PostingRefRow {
            payload_hash: row.payload_hash,
            result_entry_id: row.result_entry_id,
            status: row.status,
        }))
    }

    /// Persist the immutable SCHEDULE_CHANGE successor without changing posting statuses.
    /// Cancel records no successor; the committed claim is its durable completion marker.
    /// Takes the transaction, like [`Self::claim`]: the stamp must run in the
    /// claiming transaction.
    /// # Errors
    /// Returns a typed storage failure or missing/invalid schedule-change claim.
    pub(crate) async fn store_schedule_change_result(
        &self,
        txn: &DbTx<'_>,
        tenant: Uuid,
        change_id: &str,
        payload_hash: &str,
        successor: Option<&str>,
    ) -> Result<(), RepoError> {
        let scope = AccessScope::for_tenant(tenant);
        let result = idempotency_dedup::Entity::update_many()
            .secure()
            .scope_with(&scope)
            .col_expr(
                idempotency_dedup::Column::ResultScheduleId,
                Expr::value(successor.map(str::to_owned)),
            )
            .filter(
                Condition::all()
                    .add(idempotency_dedup::Column::TenantId.eq(tenant))
                    .add(idempotency_dedup::Column::Flow.eq(FLOW_SCHEDULE_CHANGE))
                    .add(idempotency_dedup::Column::BusinessId.eq(change_id))
                    .add(idempotency_dedup::Column::PayloadHash.eq(payload_hash))
                    .add(idempotency_dedup::Column::Status.eq(STATUS_CLAIMED))
                    .add(idempotency_dedup::Column::ResultEntryId.is_null())
                    .add(idempotency_dedup::Column::ResultScheduleId.is_null()),
            )
            .exec(txn)
            .await
            .map_err(scope_error)?;
        if result.rows_affected != 1 {
            return Err(RepoError::RowVanished(
                "schedule change result claim missing or already set".into(),
            ));
        }
        Ok(())
    }

    /// Read only the schedule-change result after caller authorization of the original target.
    /// The schedule resource ID is not the dedup business ID; use the established tenant key.
    /// Ordinary posting result APIs remain unchanged.
    /// # Errors
    /// Rejects absent/corrupt claim evidence and preserves actual contention.
    pub(crate) async fn read_schedule_change_result(
        &self,
        txn: &DbTx<'_>,
        tenant: Uuid,
        change_id: &str,
    ) -> Result<Option<String>, RepoError> {
        let scope = AccessScope::for_tenant(tenant);
        let row = idempotency_dedup::Entity::find()
            .secure()
            .scope_with(&scope)
            .filter(
                Condition::all()
                    .add(idempotency_dedup::Column::TenantId.eq(tenant))
                    .add(idempotency_dedup::Column::Flow.eq(FLOW_SCHEDULE_CHANGE))
                    .add(idempotency_dedup::Column::BusinessId.eq(change_id)),
            )
            .one(txn)
            .await
            .map_err(scope_error)?
            .ok_or_else(|| RepoError::RowVanished("schedule change result claim missing".into()))?;
        if row.status != STATUS_CLAIMED
            || row.result_entry_id.is_some()
            || row.result_schedule_id.as_deref().is_some_and(str::is_empty)
        {
            return Err(RepoError::Db(
                "invalid schedule change result evidence".into(),
            ));
        }
        Ok(row.result_schedule_id)
    }

    /// Stamp the result entry id + posted timestamp and flip the status to
    /// `POSTED` before COMMIT, so a concurrent replay reads a complete
    /// reference. `created_seq` is the journal header's monotonic sequence
    /// (owned by `journal_entry`); it is threaded through for the public
    /// contract and asserted positive — the dedup row keys the posting by
    /// `result_entry_id`, not by the sequence.
    ///
    /// # Panics
    /// In debug builds if `created_seq` is not positive (the header must be
    /// sequenced before finalize).
    ///
    /// # Errors
    /// [`RepoError::Db`] on a storage failure, or [`RepoError::RowVanished`]
    /// if the claimed dedup row could not be updated inside the transaction.
    pub async fn finalize(
        &self,
        txn: &DbTx<'_>,
        tenant: Uuid,
        flow: &str,
        business_id: &str,
        entry_id: Uuid,
        created_seq: i64,
    ) -> Result<(), RepoError> {
        debug_assert!(created_seq > 0, "finalize before the header was sequenced");
        let scope = AccessScope::for_tenant(tenant);

        let result = idempotency_dedup::Entity::update_many()
            .secure()
            .scope_with(&scope)
            .col_expr(
                idempotency_dedup::Column::ResultEntryId,
                Expr::value(Some(entry_id)),
            )
            .col_expr(
                idempotency_dedup::Column::PostedAtUtc,
                Expr::value(Some(OffsetDateTime::now_utc())),
            )
            .col_expr(
                idempotency_dedup::Column::Status,
                Expr::value(STATUS_POSTED.to_owned()),
            )
            .filter(
                Condition::all()
                    .add(idempotency_dedup::Column::TenantId.eq(tenant))
                    .add(idempotency_dedup::Column::Flow.eq(flow))
                    .add(idempotency_dedup::Column::BusinessId.eq(business_id)),
            )
            .exec(txn)
            .await
            .map_err(scope_error)?;

        if result.rows_affected == 0 {
            return Err(RepoError::RowVanished(format!(
                "idempotency_dedup {tenant}/{flow}/{business_id}"
            )));
        }
        Ok(())
    }

    /// SHA-256 through the toolkit-security facade hex digest over the canonical
    /// financial content of an entry — the business key, effective date,
    /// period, and every per-line financial dimension (amount + scale,
    /// functional amount/currency, currency, side, payer/seller, invoice,
    /// due date, revenue stream, tax dims, credit-grant event type, AR dispute
    /// status), sorted for order-independence.
    /// Transport/envelope fields (correlation id, actor, posted-at, internal
    /// ids) are excluded so the same financial intent always hashes
    /// identically; a change in any financial dimension flips the hash so a
    /// conflicting reuse of the business key is caught, not swallowed.
    #[must_use]
    pub fn payload_hash(entry: &NewEntry, lines: &[NewLine]) -> String {
        use crate::domain::canonical::{
            digest32_hex, put, put_money, put_opt_money, put_opt_str, put_opt_uuid, put_str,
            put_uuid,
        };
        let mut canon = b"VHP-BSS-LEDGER-PAYLOAD-v1\x1f".to_vec();
        put_str(&mut canon, entry.source_doc_type.as_str());
        put_str(&mut canon, &entry.source_business_id);
        put_str(&mut canon, &entry.entry_currency);
        put_str(&mut canon, &entry.effective_at.to_string());
        put_str(&mut canon, &entry.period_id);
        let mut keys: Vec<Vec<u8>> = lines
            .iter()
            .map(|line| {
                let mut key = Vec::new();
                put_uuid(&mut key, line.account_id);
                put_str(&mut key, line.account_class.as_str());
                put_money(&mut key, &line.money);
                put_str(&mut key, line.side.as_str());
                put_uuid(&mut key, line.payer_tenant_id);
                put_opt_uuid(&mut key, line.seller_tenant_id);
                put_opt_str(&mut key, line.invoice_id.as_deref());
                put_opt_str(
                    &mut key,
                    line.due_date.map(|date| date.to_string()).as_deref(),
                );
                put_opt_str(&mut key, line.revenue_stream.as_deref());
                put_opt_money(&mut key, line.functional_money.as_ref());
                put_opt_str(&mut key, line.tax_jurisdiction.as_deref());
                put_opt_str(&mut key, line.tax_filing_period.as_deref());
                put_opt_str(&mut key, line.credit_grant_event_type.as_deref());
                put_opt_str(&mut key, line.ar_status.as_deref());
                key
            })
            .collect();
        keys.sort();
        put(&mut canon, &(keys.len() as u64).to_be_bytes());
        for key in keys {
            put(&mut canon, &key);
        }
        digest32_hex(&canon)
    }

    /// SHA-256 through the toolkit-security facade hex digest over an arbitrary
    /// already-canonical string — the same crypto provider [`Self::payload_hash`]
    /// runs, factored out so a caller that hashes a *request* payload (not an
    /// entry) can reuse it. A queued allocation hashes its request payload here:
    /// the per-invoice split is only decided at apply (Group D), so the entry —
    /// and thus `payload_hash`'s entry-based signature — does not yet exist at
    /// intake. The caller is responsible for canonicalizing `canonical` stably
    /// (e.g. a fixed field order) so the same intent always hashes identically.
    #[must_use]
    pub(crate) fn content_hash(canonical: &str) -> String {
        let digest = Sha256::digest(canonical.as_bytes());
        let mut hex = String::with_capacity(64);
        for byte in digest.as_ref() {
            use std::fmt::Write as _;
            let _ = write!(hex, "{byte:02x}");
        }
        hex
    }
}

#[cfg(test)]
#[path = "idempotency_tests.rs"]
mod idempotency_tests;

#[cfg(test)]
#[path = "idempotency_hash_tests.rs"]
mod idempotency_hash_tests;
