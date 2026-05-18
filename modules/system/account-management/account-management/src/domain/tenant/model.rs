//! Tenant domain model types — internal storage / saga shapes.
//!
//! Pure Rust value types that express tenant lifecycle concepts
//! independently of storage and transport. The `SeaORM` entities live in
//! `crate::infra::storage::entity::{tenants, tenant_closure}`. Public
//! input / output DTOs (request bodies, query parameters, response
//! envelopes) live on [`account_management_sdk`] and reuse
//! [`account_management_sdk::Tenant`] (AM-owned projection) /
//! [`account_management_sdk::TenantStatus`] (re-exported from
//! `tenant-resolver-sdk`) on the public boundary. `TenantStatus` /
//! `TenantId` keep being shared cross-SDK; the `Tenant` projection
//! itself is AM-owned because admin tooling needs more than the
//! resolver's identity shape.
//!
//! What stays here:
//! * [`TenantStatus`] — internal 4-variant lifecycle (includes
//!   `Provisioning`, which is never surfaced through the public SDK).
//! * [`TenantModel`] — full storage row (with `tenant_type_uuid`,
//!   `depth`, lifecycle timestamps).
//! * [`NewTenant`] — repo-level insert input.
//! * [`ChildCountFilter`] — repo-level filter for `count_children`.
//! * `validate_*` free functions — domain validation reused by the
//!   service before mutating the DB through the SDK input shapes.

use modkit_macros::domain_model;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

/// Full lifecycle status of a tenant.
///
/// Domain enum: includes the internal `Provisioning` state that is never
/// surfaced through the public SDK / REST API. The storage layer encodes
/// these as `SMALLINT` per `m0001_initial_schema` using the mapping:
/// `0=Provisioning, 1=Active, 2=Suspended, 3=Deleted`.
// @cpt-begin:cpt-cf-account-management-state-tenant-hierarchy-management-tenant-status:p1:inst-state-tenant-status-domain
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum TenantStatus {
    Provisioning,
    Active,
    Suspended,
    Deleted,
}
// @cpt-end:cpt-cf-account-management-state-tenant-hierarchy-management-tenant-status:p1:inst-state-tenant-status-domain

impl TenantStatus {
    /// Numeric SMALLINT encoding used by the DB schema.
    #[must_use]
    pub const fn as_smallint(self) -> i16 {
        match self {
            Self::Provisioning => 0,
            Self::Active => 1,
            Self::Suspended => 2,
            Self::Deleted => 3,
        }
    }

    /// Whether the status is visible to the public API surface (`list`,
    /// `read`, status-filter). Provisioning tenants are SDK-invisible.
    #[must_use]
    pub const fn is_sdk_visible(self) -> bool {
        !matches!(self, Self::Provisioning)
    }

    /// Stable lowercase label used in audit payloads and structured
    /// logs. Pinned here (rather than as `Display` or via `serde`) so
    /// the wire shape is independent of any future derive changes.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Provisioning => "provisioning",
            Self::Active => "active",
            Self::Suspended => "suspended",
            Self::Deleted => "deleted",
        }
    }

    /// Parse from SMALLINT. Returns `None` for any value outside the
    /// documented `{0, 1, 2, 3}` domain.
    #[must_use]
    pub const fn from_smallint(value: i16) -> Option<Self> {
        match value {
            0 => Some(Self::Provisioning),
            1 => Some(Self::Active),
            2 => Some(Self::Suspended),
            3 => Some(Self::Deleted),
            _ => None,
        }
    }
}

/// Snapshot of a tenant row as returned by `TenantRepo`.
///
/// Matches the column set declared by `m0001_initial_schema`,
/// including `deleted_at`.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TenantModel {
    pub id: Uuid,
    pub parent_id: Option<Uuid>,
    pub name: String,
    pub status: TenantStatus,
    pub self_managed: bool,
    pub tenant_type_uuid: Uuid,
    /// Depth from the root. Root has depth `0`; each step adds `1`.
    pub depth: u32,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
    /// Soft-delete tombstone. `Some(_)` exactly when `status ==
    /// TenantStatus::Deleted`; also serves as the retention-timer start
    /// (hard-delete becomes eligible at `deleted_at + retention_window`).
    pub deleted_at: Option<OffsetDateTime>,
}

// NOTE: Phase 3 adds the `retention_window_secs` column to the
// `tenants` entity — see `src/infra/storage/entity/tenants.rs`. It
// stays off `TenantModel`: it is an operator-set per-tenant override
// of the module-default retention window, used by the sweep planner,
// and is surfaced through the [`retention::TenantRetentionRow`]
// selection row rather than the happy-path read shape.

