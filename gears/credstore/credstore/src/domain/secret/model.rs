//! Domain models for secret metadata and write concurrency (ADR-0006:
//! immutable value versions).
//!
//! Models the two resting statuses (`active`/`declared`), the fallback
//! policy, the value-version pointer and optimistic preconditions, all
//! persisted separately from secret values.

use credstore_sdk::{
    DestroySelector, OwnerId, SecretRef, SharingMode, StoreKey, TenantId, ValueVersion,
};
use time::OffsetDateTime;
use toolkit_macros::domain_model;
use uuid::Uuid;

use crate::domain::ports::metrics::CleanupOp;

/// A row's lifecycle status. `CHECK (status IN (2, 4))` at the storage layer
/// admits only these two; codes `1` (`provisioning`) and `3`
/// (`deprovisioning`) are retired by ADR-0006 and never reassigned — a stray
/// reference to either in an old dashboard or log line stays unambiguous.
///
/// There is no in-flight status: a write is a value-and-pointer switch inside
/// one database transaction, not a sequence of externally observable saga
/// steps, so nothing observable precedes the commit that makes a row
/// consistent.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecretStatus {
    /// The row resolves and points at a value (`value_version IS NOT NULL`).
    Active,
    /// The row holds its reference but carries no value (`value_version IS
    /// NULL`), reached only via a value-removal write (`PATCH {"secret":
    /// null}`, or a `PATCH` that suppresses an active row in the same
    /// transaction). A resolution candidate only when its `fallback` is
    /// `None` (ADR-0004, Suppression): `status = 2 OR (status = 4 AND
    /// fallback = 2)`.
    Declared,
}
impl SecretStatus {
    #[must_use]
    pub fn as_smallint(self) -> i16 {
        match self {
            Self::Active => 2,
            Self::Declared => 4,
        }
    }

    /// Decode a stored status code. `1`/`3` (retired) and any other value are
    /// out-of-domain — a storage corruption, not a reachable application
    /// state — so this returns `None` for the caller to map onto
    /// [`crate::domain::error::DomainError::Internal`], never a panic.
    #[must_use]
    pub fn from_smallint(v: i16) -> Option<Self> {
        match v {
            2 => Some(Self::Active),
            4 => Some(Self::Declared),
            _ => None,
        }
    }
}

/// Suppression policy for a `declared` row (ADR-0004): a record's policy for
/// the time it holds no value. `write` sets it (`PUT`, or `PATCH
/// {"fallback": …}`); it is stored on every row (both `Active` and
/// `Declared`) and consulted only while the row is `Declared`.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fallback {
    /// Resolution keeps walking up the tenant chain past this row (default).
    Inherit,
    /// This row blocks resolution outright when it is the nearest candidate.
    None,
}
impl Fallback {
    #[must_use]
    pub fn as_smallint(self) -> i16 {
        match self {
            Self::Inherit => 1,
            Self::None => 2,
        }
    }

    /// Decode a stored fallback code; out-of-domain values are storage
    /// corruption, mapped by the caller onto `DomainError::Internal`.
    #[must_use]
    pub fn from_smallint(v: i16) -> Option<Self> {
        match v {
            1 => Some(Self::Inherit),
            2 => Some(Self::None),
            _ => None,
        }
    }
}

impl From<credstore_sdk::Fallback> for Fallback {
    fn from(f: credstore_sdk::Fallback) -> Self {
        match f {
            credstore_sdk::Fallback::Inherit => Self::Inherit,
            credstore_sdk::Fallback::None => Self::None,
        }
    }
}

impl From<Fallback> for credstore_sdk::Fallback {
    fn from(f: Fallback) -> Self {
        match f {
            Fallback::Inherit => Self::Inherit,
            Fallback::None => Self::None,
        }
    }
}

#[domain_model]
#[derive(Debug, Clone)]
pub struct SecretRow {
    /// Record identity, minted at create and never reused; also the record
    /// part of the store key.
    pub id: Uuid,
    pub tenant_id: TenantId,
    pub reference: String,
    pub sharing: SharingMode,
    pub owner_id: OwnerId,
    pub status: SecretStatus,
    /// Monotonic version (optimistic-locking); 1 on create, bumped by every
    /// successful value switch or metadata update.
    pub version: i64,
    /// Last-write instant; bumped alongside `version` by every successful
    /// value switch or metadata update. Surfaced on `Credential` for the
    /// caller's own row only (ADR-0004).
    pub updated_at: OffsetDateTime,
    /// Deterministic v5 UUID of the secret's GTS type id (the stored
    /// representation); immutable for the row's lifetime. Resolved to the
    /// type id + traits via the types-registry per operation.
    pub secret_type_uuid: Uuid,
    /// Expiry instant for expirable types. Expiry applies to the secret, not
    /// to the record: an expired `active` row still resolves (it is the
    /// decisive record) but its secret is never served.
    pub expires_at: Option<OffsetDateTime>,
    /// The value version the provider returned from `put` for the secret
    /// this row serves (the pointer into the store). `None` iff `status =
    /// Declared` (`credstore_secrets_value_version_check`). Opaque: never
    /// parsed or compared by the gear, and distinct from [`Self::version`],
    /// the row's optimistic-concurrency counter.
    pub value_version: Option<ValueVersion>,
    /// Suppression policy of this row (ADR-0004): consulted only while the
    /// row is `Declared` — an `Active` row's own value always wins,
    /// `fallback` stays stored but not consulted.
    pub fallback: Fallback,
}

