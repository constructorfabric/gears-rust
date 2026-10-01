//! Persistence port for secret metadata and value-pointer switching
//! (ADR-0006: immutable value versions).
//!
//! Every pointer switch is ONE database compare-and-set on the row `version`.
//! [`Self::delete_by_id`] pairs the
//! `credstore_secrets` mutation with the enqueue of a key purge in the
//! platform transactional outbox, atomically in the same transaction.

use async_trait::async_trait;
use credstore_sdk::{OwnerId, SecretRef, SharingMode, StoreKey, TenantId, ValueVersion};
use time::OffsetDateTime;
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::secret::model::{Fallback, NewDeclaredSecret, NewSecret, SecretRow};

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

    /// Create step 3: `INSERT` the row `active`, pointing at
    /// `new.value_version` (with `new.fallback`). A unique-index conflict on
    /// the reference's own create-only uniqueness maps to the existing
    /// `Conflict` error (a definite loss).
    async fn insert_active(&self, scope: &AccessScope, new: &NewSecret) -> Result<(), DomainError>;

    /// Create-with-no-value path (ADR-0004 Amendment B, "The value-less
    /// record: reached only on purpose"): ONE plain `INSERT` — `status =
    /// declared`, `value_version` `NULL`. No plugin call is ever made. A
    /// unique-index conflict maps to the existing `Conflict` error, exactly
    /// like [`Self::insert_active`].
    async fn insert_declared(
        &self,
        scope: &AccessScope,
        new: &NewDeclaredSecret,
    ) -> Result<(), DomainError>;

    /// Overwrite step 3: ONE compare-and-set —
    /// `UPDATE … SET value_version = new_value_version, sharing, fallback,
    /// expires_at, version = version + 1, updated_at = now(), status = active
    /// WHERE id = ? AND version = expected_version`; 0 rows affected →
    /// `Ok(None)` (a definite loss; the caller maps it to a conflict).
    /// `expected_version` is always the row version the caller read in step 1,
    /// whatever the client precondition: the ordering argument that makes
    /// `destroy(Below)` safe holds only for a CAS on the version read before
    /// the `put`. Accepts a **`declared`** current row — `PUT`'s replace leg
    /// and `PATCH {"secret": …}` both switch a `declared` row to `active`
    /// this way (ADR-0004). Returns the post-write row.
    #[allow(
        clippy::too_many_arguments,
        reason = "one CAS with every field it may update"
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
    ) -> Result<Option<SecretRow>, DomainError>;

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

    /// Secret removal (ADR-0004 `PATCH {"secret": null}`): ONE compare-and-set
    /// — `UPDATE … SET value_version = NULL, status = declared, sharing,
    /// fallback, expires_at, version = version + 1, updated_at = now() WHERE
    /// id = ? [AND version = ?]`. 0 rows affected → `Ok(None)`. Returns the
    /// post-write (now `declared`) row plus the value version the row held
    /// (read under the same lock; `None` if it was already `declared`) so the
    /// caller can best-effort `destroy` it. Never touches the store.
    async fn remove_value(
        &self,
        scope: &AccessScope,
        id: Uuid,
        expected_version: Option<i64>,
        sharing: SharingMode,
        fallback: Fallback,
        expires_at: Option<OffsetDateTime>,
    ) -> Result<Option<(SecretRow, Option<ValueVersion>)>, DomainError>;

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
