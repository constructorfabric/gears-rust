//! User Groups feature (DECOMPOSITION §2.6).
//!
//! Delegates all user-group state to the Resource Group gear.
//! AM owns two thin touchpoints:
//!
//! 1. Two AM-owned RG types, declared in [`crate::gts`] and published
//!    with AM's manifest before `init` (resource-group reads them from the
//!    types registry):
//!
//!    - [`USER_RG_TYPE_CODE`](account_management_sdk::gts::USER_RG_TYPE_CODE) -- the AM-user member handle. AM users
//!      live in AM's tables + `IdP`, never as RG groups; the handle is the
//!      `resource_type` of their memberships.
//!    - [`USER_GROUP_TYPE_CODE`] -- the user-group container. Groups
//!      of this type are AM-owned RG groups (tenant-scoped) whose
//!      `allowed_membership_types` lists [`USER_RG_TYPE_CODE`](account_management_sdk::gts::USER_RG_TYPE_CODE).
//! 2. A [`TenantHardDeleteHook`] that triggers RG-side cascade cleanup
//!    of the tenant's user-group subtree before the `tenants` row is
//!    removed ([`build_cascade_cleanup_hook`]).

pub(crate) mod cascade;

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "cascade_tests.rs"]
mod cascade_tests;

pub(crate) use cascade::build_cascade_cleanup_hook;

/// Re-export of [`account_management_sdk::gts::USER_GROUP_RG_TYPE_CODE`]
/// under the legacy AM-internal name `USER_GROUP_TYPE_CODE`. The RG
/// type identifier for the AM user-group container in Types Registry.
pub const USER_GROUP_TYPE_CODE: &str = account_management_sdk::gts::USER_GROUP_RG_TYPE_CODE;

// System-actor factories live in [`crate::domain::system_actor`] and
// are imported directly at each call site (no crate-root convenience
// re-export). Routing every legitimate use case through a named
// factory makes "where does AM elevate to system?" trivially
// auditable by grep, and the per-site `tracing` log emitted by each
// factory gives the audit pipeline a wire signal. See the
// `system_actor` gear docs for the full rationale.