impl SecretRow {
    /// `true` iff this is an `active` row whose `expires_at` has passed at
    /// `now`. A `declared` row never expires (a suppression policy does not).
    /// Derived at read time; never stored.
    #[must_use]
    pub fn is_expired(&self, now: OffsetDateTime) -> bool {
        self.status == SecretStatus::Active && self.expires_at.is_some_and(|at| at <= now)
    }
}

/// Optimistic-concurrency precondition for `patch`/`delete`, parsed from
/// `If-Match`. `put` uses the distinct [`PutPrecondition`], which additionally
/// carries the create-only intent (`If-None-Match: *`).
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WritePrecondition {
    /// `If-Match: *` — the target credential must already exist.
    Exists,
    /// `If-Match: "<id>.<version>"` — the generation-bound strong validator.
    /// `id` is the row UUID (a fresh one per recreated credential), so a
    /// validator from a deleted-and-recreated credential's earlier generation
    /// can never match the current row even when the version counters
    /// coincide (no ABA); `version` is the per-row monotonic counter.
    Version {
        /// Row (generation) UUID the caller's validator was minted for.
        id: Uuid,
        /// Version counter the caller last observed.
        version: i64,
    },
    /// `If-Match: "<id>.<v>", "<id2>.<v2>", …` — a multi-valued list (RFC 7232
    /// §3.1). The precondition is satisfied if the current row matches **any**
    /// listed `(id, version)` validator.
    AnyVersion(Vec<(Uuid, i64)>),
}

/// Precondition for `put` (ADR-0004, "Two write verbs on one resource"):
/// distinguishes create-only from a guarded or unconditional replace, parsed
/// from `If-None-Match`/`If-Match`.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PutPrecondition {
    /// `If-None-Match: *` — create-only; `Conflict` if the caller's own
    /// tenant already holds a row under the reference (of any status —
    /// `declared` counts as "holds a row" too).
    CreateOnly,
    /// `If-Match: *` — replace, last-writer-wins; `Conflict` (mapped to
    /// [`crate::domain::error::DomainError::VersionConflict`] by the caller,
    /// mirroring `WritePrecondition::Exists`) if no own row exists.
    Exists,
    /// `If-Match: "<id>.<version>"` — guarded replace.
    Version {
        /// Row (generation) UUID the caller's validator was minted for.
        id: Uuid,
        /// Version counter the caller last observed.
        version: i64,
    },
    /// `If-Match: "<id>.<v>", "<id2>.<v2>", …` — a multi-valued list (RFC 7232
    /// §3.1); satisfied if the current row matches **any** listed validator.
    AnyVersion(Vec<(Uuid, i64)>),
}

/// A new active row: a create always inserts `active`, pointing at the
/// value version its `plugin.put` already returned.
#[domain_model]
#[derive(Debug, Clone)]
pub struct NewSecret {
    pub id: Uuid,
    pub tenant_id: TenantId,
    pub reference: SecretRef,
    pub sharing: SharingMode,
    pub owner_id: OwnerId,
    /// Deterministic v5 UUID of the (registry-validated) GTS type id.
    pub secret_type_uuid: Uuid,
    pub expires_at: Option<OffsetDateTime>,
    /// The value version `plugin.put` returned for this create's value.
    pub value_version: ValueVersion,
    /// Suppression policy carried into the row at create time (ADR-0004);
    /// `PUT`'s default is [`Fallback::Inherit`] when the body omits it.
    pub fallback: Fallback,
}

/// A new declared row (ADR-0004 Amendment B, "The value-less record: reached
/// only on purpose"): a create whose `value` is an explicit `null` inserts
/// the row `declared` directly — no `value_version`, no backend call. Distinct from [`NewSecret`], which always carries a
/// written value.
#[domain_model]
#[derive(Debug, Clone)]
pub struct NewDeclaredSecret {
    pub id: Uuid,
    pub tenant_id: TenantId,
    pub reference: SecretRef,
    pub sharing: SharingMode,
    pub owner_id: OwnerId,
    /// Deterministic v5 UUID of the (registry-validated) GTS type id.
    pub secret_type_uuid: Uuid,
    pub expires_at: Option<OffsetDateTime>,
    /// Suppression policy carried into the row at create time (ADR-0004).
    pub fallback: Fallback,
}

