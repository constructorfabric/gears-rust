use credstore_sdk::{OwnerId, SecretRef, SharingMode, TenantId, WriteOptions};
use time::OffsetDateTime;
use toolkit_macros::domain_model;
use uuid::Uuid;

#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecretStatus {
    Provisioning,
    Active,
    /// Delete saga in flight: invisible to resolution, still holds the
    /// partial unique index until backend cleanup completes.
    Deprovisioning,
}
impl SecretStatus {
    #[must_use]
    pub fn as_smallint(self) -> i16 {
        match self {
            Self::Provisioning => 1,
            Self::Active => 2,
            Self::Deprovisioning => 3,
        }
    }
    #[must_use]
    pub fn from_smallint(v: i16) -> Option<Self> {
        match v {
            1 => Some(Self::Provisioning),
            2 => Some(Self::Active),
            3 => Some(Self::Deprovisioning),
            _ => None,
        }
    }
}

#[domain_model]
#[derive(Debug, Clone)]
pub struct SecretRow {
    pub id: Uuid,
    pub tenant_id: TenantId,
    pub reference: String,
    pub sharing: SharingMode,
    pub owner_id: OwnerId,
    pub status: SecretStatus,
    /// Monotonic version (optimistic-locking groundwork); 1 on create.
    pub version: i64,
    /// Deterministic v5 UUID of the secret's GTS type id (the stored
    /// representation); immutable for the row's lifetime. Resolved to the
    /// type id + traits via the types-registry per operation.
    pub secret_type_uuid: Uuid,
    /// Expiry instant for expirable types; expired rows do not resolve.
    pub expires_at: Option<OffsetDateTime>,
    /// Value-fingerprint fence (`HMAC-SHA256(fence_key, value)`) binding this
    /// row's metadata to the backend value it was written for. `None` only
    /// for out-of-band seeded rows, which are served on trust and backfilled
    /// on first read / reaper sweep. Internal-only: never serialized to any
    /// API response or log.
    pub value_fp: Option<Vec<u8>>,
    /// Fence-key id `value_fp` was computed under; `None` iff `value_fp` is.
    pub fp_key_id: Option<i16>,
}

/// Everything a write needs beyond identity and value: sharing mode,
/// create-only vs upsert, optimistic-concurrency precondition, and typed
/// options (secret type + expiry).
#[domain_model]
#[derive(Debug, Clone, Default)]
pub struct WriteSpec {
    pub sharing: SharingMode,
    pub create_only: bool,
    pub precondition: Option<WritePrecondition>,
    pub opts: WriteOptions,
    /// When overwriting an existing secret, keep the stored sharing mode
    /// instead of applying `sharing`. Set for a PUT that omitted `sharing`, so
    /// a value rotation (`{"value": "..."}`) never silently narrows a `shared`
    /// secret back to `tenant`. `sharing` is still the class/create default.
    pub preserve_sharing: bool,
}

impl WriteSpec {
    /// Upsert with default options.
    #[must_use]
    pub fn upsert(sharing: SharingMode) -> Self {
        Self {
            sharing,
            ..Self::default()
        }
    }

    /// Create-only (409 on same-class duplicate) with default options.
    #[must_use]
    pub fn create(sharing: SharingMode) -> Self {
        Self {
            sharing,
            create_only: true,
            ..Self::default()
        }
    }

    /// Attach an `If-Match` precondition.
    #[must_use]
    pub fn with_precondition(mut self, precondition: Option<WritePrecondition>) -> Self {
        self.precondition = precondition;
        self
    }

    /// Attach typed write options (secret type, expiry).
    #[must_use]
    pub fn with_opts(mut self, opts: WriteOptions) -> Self {
        self.opts = opts;
        self
    }

    /// Preserve the existing secret's sharing on overwrite (see field docs).
    #[must_use]
    pub fn preserve_sharing(mut self, preserve: bool) -> Self {
        self.preserve_sharing = preserve;
        self
    }
}

/// Optimistic-concurrency precondition for a write, parsed from `If-Match`.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WritePrecondition {
    /// `If-Match: *` — the target secret must already exist.
    Exists,
    /// `If-Match: "<id>.<version>"` — the generation-bound strong validator.
    /// `id` is the row UUID (a fresh one per recreated secret), so a
    /// validator from a deleted-and-recreated secret's earlier generation can
    /// never match the current row even when the version counters coincide
    /// (no ABA); `version` is the per-row monotonic counter.
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
    /// Fence fingerprint of the value this create will write to the backend
    /// (API creates always stamp; only out-of-band seeding leaves it NULL).
    pub value_fp: Vec<u8>,
    /// Fence-key id `value_fp` was computed under.
    pub fp_key_id: i16,
}

#[cfg(test)]
mod tests {
    use super::SecretStatus;

    #[test]
    fn secret_status_smallint_round_trips() {
        for s in [
            SecretStatus::Provisioning,
            SecretStatus::Active,
            SecretStatus::Deprovisioning,
        ] {
            assert_eq!(SecretStatus::from_smallint(s.as_smallint()), Some(s));
        }
        assert_eq!(SecretStatus::Provisioning.as_smallint(), 1);
        assert_eq!(SecretStatus::Active.as_smallint(), 2);
        assert_eq!(SecretStatus::Deprovisioning.as_smallint(), 3);
    }

    #[test]
    fn secret_status_from_smallint_rejects_out_of_domain() {
        assert_eq!(SecretStatus::from_smallint(0), None);
        assert_eq!(SecretStatus::from_smallint(4), None);
        assert_eq!(SecretStatus::from_smallint(-1), None);
    }
}
