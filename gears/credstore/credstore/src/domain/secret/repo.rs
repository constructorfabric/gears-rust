//! Persistence port for secret metadata, value-pointer switching and the
//! write-intent journal (ADR-0006: immutable value versions).
//!
//! Every pointer switch is ONE database compare-and-set on the row `version`.
//! Every store side effect is either announced before it happens (a write
//! intent, [`SecretRepo::begin_write_intent`]) or executed by an outbox task
//! enqueued in the same transaction that learned it is needed: the methods
//! that change a row or retire an intent pair that change with the enqueue of
//! the [`CleanupTask`]s it implies in the platform transactional outbox,
//! atomically.

use std::time::Duration;

use async_trait::async_trait;
use credstore_sdk::{OwnerId, SecretRef, SharingMode, StoreKey, TenantId, ValueVersion};
use time::OffsetDateTime;
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::secret::model::{
    CleanupTask, Fallback, IntentCommit, NewDeclaredSecret, NewSecret, Reclaimed, SecretRow,
    WriteAttempt,
};

#[async_trait]
pub trait SecretRepo: Send + Sync {
    /// Resolve the winning row for `req_tenant` walking the ordered `chain`
    /// (req first, root last), applying two-phase priority + sharing. The
    /// resolution predicate is `status = active OR (status = declared AND
    /// fallback = none)` (ADR-0004, Suppression): a `declared`/`none` row
    /// competes and, when nearest, wins (blocking the walk); a
    /// `declared`/`inherit` row never competes.
    async fn resolve_for_get(
        &self,
        req_tenant: TenantId,
        subject: OwnerId,
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
    /// [`crate::domain::secret::service::Service`] reduces the hierarchy in
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
    /// legitimate "own record" `patch`/`delete` must be able to find.
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
    /// insert `{attempt_id, tenant_id, record_id, lease_until = now() +
    /// lease}` (the database clock; `record_id` and `tenant_id` from
    /// `attempt.key`). A failure means nothing was announced and the caller
    /// writes nothing to the store.
    async fn begin_write_intent(
        &self,
        attempt: &WriteAttempt,
        lease: Duration,
    ) -> Result<(), DomainError>;

    /// Best-effort retirement of an intent whose attempt will not `put` (the
    /// lease guard fired): delete `attempt_id`, whether or not it still
    /// exists.
    async fn drop_write_intent(&self, attempt_id: Uuid) -> Result<(), DomainError>;

    /// Create step 3 (tx1): ONE transaction that deletes the attempt's
    /// intent (which must affect exactly one row) and `INSERT`s the row
    /// `active`, pointing at `new.value_version` (with `new.fallback`).
    ///
    /// * [`IntentCommit::Committed`] — both done; nothing enqueued (the key
    ///   is fresh, no older version can exist).
    /// * [`IntentCommit::Lost`] — the insert hit the reference's create-only
    ///   uniqueness (a definite loss): the intent deletion commits anyway,
    ///   together with an enqueued `purge(key)` (the fresh record id can never
    ///   get a row).
    /// * [`IntentCommit::IntentLost`] — the intent was already reclaimed; the
    ///   transaction changed nothing.
    ///
    /// Any `Err` is ambiguous (the commit may or may not have happened); the
    /// intent may still exist and is reclaimed later.
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
    /// * [`IntentCommit::Committed`] — the post-write row, plus an enqueued
    ///   `destroy(key, Below(new_value_version))` when
    ///   `attempt.destroy_supported`.
    /// * [`IntentCommit::Lost`] — 0 rows affected (a definite loss; the caller
    ///   maps it to a conflict). The intent deletion commits anyway, with an
    ///   enqueued `destroy(key, Exactly(new_value_version))` if a row with
    ///   this record id still exists (and the plugin supports destroy), else
    ///   `purge(key)`.
    /// * [`IntentCommit::IntentLost`] — the intent was already reclaimed; the
    ///   transaction changed nothing.
    ///
    /// Any `Err` is ambiguous, as for [`Self::insert_active`].
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

    /// Step 5c, after [`IntentCommit::IntentLost`]: the writer is alive and
    /// knows the version `version` it `put` under `key`, but its intent was
    /// reclaimed, so nobody else will clean that version up. ONE
    /// transaction: enqueue `destroy(key, Exactly(version))` if the row with
    /// `key.record_id` exists (and `destroy_supported`), else `purge(key)`.
    /// Returns what it enqueued. If this fails the version leaks until the
    /// record's next secret write or delete (the documented lease-window
    /// residual).
    async fn settle_lost_intent(
        &self,
        key: &StoreKey,
        version: &ValueVersion,
        destroy_supported: bool,
    ) -> Result<Vec<CleanupTask>, DomainError>;

    /// Reclaim, in ONE transaction: select up to `limit` intents with
    /// `lease_until < now()` (`PostgreSQL`: `ORDER BY lease_until LIMIT $1 FOR
    /// UPDATE SKIP LOCKED`) and delete them; for each whose record has NO
    /// `credstore_secrets` row (`SELECT 1 … LIMIT 1`, never a `COUNT`),
    /// enqueue `purge(key)`. An intent whose row exists enqueues nothing (an
    /// orphan above a live record's pointer: removed by the record's next
    /// secret write or its delete).
    async fn reclaim_expired(&self, limit: u64) -> Result<Reclaimed, DomainError>;

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
    /// held a value version `old` and `destroy_supported`, the enqueue of
    /// `destroy(key, Below(old))` and `destroy(key, Exactly(old))` (never
    /// `delete_key`: a concurrent writer may already have put a newer version
    /// under the same key). 0 rows affected → `Ok(None)`. Returns the
    /// post-write (now `declared`) row and what it enqueued. No intent (there
    /// is no `put`) and never an inline store call.
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
    ) -> Result<Option<(SecretRow, Vec<CleanupTask>)>, DomainError>;

    /// Delete record (section 6.3): ONE transaction — `DELETE` the row (CAS
    /// on `expected_version` when given; 0 rows affected →
    /// `DomainError::NotFound`) and enqueue an outbox `purge(key)` message,
    /// where `key = (tenant_id, id)`. The outbox wake is fired after the
    /// commit.
    async fn delete_by_id(
        &self,
        scope: &AccessScope,
        key: &StoreKey,
        expected_version: Option<i64>,
    ) -> Result<(), DomainError>;
}
