use async_trait::async_trait;
use credstore_sdk::{OwnerId, SecretRef, SharingMode, TenantId};
use time::OffsetDateTime;
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::ports::metrics::SecretCounts;
use crate::domain::secret::model::{NewSecret, SecretRow, SecretStatus};

#[async_trait]
pub trait SecretRepo: Send + Sync {
    /// Resolve the winning active secret for `req_tenant` walking the ordered
    /// `chain` (req first, root last), applying two-phase priority + sharing.
    async fn resolve_for_get(
        &self,
        req_tenant: TenantId,
        subject: OwnerId,
        key: &SecretRef,
        chain: &[Uuid],
    ) -> Result<Option<SecretRow>, DomainError>;

    /// Insert a `provisioning` row (scope-checked); maps UNIQUE → Conflict.
    async fn insert_provisioning(
        &self,
        scope: &AccessScope,
        new: &NewSecret,
    ) -> Result<(), DomainError>;

    /// Flip provisioning → active (scope-checked). 0 rows → Conflict.
    async fn mark_active(&self, scope: &AccessScope, id: Uuid) -> Result<(), DomainError>;

    /// Bump the version (and set sharing + expiry + fence fingerprint) of an
    /// existing active row, keyed by id. Atomic `version = version + 1`. When
    /// `expected_version` is `Some`, the bump is gated on `version = expected`
    /// (optimistic concurrency). `expires_at` fully replaces the stored expiry
    /// (a PUT is a whole-value replace, expiry included). `value_fp` is the
    /// fingerprint of the value this write puts to the backend; it is set in
    /// the same atomic UPDATE as `sharing`, which is what makes a fingerprint
    /// match on read prove that value and metadata came from one writer.
    /// Returns the post-write row, or `None` if no active row matched (row gone,
    /// or — under `expected_version` — the version no longer matches).
    async fn touch(
        &self,
        scope: &AccessScope,
        id: Uuid,
        sharing: SharingMode,
        expected_version: Option<i64>,
        expires_at: Option<OffsetDateTime>,
        value_fp: Vec<u8>,
    ) -> Result<Option<SecretRow>, DomainError>;

    /// Stamp the fence fingerprint onto an out-of-band seeded row (CAS on
    /// `value_fp IS NULL`; a concurrent PUT that already stamped wins → `false`).
    /// Does NOT bump `version`/`updated_at` — nothing client-visible changed.
    async fn backfill_fp(
        &self,
        id: Uuid,
        value_fp: Vec<u8>,
        fp_key_id: i16,
    ) -> Result<bool, DomainError>;

    /// Active rows still missing a fence fingerprint (bounded batch; reaper
    /// backfill sweep).
    async fn list_unfenced(&self, limit: u64) -> Result<Vec<SecretRow>, DomainError>;

    /// Flip active → deprovisioning (scope-checked), stamping `updated_at`
    /// (the deprovisioning-timeout clock). When `expected_version` is `Some`,
    /// the flip is gated on `version = expected` (optimistic concurrency).
    /// The version itself is NOT bumped — an `If-Match` retry of the same
    /// delete must still match. Returns `false` if no active row matched.
    async fn mark_deprovisioning(
        &self,
        scope: &AccessScope,
        id: Uuid,
        expected_version: Option<i64>,
    ) -> Result<bool, DomainError>;

    /// Find the caller's own-tenant row (two-phase: private-for-subject, else
    /// tenant/shared). Returns `active` rows and — so a `DELETE` retry can
    /// resume a stuck delete saga — `deprovisioning` rows; never
    /// `provisioning` ones.
    async fn find_own(
        &self,
        scope: &AccessScope,
        tenant: TenantId,
        subject: OwnerId,
        key: &SecretRef,
    ) -> Result<Option<SecretRow>, DomainError>;

    /// Find the row a write of `sharing` would target, by sharing-class identity
    /// (mirrors the partial unique indexes): `Private` → `(tenant, ref, owner)`,
    /// `Tenant`/`Shared` → `(tenant, ref)` among non-private. Unlike [`find_own`]
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

    /// Delete a row by id (scope-checked). When `expected_version` is `Some`,
    /// the delete is gated on `version = expected` (optimistic concurrency).
    /// 0 rows → `NotFound`.
    async fn delete_by_id(
        &self,
        scope: &AccessScope,
        id: Uuid,
        expected_version: Option<i64>,
    ) -> Result<(), DomainError>;

    /// List stale saga rows for the reaper (unscoped, bounded by `limit`):
    /// `provisioning` rows whose `updated_at` is older than
    /// `provisioning_older_than_secs` and `deprovisioning` rows older than
    /// `deprovisioning_older_than_secs`. Provisioning rows are never updated
    /// after insert, so `updated_at` equals `created_at` for them.
    async fn list_stale_pending(
        &self,
        provisioning_older_than_secs: u64,
        deprovisioning_older_than_secs: u64,
        limit: u64,
    ) -> Result<Vec<SecretRow>, DomainError>;

    /// Delete a saga row by id, but only while it still holds `expected`
    /// status (unscoped; reaper cleanup). The status guard fences the delete
    /// against a concurrent saga transition (notably a slow create's
    /// `mark_active`), so a secret that became active after the reaper listed
    /// it is never removed. Returns `true` when this call removed the row,
    /// `false` when it was already gone or has moved on (both benign).
    async fn reap_by_id(&self, id: Uuid, expected: SecretStatus) -> Result<bool, DomainError>;

    /// Flip expired `active` rows (`expires_at <= now`) to `deprovisioning`
    /// (unscoped; reaper), stamping `updated_at`. The rows then complete
    /// through the ordinary deprovisioning sweep. Returns rows flipped.
    async fn mark_expired_deprovisioning(&self) -> Result<u64, DomainError>;

    /// Inventory counts by sharing + provisioning + distinct tenants (for gauges).
    async fn inventory(&self) -> Result<SecretCounts, DomainError>;

    /// True iff `tenant` is within the read `scope` (closure-backed for subtree).
    async fn scope_includes_tenant(
        &self,
        scope: &AccessScope,
        tenant: Uuid,
    ) -> Result<bool, DomainError>;
}