/// One secret-write attempt, as the repository needs to know it: the
/// identity of its write intent (`credstore_write_intents` row; inserted by
/// tx0 before `plugin.put`, deleted by tx1 in the transaction that commits
/// or definitively loses the write), the store key the attempt `put`s under,
/// and whether the selected plugin supports `destroy` (destroy tasks are
/// enqueued only then; purges always).
#[domain_model]
#[derive(Debug, Clone)]
pub struct WriteAttempt {
    /// Minted per write attempt (v4), never reused: a retried write gets a
    /// new one.
    pub attempt_id: Uuid,
    /// The store key the attempt `put`s under.
    pub key: StoreKey,
    pub destroy_supported: bool,
}

/// A store-cleanup task a repository transaction enqueued in the platform
/// transactional outbox, next to the row change or intent deletion that
/// learned it was needed. Executed by the outbox handler, never inline.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CleanupTask {
    /// `plugin.delete_key(key)`: the key will never hold a live value
    /// (record deleted, or the attempt's fresh key lost its row).
    Purge(StoreKey),
    /// `plugin.destroy(key, selector)`; enqueued only for destroy-capable
    /// plugins.
    Destroy {
        key: StoreKey,
        selector: DestroySelector,
    },
}

impl CleanupTask {
    /// The metric label of this task.
    #[must_use]
    pub fn op(&self) -> CleanupOp {
        match self {
            Self::Purge(_) => CleanupOp::Purge,
            Self::Destroy { .. } => CleanupOp::Destroy,
        }
    }

    /// The store key the task acts on.
    #[must_use]
    pub fn key(&self) -> &StoreKey {
        match self {
            Self::Purge(key) | Self::Destroy { key, .. } => key,
        }
    }
}

/// How a secret write's commit transaction (tx1) ended. Everything but an
/// error is a definite outcome: the intent is retired in the same
/// transaction as the row change, and every cleanup task listed was enqueued
/// in that same transaction.
#[domain_model]
#[derive(Debug)]
pub enum IntentCommit<T> {
    /// The row change committed (`value`: the post-write row, if the method
    /// returns one); the intent is gone.
    Committed {
        value: T,
        enqueued: Vec<CleanupTask>,
    },
    /// The row change lost definitively (the create hit the reference's
    /// unique index, or the compare-and-set matched no row). The intent
    /// deletion committed anyway, together with the cleanup of this
    /// attempt's version.
    Lost { enqueued: Vec<CleanupTask> },
    /// The intent had already been reclaimed: the transaction changed
    /// nothing. The writer is alive and knows its version, so it settles it
    /// itself ([`crate::domain::secret::repo::SecretRepo::settle_lost_intent`]).
    IntentLost,
}

/// Result of one [`crate::domain::secret::repo::SecretRepo::reclaim_expired`]
/// pass.
#[domain_model]
#[derive(Debug, Default)]
pub struct Reclaimed {
    /// Expired intents deleted.
    pub intents: u64,
    /// The purges enqueued for those whose record has no row.
    pub enqueued: Vec<CleanupTask>,
}

impl SecretRow {
    /// The store key of this record: `(tenant_id, id)`.
    #[must_use]
    pub fn store_key(&self) -> StoreKey {
        StoreKey::new(self.tenant_id, self.id)
    }
}

#[cfg(test)]
mod tests {
    use super::{Fallback, SecretStatus};

    #[test]
    fn secret_status_smallint_round_trips() {
        for s in [SecretStatus::Active, SecretStatus::Declared] {
            assert_eq!(SecretStatus::from_smallint(s.as_smallint()), Some(s));
        }
        assert_eq!(SecretStatus::Active.as_smallint(), 2);
        assert_eq!(SecretStatus::Declared.as_smallint(), 4);
    }

    #[test]
    fn secret_status_from_smallint_rejects_retired_and_out_of_domain_codes() {
        // 1 (provisioning) and 3 (deprovisioning) are retired by ADR-0006:
        // reserved, never reassigned, never stored again.
        assert_eq!(SecretStatus::from_smallint(1), None);
        assert_eq!(SecretStatus::from_smallint(3), None);
        assert_eq!(SecretStatus::from_smallint(0), None);
        assert_eq!(SecretStatus::from_smallint(5), None);
        assert_eq!(SecretStatus::from_smallint(-1), None);
    }

    #[test]
    fn fallback_smallint_round_trips() {
        for f in [Fallback::Inherit, Fallback::None] {
            assert_eq!(Fallback::from_smallint(f.as_smallint()), Some(f));
        }
        assert_eq!(Fallback::Inherit.as_smallint(), 1);
        assert_eq!(Fallback::None.as_smallint(), 2);
    }

    #[test]
    fn fallback_from_smallint_rejects_out_of_domain() {
        assert_eq!(Fallback::from_smallint(0), None);
        assert_eq!(Fallback::from_smallint(3), None);
    }
}
