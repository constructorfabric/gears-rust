//! AM-internal "system actor" `SecurityContext` factories.
//!
//! Background AM flows have no end-user `SecurityContext` to forward
//! but still need to call into cross-gear clients (Resource Group,
//! `IdP` plugin) and the platform's `AuthZ` resolver. This gear
//! mints the stable, audit-correlatable identity AM uses on those
//! paths: every system call carries `subject_id =
//! AM_SYSTEM_ACTOR_UUID` and `subject_type = "am.system"`, mirroring
//! the `actor=system` label AM emits in its own `am.events` audit
//! pipeline.
//!
//! # Why site-specific factories instead of one constructor
//!
//! The naïve shape — one `am_system_context(scope_tenant)` exposed
//! crate-wide — turns "system actor" into a casual primitive any
//! future contributor can reach for to bypass the request-scoped
//! `SecurityContext` contract (e.g. a REST handler that wants to
//! "just check existence" without plumbing the caller context).
//! Cross-tenant leak regressions in adjacent monorepos have started
//! exactly there.
//!
//! Routing every legitimate call through a named factory tightens the
//! seam two ways:
//!
//! 1. **Visibility audit.** The factory name is the contract: a grep
//!    for `for_retention_sweep` / `for_provisioning_reaper` / … shows
//!    every legitimate use case in O(filename) and makes "where does
//!    AM elevate to system?" trivially searchable. A new use case
//!    must add a new factory here, which is a review-magnet.
//! 2. **Construction-site observability.** Each factory emits a
//!    `tracing::info!(target: "am.system_actor", site = …, …)` line
//!    on every call. A future audit pipeline can subscribe to that
//!    target and reconcile every system-actor invocation against the
//!    legitimate-sites list; without per-site logging there is no
//!    wire signal to audit.
//!
//! Visibility is `pub(crate)` so the impl-crate boundary keeps this
//! out of the `account-management-sdk` surface; external consumers
//! drive AM through the [`AccountManagementClient`] trait and never
//! mint a system context themselves.
//!
//! # Today's call sites
//!
//! * [`for_gear_init`] — RG type-schema registration from
//!   `gear::init`. Platform-scoped (no tenant binding).
//! * [`for_bootstrap`] — `provision_tenant` / compensation
//!   `deprovision_tenant` on the platform root inside the bootstrap
//!   saga and its step-3 compensator.
//! * [`for_provisioning_reaper`] — `deprovision_tenant` calls from
//!   the provisioning-reaper batch on rows stuck in retry.
//! * [`for_retention_sweep`] — `deprovision_tenant` from the
//!   hard-delete retention sweep on rows past their retention window.
//! * [`for_user_groups_cascade`] — RG cascade-cleanup hook fired
//!   when a tenant is hard-deleted.
//!
//! `delete_user`'s RG membership cleanup used to mint a `for_user_cleanup`
//! system actor here; VHP-190 moved it to run under the caller's context
//! (it is a caller-initiated flow), so no system actor is needed for it.

use toolkit_security::SecurityContext;
use uuid::Uuid;

/// Hand-picked actor UUID, version-nibble = 0 so it cannot collide
/// with any v4/v5 actor UUID. Stable across processes so audit sinks
/// can correlate AM-system invocations under one identity.
pub(crate) const AM_SYSTEM_ACTOR_UUID: Uuid = uuid::uuid!("00000000-0000-cf01-0000-616d73797374");

/// `subject_type` AM stamps on every system-actor context. Plugins /
/// `AuthZ` policies MAY key on this string to route system traffic
/// through a separate credstore path (e.g. AM service credentials
/// vs. tenant/user credentials).
const AM_SYSTEM_SUBJECT_TYPE: &str = "am.system";

/// Internal builder shared by every factory. Centralises the
/// `SecurityContext::builder()` shape so a future change to the
/// system-actor envelope (extra claim, attestation field) lands once.
///
/// `scope_tenant = None` falls back to the platform-root sentinel
/// ([`Uuid::nil`]) for platform-scoped flows (gear init, etc.) —
/// the same fallback the prior monolithic constructor used.
///
/// # Panics
///
/// Never in practice: both required builder fields are set
/// unconditionally below.
#[allow(
    clippy::expect_used,
    reason = "both builder fields are statically set; the expect anchors the impossible-failure invariant"
)]
fn build_inner(scope_tenant: Option<Uuid>) -> SecurityContext {
    SecurityContext::builder()
        .subject_id(AM_SYSTEM_ACTOR_UUID)
        .subject_type(AM_SYSTEM_SUBJECT_TYPE)
        .subject_tenant_id(scope_tenant.unwrap_or_else(Uuid::nil))
        .build()
        .expect("AM_SYSTEM_ACTOR_UUID + tenant_id are always present")
}

/// gear init — RG type-schema registration. Platform-scoped (no
/// tenant binding; the registration is a workspace-wide operation
/// that pre-dates any tenant).
#[must_use]
pub(crate) fn for_gear_init() -> SecurityContext {
    tracing::info!(
        target: "am.system_actor",
        site = "gear_init",
        "am system actor constructed",
    );
    build_inner(None)
}

