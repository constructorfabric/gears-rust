// Updated: 2026-10-06 by Constructor Tech
//! Persistence port for secret metadata, value-pointer switching and the
//! write-intent journal (ADR-0006: immutable value versions).
//!
//! Every pointer switch is ONE database compare-and-set on the row `version`.
//! Every store side effect is either announced before it happens (a write
//! intent, [`SecretRepo::begin_write_intent`]) or recorded as a cleanup debt
//! in the same transaction that learned it is needed: the methods that change
//! a row or retire an intent pair that change with the [`CleanupDebt`]s it
//! implies, atomically. Executing a debt is the caller's job, after the
//! commit is confirmed ([`SecretRepo::delete_debt`] removes it afterwards).

use std::time::Duration;

use async_trait::async_trait;
use credstore_sdk::{OwnerId, SecretRef, SharingMode, StoreKey, TenantId, ValueVersion};
use time::OffsetDateTime;
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::secret::model::{
    CleanupDebt, DeleteVerification, Fallback, HealedCreates, IntentCommit, NewDeclaredSecret,
    NewSecret, SecretRow, WriteAttempt, WriteVerification,
};

#[async_trait]
pub trait SecretRepo: Send + Sync {
    /// Resolve the winning row for `req_tenant` walking the ordered `chain`
    /// (req first, root last), applying two-phase priority + sharing. The
    /// resolution predicate is `status = active OR (status = declared AND
    /// fallback = none)` (ADR-0004, Suppression): a `declared`/`none` row
    /// competes and, when nearest, wins (blocking the walk); a
    /// `declared`/`inherit` row never competes. The row carries the heal
    /// flags ([`SecretRow::heal`]), computed by the same SQL.
    async fn resolve_for_get(
        &self,
        req_tenant: TenantId,
        subject: OwnerId,
        key: &SecretRef,
        chain: &[Uuid],
    ) -> Result<Option<SecretRow>, DomainError>;

    /// Create-time upward type check
    /// (`cpt-cf-credstore-fr-override-type-consistency`): the winning row for
    /// `req_tenant` among **non-private** rows only (a `shared` row in `chain`
    /// or a `tenant` row of `req_tenant`), same resolution predicate and
    /// nearest-tenant order as [`Self::resolve_for_get`]. Private rows, the
    /// caller's own included, never take part.
    async fn resolve_non_private(
        &self,
        req_tenant: TenantId,
        key: &SecretRef,
        chain: &[Uuid],
    ) -> Result<Option<SecretRow>, DomainError>;

    /// Every row of the reference visible to the caller across `chain`
    /// (`req_tenant` first, root last), for the credential-**record** read
    /// (ADR-0004 `get`): the caller's own-tenant rows of **any** status
    /// (private-for-subject, tenant, shared — so the record view can report
    /// `declared`/`inherit` and its validator even though such a row never
    /// resolves), plus every ancestor's `shared` row that passes the
    /// resolution predicate above (an ancestor's `declared`/`inherit` row is
    /// invisible here, exactly as it is to a value read). One SQL query;
    /// `Service` reduces the hierarchy in
    /// memory (ADR-0005 "Reducing a reference to one item": nearest
    /// resolvable row wins; a `declared`/`none` winner blocks).
    async fn resolve_candidates(
        &self,
        req_tenant: TenantId,
        subject: OwnerId,
        key: &SecretRef,
        chain: &[Uuid],
    ) -> Result<Vec<SecretRow>, DomainError>;

    /// Find the caller's own-tenant row (two-phase: private-for-subject, else
    /// tenant/shared), of **either** resting status — a `declared` row is a
    /// legitimate "own record" `patch`/`delete` must be able to find. The row
    /// carries the heal flags ([`SecretRow::heal`]).
    async fn find_own(
        &self,
        scope: &AccessScope,
        tenant: TenantId,
        subject: OwnerId,
        key: &SecretRef,
    ) -> Result<Option<SecretRow>, DomainError>;

    /// Find the row a write of `sharing` would target, by sharing-class identity
    /// (mirrors the partial unique indexes): `Private` → `(tenant, ref, owner)`,
    /// `Tenant`/`Shared` → `(tenant, ref)` among non-private — of **either**
    /// resting status, so `put` can see a `declared` row it must treat as
    /// "already exists" (ADR-0004: "does my tenant hold a row under this
    /// reference" counts a `declared` row too). Unlike [`Self::find_own`]
    /// this never crosses the private boundary, so a private write does not see a
    /// coexisting tenant/shared secret (and vice-versa) — they coexist per design.
    /// The row carries the heal flags ([`SecretRow::heal`]).
    async fn find_for_write(
        &self,
        scope: &AccessScope,
        tenant: TenantId,
        subject: OwnerId,
        key: &SecretRef,
        sharing: SharingMode,
    ) -> Result<Option<SecretRow>, DomainError>;