/// Validated fields used to create a tenant's initial row (before the
/// `IdP` provisioning step of the create-tenant saga).
///
/// `parent_id` is `None` exclusively for the platform root tenant (the
/// row inserted by `BootstrapService`); every other tenant is a child
/// of an existing active tenant and carries `Some(parent_id)`. This
/// matches the migration's `ck_tenants_root_depth` invariant
/// (`(parent_id IS NULL AND depth = 0) OR (parent_id IS NOT NULL AND
/// depth > 0)`) and the FK on `tenants.parent_id`.
#[domain_model]
#[derive(Debug, Clone)]
pub struct NewTenant {
    pub id: Uuid,
    pub parent_id: Option<Uuid>,
    pub name: String,
    pub self_managed: bool,
    pub tenant_type_uuid: Uuid,
    pub depth: u32,
}

// `validate_tenant_name` was a synchronous, hardcoded `[1, 255]`
// length check that duplicated the published `gts.cf.core.am.tenant.v1~`
// schema. The runtime validation has moved to
// [`crate::domain::gts_validation::validate_tenant_name_via_gts`],
// which fetches the schema from the Types Registry at the call site
// and runs `jsonschema::validator_for` (mirroring the
// `cf-resource-group::validate_metadata_via_gts` pattern). Per
// `validate_metadata_via_gts` semantics, a missing schema short-
// circuits to `Ok(())` and the database `CHECK (length(name) BETWEEN
// 1 AND 255)` constraint serves as the last-line guard for the
// underlying column.

/// Lift a SDK [`account_management_sdk::TenantStatus`] (3-variant,
/// public surface) into the AM-internal 4-variant
/// [`TenantStatus`]. Infallible because the SDK type cannot represent
/// `Provisioning`.
impl From<account_management_sdk::TenantStatus> for TenantStatus {
    fn from(s: account_management_sdk::TenantStatus) -> Self {
        match s {
            account_management_sdk::TenantStatus::Active => Self::Active,
            account_management_sdk::TenantStatus::Suspended => Self::Suspended,
            account_management_sdk::TenantStatus::Deleted => Self::Deleted,
        }
    }
}

/// Error returned by [`TryFrom<TenantStatus>`] for
/// [`account_management_sdk::TenantStatus`] when the input is
/// `Provisioning` — the internal-only variant that has no public
/// SDK representation. Service-layer call sites filter Provisioning
/// rows via [`TenantStatus::is_sdk_visible`] before lowering, so
/// reaching this error means the filter was bypassed (programmer
/// error). The service layer maps it to `DomainError::Internal`
/// so the bypass surfaces as HTTP 500 rather than a process panic.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProvisioningNotPublic;

impl core::fmt::Display for ProvisioningNotPublic {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(
            "TenantStatus::Provisioning has no public SDK representation; \
             callers must filter via is_sdk_visible before lowering",
        )
    }
}

impl core::error::Error for ProvisioningNotPublic {}

/// Lower the AM-internal 4-variant [`TenantStatus`] into the SDK
/// 3-variant [`account_management_sdk::TenantStatus`].
///
/// # Errors
///
/// Returns [`ProvisioningNotPublic`] on [`TenantStatus::Provisioning`].
/// The Rust convention is that `From` is infallible; expressing the
/// public-vs-internal-vocabulary split via `TryFrom` makes the
/// invariant type-safe and replaces the previous `unreachable!()`
/// panic with a propagatable error if the upstream `is_sdk_visible`
/// filter is ever bypassed.
impl TryFrom<TenantStatus> for account_management_sdk::TenantStatus {
    type Error = ProvisioningNotPublic;

    fn try_from(s: TenantStatus) -> Result<Self, Self::Error> {
        match s {
            TenantStatus::Active => Ok(Self::Active),
            TenantStatus::Suspended => Ok(Self::Suspended),
            TenantStatus::Deleted => Ok(Self::Deleted),
            TenantStatus::Provisioning => Err(ProvisioningNotPublic),
        }
    }
}

/// Filter consumed by `TenantRepo::count_children`.
///
/// `Provisioning` rows are *always* counted; the variants only differ in
/// how they treat `Deleted` rows. The split exists because the two
/// service-layer call sites have different semantics:
///
/// * Soft-delete preconditions ask "are there any **non-deleted**
///   children?" → [`ChildCountFilter::NonDeleted`].
/// * The hard-delete leaf-first guard asks "is *anything* still under
///   this tenant — even soft-deleted descendants the reaper hasn't
///   collected yet?" → [`ChildCountFilter::All`].
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ChildCountFilter {
    /// Counts `Provisioning + Active + Suspended` (excludes `Deleted`
    /// only).
    NonDeleted,
    /// Counts every status, including `Deleted`.
    All,
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "model_tests.rs"]
mod model_tests;
