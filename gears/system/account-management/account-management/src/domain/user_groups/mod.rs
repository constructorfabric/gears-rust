//! User Groups feature (DECOMPOSITION §2.6).
//!
//! Delegates all user-group state to the Resource Group gear.
//! AM owns two thin touchpoints:
//!
//! 1. Group and membership schemas declared in [`crate::gts`] and published
//!    with AM's manifest before `init`. The group type
//!    [`USER_GROUP_TYPE_CODE`] binds the schema that accepts the AM
//!    [user resource type](account_management_sdk::gts::USER_RESOURCE_TYPE)
//!    and its derived types.
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
