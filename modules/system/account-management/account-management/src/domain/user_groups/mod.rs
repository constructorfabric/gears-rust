//! User Groups feature (DECOMPOSITION §2.6).
//!
//! Delegates all user-group state to the Resource Group module.
//! AM owns two thin touchpoints:
//!
//! 1. Idempotent registration of **two** chained RG type schemas
//!    during module init ([`register_user_group_types`]):
//!
//!    - [`USER_MEMBERSHIP_TYPE`] -- the AM-user member handle. A
//!      type-registry-only entry; AM users live in AM's tables +
//!      `IdP`, never as RG groups. RG needs the row in `gts_type` to
//!      let `add_membership` resolve the resource type.
//!    - [`USER_GROUP_TYPE_CODE`] -- the user-group container. Groups
//!      of this type are AM-owned RG groups (tenant-scoped) whose
//!      `allowed_membership_types` lists [`USER_MEMBERSHIP_TYPE`].
//!
//!    Registration order matters: the member handle MUST land before
//!    the container, otherwise the container's
//!    `resolve_ids(allowed_membership_types)` step fails closed.
//! 2. A [`TenantHardDeleteHook`] that triggers RG-side cascade cleanup
//!    of the tenant's user-group subtree before the `tenants` row is
//!    removed ([`build_cascade_cleanup_hook`]).

pub(crate) mod registration;

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "registration_tests.rs"]
mod registration_tests;

pub(crate) mod cascade;

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "cascade_tests.rs"]
mod cascade_tests;

pub(crate) use cascade::build_cascade_cleanup_hook;
pub(crate) use registration::register_user_group_types;

// Type-code constants live in `account-management-sdk::gts` as the
// single source of truth shared with sibling modules (RBAC, future
// UI gateways) that talk to RG directly. AM re-exports them here
// under the legacy local names so existing call-sites
// (`registration.rs`, `cascade.rs`, fixture builders) keep their
// short imports without scattering `account_management_sdk::gts::`
// paths through the codebase. The legacy names are kept verbatim --
// renaming is a separate refactor.

/// Re-export of [`account_management_sdk::gts::USER_GROUP_RG_TYPE_CODE`]
/// under the legacy AM-internal name `USER_GROUP_TYPE_CODE`. The RG
/// type-registry handle for the AM user-group container.
pub const USER_GROUP_TYPE_CODE: &str = account_management_sdk::gts::USER_GROUP_RG_TYPE_CODE;

/// Re-export of [`account_management_sdk::gts::USER_RG_TYPE_CODE`]
/// under the legacy AM-internal name `USER_MEMBERSHIP_TYPE`. The RG
/// type-registry handle for the AM user member type, used as
/// `resource_type` when adding / removing AM-user memberships in
/// user-groups.
pub(crate) const USER_MEMBERSHIP_TYPE: &str = account_management_sdk::gts::USER_RG_TYPE_CODE;

/// Deterministic subject UUID identifying AM's system actor in
/// cross-module calls (RG type registration, cascade cleanup hook).
///
/// Stable across processes / restarts so downstream audit pipelines on
/// the RG side can correlate every AM-system invocation under one
/// actor identity, distinct from real user actors AND from
/// `Uuid::nil()` (which AM elsewhere treats as "default-constructed
/// actor; service-layer bug"). Hand-picked once with the layout below
/// so the constant is grep-able and never needs to be regenerated.
// Hand-picked stable UUID. Layout: zeros + `cf01` discriminator
// (cyberfabric, AM module = 01) + zeros + 12-hex ASCII `amsystem`
// (`616d 7379 7374` → "amsy st"; first 12 chars of "am.system").
// Stable across releases; never collide with random / v4 / v5 actor
// UUIDs because the version nibble is 0 and the high bits are zero.
const AM_SYSTEM_ACTOR_UUID: uuid::Uuid = uuid::uuid!("00000000-0000-cf01-0000-616d73797374");

/// Build the [`modkit_security::SecurityContext`] that AM passes into
/// cross-module clients (today: `ResourceGroupClient`) for
/// system-initiated calls. `scope_tenant` carries the tenant the work
/// is bound to; pass `None` for module-init paths that have no tenant
/// scope (e.g. RG type schema registration), in which case the
/// platform-root sentinel ([`uuid::Uuid::nil`]) is used.
///
/// Mirrors the `actor=system` label AM uses in its own audit pipeline
/// (`am.events`), but rendered as a proper [`SecurityContext`] so a
/// future RG-side authz tightening that rejects
/// [`SecurityContext::anonymous`] does not regress the cascade hook
/// into permanent `HookError::Retryable`.
///
/// # Panics
///
/// Never panics in practice: both required builder fields
/// (`subject_id` and `subject_tenant_id`) are always set above, so
/// the `build()` call has no failure path that can fire. The
/// `expect` keeps the assertion executable in debug rather than
/// pushing the impossibility upstream as an unreachable `Result`.
#[allow(
    clippy::expect_used,
    reason = "both builder fields are statically set; the expect anchors the impossible-failure invariant"
)]
pub fn am_system_context(scope_tenant: Option<uuid::Uuid>) -> modkit_security::SecurityContext {
    modkit_security::SecurityContext::builder()
        .subject_id(AM_SYSTEM_ACTOR_UUID)
        .subject_type("am.system")
        .subject_tenant_id(scope_tenant.unwrap_or_else(uuid::Uuid::nil))
        .build()
        .expect("AM_SYSTEM_ACTOR_UUID + tenant_id are always present")
}