    /// True iff `tenant` is within `scope`, ignoring credential-type
    /// predicates (the fail-closed own-tenant gate; type narrowing is
    /// applied by the lookups themselves).
    async fn scope_includes_tenant(
        &self,
        scope: &AccessScope,
        tenant: Uuid,
    ) -> Result<bool, DomainError>;

    /// Create-time downward type check
    /// (`cpt-cf-credstore-fr-override-type-consistency`): the distinct
    /// tenants, other than `exclude_tenant`, holding a row under `reference`
    /// of a type other than `requested_type` — any status and owner,
    /// non-private rows only (private records are exempt).
    /// An unscoped internal lookup (no PDP clamp), keyset-paged by tenant id:
    /// tenants after `after` in ascending order, at most `limit`. Never a
    /// `COUNT`.
    async fn list_tenants_with_other_type(
        &self,
        reference: &SecretRef,
        requested_type: Uuid,
        exclude_tenant: TenantId,
        after: Option<Uuid>,
        limit: u64,
    ) -> Result<Vec<Uuid>, DomainError>;

    // ── Collection read (ADR-0005) ──────────────────────────────────────────

    /// Step 1: candidate **references** visible across `chain`, under the
    /// same predicate [`Self::resolve_candidates`] applies per reference —
    /// own tenant: every sharing-visible row of any status; ancestors:
    /// resolution-eligible `shared` rows only — clamped by an exact
    /// `reference` set when the caller's `$filter` named one, and by
    /// `type_scope`, a type-only scope the caller (the collection-read
    /// service) derived from one PDP decision on the base credential type
    /// and intersected with the caller's own `$filter type in (…)` (ADR-0005,
    /// ADR-0010), applied through the secure ORM — both invariant across a
    /// reference's chain. `DISTINCT
    /// reference`, ordered by `reference` (`desc` when `desc`),
    /// keyset-paginated by `cursor` (exclusive); fetches at most `limit`
    /// references. Never a `COUNT`.
    #[allow(
        clippy::too_many_arguments,
        reason = "every clamp the collection read's step 1 query supports"
    )]
    async fn list_candidate_references(
        &self,
        req_tenant: TenantId,
        subject: OwnerId,
        chain: &[Uuid],
        reference_in: Option<&[String]>,
        type_scope: &AccessScope,
        cursor: Option<&str>,
        desc: bool,
        limit: u64,
    ) -> Result<Vec<String>, DomainError>;

    /// Step 2: every visible row of `references`, whole and unclamped by
    /// type — exactly what [`Self::resolve_candidates`] would return for
    /// each reference individually, so reduction sees every row a value
    /// read would see.
    async fn list_candidates_for_references(
        &self,
        req_tenant: TenantId,
        subject: OwnerId,
        chain: &[Uuid],
        references: &[String],
    ) -> Result<Vec<SecretRow>, DomainError>;

    // ── Write protocol (ADR-0006 section 6.2) ───────────────────────────────

    /// tx0 of a secret write: announce the attempt before `plugin.put` —
    /// insert `{attempt_id, tenant_id, record_id, reference, lease_until =
    /// now() + lease}` (the database clock; `record_id` and `tenant_id` from
    /// `attempt.key`, `reference` from `attempt.reference`). A failure means nothing was announced and the caller
    /// writes nothing to the store.
    async fn begin_write_intent(
        &self,
        attempt: &WriteAttempt,
        lease: Duration,
    ) -> Result<(), DomainError>;

    /// Create step 3 (tx1): ONE transaction that deletes the attempt's
    /// intent (which must affect exactly one row) and `INSERT`s the row
    /// `active`, pointing at `new.value_version` (with `new.fallback`).
    ///
    /// * [`IntentCommit::Committed`] — both done; no debt (the key is fresh,
    ///   no older version can exist).
    /// * [`IntentCommit::Lost`] — the insert hit the reference's create-only
    ///   uniqueness (a definite loss): the intent deletion commits anyway,
    ///   together with a recorded `purge(key)` debt (the fresh record id can
    ///   never get a row).
    /// * [`IntentCommit::IntentLost`] — the intent was already healed; the
    ///   transaction changed nothing.
    ///
    /// Any `Err` is ambiguous (the commit may or may not have happened); the
    /// caller resolves it with [`Self::verify_insert_active`].
    async fn insert_active(
        &self,
        scope: &AccessScope,
        new: &NewSecret,
        attempt: &WriteAttempt,
    ) -> Result<IntentCommit<()>, DomainError>;

    /// Create-with-no-value path (ADR-0004 Amendment B, "The value-less
    /// record: reached only on purpose"): ONE plain `INSERT` — `status =
    /// declared`, `value_version` `NULL`. No plugin call is ever made and no
    /// intent is written. A unique-index conflict maps to the existing
    /// `Conflict` error.
    async fn insert_declared(
        &self,
        scope: &AccessScope,
        new: &NewDeclaredSecret,
    ) -> Result<(), DomainError>;

    /// Overwrite step 3 (tx1): ONE transaction that deletes the attempt's
    /// intent (which must affect exactly one row) and runs ONE
    /// compare-and-set —
    /// `UPDATE … SET value_version = new_value_version, sharing, fallback,
    /// expires_at, version = version + 1, updated_at = now(), status = active
    /// WHERE id = ? AND version = expected_version`.
    /// `expected_version` is always the row version the caller read in step 1,
    /// whatever the client precondition: the ordering argument that makes
    /// `destroy(Below)` safe holds only for a CAS on the version read before
    /// the `put`. Accepts a **`declared`** current row — `PUT`'s replace leg
    /// and `PATCH {"secret": …}` both switch a `declared` row to `active`
    /// this way (ADR-0004).
    ///
    /// * [`IntentCommit::Committed`] — the post-write row, plus a recorded
    ///   `destroy(key, Below(new_value_version))` debt when
    ///   `attempt.destroy_supported`; when `attempt.heal_expired_intents`,
    ///   the same transaction deletes the record's expired intents
    ///   (`lease_until < now()`, re-checked in the DELETE) and reports how
    ///   many.
    /// * [`IntentCommit::Lost`] — 0 rows affected (a definite loss; the caller
    ///   maps it to a conflict). The intent deletion commits anyway, with a
    ///   recorded `destroy(key, Exactly(new_value_version))` debt if a row
    ///   with this record id still exists (and the plugin supports destroy),
    ///   else a `purge(key)` debt.
    /// * [`IntentCommit::IntentLost`] — the intent was already healed; the
    ///   transaction changed nothing.
    ///
    /// Any `Err` is ambiguous, as for [`Self::insert_active`]; the caller
    /// resolves it with [`Self::verify_switch_value`].
    #[allow(
        clippy::too_many_arguments,
        reason = "one CAS with every field it may update, plus the attempt it retires"
    )]
    async fn switch_value(
        &self,
        scope: &AccessScope,
        id: Uuid,
        expected_version: i64,
        sharing: SharingMode,
        fallback: Fallback,
        expires_at: Option<OffsetDateTime>,
        new_value_version: ValueVersion,
        attempt: &WriteAttempt,
    ) -> Result<IntentCommit<SecretRow>, DomainError>;

    /// Failed-create heal: the expired intents of `(tenant, reference)`
    /// whose record id has NO `credstore_secrets` row (`NOT EXISTS`, never a
    /// `COUNT`) are deleted (`lease_until < now()` re-checked in the DELETE;
    /// an intent another instance already took is skipped) and a `purge(key)`
    /// debt is recorded for each distinct key, all in ONE transaction. Finds
    /// nothing without writing anything. The caller executes the returned
    /// debts after the commit.
    async fn heal_failed_creates(
        &self,
        tenant: TenantId,
        reference: &SecretRef,
    ) -> Result<HealedCreates, DomainError>;

    /// The pending cleanup debts of the record `key` (a point lookup on
    /// `(tenant_id, record_id)`), for heal on access.
    async fn pending_debts(&self, key: &StoreKey) -> Result<Vec<CleanupDebt>, DomainError>;

    /// Deletes the debt row `id` after its execution succeeded. Deleting an
    /// already-deleted row affects nothing and is not an error.
    async fn delete_debt(&self, id: Uuid) -> Result<(), DomainError>;

    /// Metadata-only update (ADR-0004 `PATCH` with no `value` key): ONE
    /// transaction — `UPDATE … SET sharing, fallback, expires_at, version =
    /// version + 1, updated_at = now() WHERE id = ? [AND version = ?]`;
    /// never touches `value_version` or `status`. 0 rows affected →
    /// `Ok(None)` (version mismatch or the row vanished).
    async fn update_metadata(
        &self,
        scope: &AccessScope,
        id: Uuid,
        expected_version: Option<i64>,
        sharing: SharingMode,
        fallback: Fallback,
        expires_at: Option<OffsetDateTime>,
    ) -> Result<Option<SecretRow>, DomainError>;

    /// Secret removal (ADR-0004 `PATCH {"secret": null}`): ONE transaction —
    /// the compare-and-set `UPDATE … SET value_version = NULL, status =
    /// declared, sharing, fallback, expires_at, version = version + 1,
    /// updated_at = now() WHERE id = ? [AND version = ?]` plus, when the row
    /// held a value version `old` and `destroy_supported`, the recorded debts
    /// `destroy(key, Below(old))` and `destroy(key, Exactly(old))` (never
    /// `delete_key`: a concurrent writer may already have put a newer version
    /// under the same key). 0 rows affected → `Ok(None)`. Returns the
    /// post-write (now `declared`) row and the debts it recorded. No intent
    /// (there is no `put`) and never a store call.
    #[allow(
        clippy::too_many_arguments,
        reason = "one CAS with every field it may update, plus the plugin's destroy capability"
    )]
    async fn remove_value(
        &self,
        scope: &AccessScope,
        id: Uuid,
        expected_version: Option<i64>,
        sharing: SharingMode,
        fallback: Fallback,
        expires_at: Option<OffsetDateTime>,
        destroy_supported: bool,
    ) -> Result<Option<(SecretRow, Vec<CleanupDebt>)>, DomainError>;

    /// Delete record (section 6.3): ONE transaction — `DELETE` the row (CAS
    /// on `expected_version` when given; 0 rows affected →
    /// `DomainError::NotFound`) and record a `purge(key)` debt, where
    /// `key = (tenant_id, id)`. Returns the debt; the caller executes it
    /// after the commit.
    async fn delete_by_id(
        &self,
        scope: &AccessScope,
        key: &StoreKey,
        expected_version: Option<i64>,
    ) -> Result<Vec<CleanupDebt>, DomainError>;

    // ── Verification after an ambiguous commit (ADR-0006 section 6.2) ───────

    /// Verification of [`Self::insert_active`] after an ambiguous commit:
    /// ONE transaction that starts with a LOCKING read of the attempt's own
    /// intent (`SELECT … FOR UPDATE` by `attempt_id`; a pending tx1 holds that
    /// lock, so this waits for it to resolve) and reads the record row.
    ///
    /// * intent present → tx1 had not committed: it runs again here
    ///   ([`WriteVerification::Retried`]);
    /// * intent gone, row points at `new.value_version` →
    ///   [`WriteVerification::Committed`];
    /// * intent gone, row does not → [`WriteVerification::NotApplied`], with
    ///   the recorded `destroy(Exactly(vv))` debt (or `purge(key)` when no
    ///   row has the record id).
    ///
    /// `Err` means the verification itself failed: the outcome stays unknown.
    async fn verify_insert_active(
        &self,
        scope: &AccessScope,
        new: &NewSecret,
        attempt: &WriteAttempt,
    ) -> Result<WriteVerification<()>, DomainError>;

    /// Verification of [`Self::switch_value`] after an ambiguous commit; the
    /// same transaction and branches as [`Self::verify_insert_active`], with
    /// the CAS of `switch_value` as the retried body.
    #[allow(
        clippy::too_many_arguments,
        reason = "the same CAS as switch_value: it may have to run it again"
    )]
    async fn verify_switch_value(
        &self,
        scope: &AccessScope,
        id: Uuid,
        expected_version: i64,
        sharing: SharingMode,
        fallback: Fallback,
        expires_at: Option<OffsetDateTime>,
        new_value_version: ValueVersion,
        attempt: &WriteAttempt,
    ) -> Result<WriteVerification<SecretRow>, DomainError>;

    /// Verification of [`Self::delete_by_id`] after an ambiguous commit: ONE
    /// transaction that locks the record row by id (`SELECT … FOR UPDATE`; a
    /// pending delete holds the lock). No row → [`DeleteVerification::Committed`];
    /// a row → the delete runs again with the same precondition
    /// ([`DeleteVerification::Retried`], or
    /// [`DeleteVerification::PreconditionFailed`] when the row changed).
    async fn verify_delete(
        &self,
        scope: &AccessScope,
        key: &StoreKey,
        expected_version: Option<i64>,
    ) -> Result<DeleteVerification, DomainError>;
}