/// Bootstrap saga — `provision_tenant` on the platform root and its
/// compensation `deprovision_tenant` paths (steps 2 and 3 of the
/// saga). `root_id` is the platform-root tenant id.
#[must_use]
pub(crate) fn for_bootstrap(root_id: Uuid) -> SecurityContext {
    tracing::info!(
        target: "am.system_actor",
        site = "bootstrap",
        tenant_id = %root_id,
        "am system actor constructed",
    );
    build_inner(Some(root_id))
}

/// Provisioning-reaper batch — `deprovision_tenant` on a row stuck in
/// `Provisioning` past its retry budget.
#[must_use]
pub(crate) fn for_provisioning_reaper(tenant_id: Uuid) -> SecurityContext {
    tracing::info!(
        target: "am.system_actor",
        site = "provisioning_reaper",
        tenant_id = %tenant_id,
        "am system actor constructed",
    );
    build_inner(Some(tenant_id))
}

/// Hard-delete retention sweep — `deprovision_tenant` on a
/// soft-deleted row that has aged past its retention window.
#[must_use]
pub(crate) fn for_retention_sweep(tenant_id: Uuid) -> SecurityContext {
    tracing::info!(
        target: "am.system_actor",
        site = "retention_sweep",
        tenant_id = %tenant_id,
        "am system actor constructed",
    );
    build_inner(Some(tenant_id))
}

/// User-groups cascade-cleanup hook — fired when a tenant is
/// hard-deleted and AM must walk its user-group memberships in RG.
/// `tenant_id` is the tenant being deleted; it rides the audit log
/// line only.
///
/// Platform-scoped, NOT scoped to `tenant_id`: the subject
/// tenant here is by definition a `Deleted` row mid-purge, and the
/// `AuthZ` resolver's system-actor grant would root a tenant-subtree
/// scope at it — deleted roots are clamped out of scope
/// materialization, the subtree resolves to zero tenants, and every
/// RG cascade call fails closed. The reaper then defers the whole
/// backlog forever. Platform scope (nil tenant → Global grant) is
/// honest for this flow: the cascade addresses explicit group ids of
/// a tenant that is being erased, not a live tenant's data.
#[must_use]
pub(crate) fn for_user_groups_cascade(tenant_id: Uuid) -> SecurityContext {
    tracing::info!(
        target: "am.system_actor",
        site = "user_groups_cascade",
        tenant_id = %tenant_id,
        "am system actor constructed",
    );
    build_inner(None)
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    //! Pin the shared invariants every factory MUST satisfy:
    //!
    //! * `subject_id` is the stable [`AM_SYSTEM_ACTOR_UUID`].
    //! * `subject_type` is `"am.system"` (plugins key on it).
    //! * Tenant-bound factories carry the supplied tenant id; the
    //!   platform-scoped factory uses [`Uuid::nil`].
    //!
    //! A future contributor adding a fifth tenant-bound factory MUST
    //! extend this test block — otherwise the new path could drift on
    //! one of these three properties without trial-firing.
    use super::*;

    #[test]
    fn gear_init_uses_nil_tenant() {
        let ctx = for_gear_init();
        assert_eq!(ctx.subject_id(), AM_SYSTEM_ACTOR_UUID);
        assert_eq!(ctx.subject_type(), Some(AM_SYSTEM_SUBJECT_TYPE));
        assert_eq!(ctx.subject_tenant_id(), Uuid::nil());
    }

    #[test]
    fn tenant_bound_factories_carry_supplied_tenant() {
        let tenant = Uuid::from_u128(0xDEAD_BEEF_FACE_CAFE);
        for (label, ctx) in [
            ("bootstrap", for_bootstrap(tenant)),
            ("provisioning_reaper", for_provisioning_reaper(tenant)),
            ("retention_sweep", for_retention_sweep(tenant)),
        ] {
            assert_eq!(
                ctx.subject_id(),
                AM_SYSTEM_ACTOR_UUID,
                "{label}: subject_id MUST be the stable AM system UUID"
            );
            assert_eq!(
                ctx.subject_type(),
                Some(AM_SYSTEM_SUBJECT_TYPE),
                "{label}: subject_type MUST be `am.system`"
            );
            assert_eq!(
                ctx.subject_tenant_id(),
                tenant,
                "{label}: subject_tenant_id MUST carry the supplied tenant"
            );
        }
    }

    // The cascade hook's subject tenant is a Deleted row
    // mid-purge — a subtree grant rooted at it materializes to zero
    // tenants and fails the whole reaper closed. The factory MUST be
    // platform-scoped (nil tenant → Global grant at the resolver).
    #[test]
    fn user_groups_cascade_is_platform_scoped() {
        let ctx = for_user_groups_cascade(Uuid::from_u128(0xDEAD_BEEF_FACE_CAFE));
        assert_eq!(ctx.subject_id(), AM_SYSTEM_ACTOR_UUID);
        assert_eq!(ctx.subject_type(), Some(AM_SYSTEM_SUBJECT_TYPE));
        assert_eq!(
            ctx.subject_tenant_id(),
            Uuid::nil(),
            "cascade context must be platform-scoped, not scoped to the deleted tenant"
        );
    }
}
