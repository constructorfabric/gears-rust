//! Plugin-owned `SecurityContext` builder. Stable for the lifetime of the
//! process.
//!
//! Built once at `Gear::init` and cloned (via `Arc`) into every component
//! that performs `CredStore` reads or writes. See DESIGN §8.1 "Security
//! context for credstore operations".
//!
//! Mirrors the shape of AM's `system_actor::for_module_init` (see
//! `external/gears-rust/modules/system/account-management/account-management/src/domain/system_actor.rs`).
//! Differences:
//!
//! * AM exposes named per-call-site factories (`for_bootstrap`,
//!   `for_user_cleanup`, …) so every legitimate elevation is grep-visible.
//!   This plugin only has **one** legitimate system-actor use case
//!   (`CredStore` ops on tenant-scoped secrets owned by the plugin), so a
//!   single [`build_system_ctx`] entrypoint is the whole surface.
//! * Returns `Arc<SecurityContext>` because the resulting ctx is cloned into
//!   the [`CredStoreReader`](super::credstore::CredStoreReader) /
//!   [`CredStoreWriter`](super::credstore::CredStoreWriter) wrappers and lives
//!   for the lifetime of the process.

use std::sync::Arc;

use toolkit_gts::gts_id;
use toolkit_security::SecurityContext;
use uuid::Uuid;

/// Fixed UUID identifying the plugin as a system actor in audit /
/// `CredStore` ownership. **MUST remain stable across processes, replicas,
/// and upgrades** — changing it breaks secret ownership in `OpenBao` for
/// tenants whose `Created` realm secrets were written under the previous
/// value.
pub const VP_IDP_PLUGIN_ACTOR_UUID: Uuid = uuid::uuid!("1d70b6d4-6e2e-4f3c-9aa3-7d8c2e3f5b91");

/// `subject_type` recorded on every `CredStore` op the plugin performs.
///
/// This is the **canonical service-principal GTS subject type** (matches the
/// `subject_service` classifier pattern) so the authz-resolver classifies the
/// plugin as a `ServicePrincipal` and authorizes its credstore writes through
/// **ordinary RBAC** — an Owner/credstore grant seeded for
/// [`VP_IDP_PLUGIN_ACTOR_UUID`] — rather than a PEP bypass. The resolver
/// rejects an unknown `subject_type` fail-closed (Internal/500), so this MUST
/// stay a value `classify_subject_type` accepts (i.e. contains
/// `subject_service`); keep it in sync with
/// `VpIdpPluginConfig.service_principal.subject_type` (config default).
///
/// Audit attribution back to the plugin is keyed on the stable
/// [`VP_IDP_PLUGIN_ACTOR_UUID`], NOT on this string — see [`crate::domain::audit`].
pub const SUBJECT_TYPE: &str = gts_id!("cf.core.security.subject_service_principal.v1~");

/// First-party wildcard token scope (`"*"`). The plugin's in-process system
/// ctx carries no bearer token, so it presents this wildcard to satisfy the
/// authz resolver's OAuth token-scope enforcer (`scope_enforcer` fail-closes on
/// empty `token_scopes` with `scope_mismatch`, BEFORE RBAC even runs). It is
/// the same scope the resolver synthesizes for its own trusted system actor;
/// RBAC (the plugin's Credstore Secret Operator grant) remains the
/// authoritative limit on what the plugin may actually do.
const FIRST_PARTY_TOKEN_SCOPE: &str = "*";

/// Build the plugin-owned system ctx.
///
/// `platform_tenant_id` comes from
/// `VpIdpPluginConfig.security_context.platform_tenant_id` (defaults to the
/// platform root tenant `…0001`, NOT nil — credstore's own-tenant gate
/// requires a real tenant covered by the plugin's RBAC grant; see
/// `SecurityContextConfig`).
///
/// # Panics
///
/// Never in practice: both required builder fields are set unconditionally
/// below. The `expect` anchors the impossible-failure invariant.
#[must_use]
#[expect(
    clippy::expect_used,
    reason = "subject_id + subject_tenant_id are statically set; the expect anchors the impossible-failure invariant"
)]
pub fn build_system_ctx(platform_tenant_id: Uuid) -> Arc<SecurityContext> {
    tracing::debug!(
        target: "vp_idp_plugin.system_actor",
        site = "module_init",
        platform_tenant_id = %platform_tenant_id,
        "vp-idp-plugin system actor constructed",
    );
    let ctx = SecurityContext::builder()
        .subject_id(VP_IDP_PLUGIN_ACTOR_UUID)
        .subject_type(SUBJECT_TYPE)
        .subject_tenant_id(platform_tenant_id)
        // First-party wildcard scope: this in-process ctx carries no bearer
        // token, so present "*" to clear the resolver's token-scope enforcer
        // (which fail-closes on empty token_scopes before RBAC runs). RBAC is
        // still the authoritative gate. See FIRST_PARTY_TOKEN_SCOPE.
        .token_scopes(vec![FIRST_PARTY_TOKEN_SCOPE.to_owned()])
        .build()
        .expect("VP_IDP_PLUGIN_ACTOR_UUID + platform_tenant_id are always present");
    Arc::new(ctx)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ctx_has_expected_identity() {
        let ctx = build_system_ctx(Uuid::nil());
        assert_eq!(ctx.subject_id(), VP_IDP_PLUGIN_ACTOR_UUID);
        assert_eq!(ctx.subject_type(), Some(SUBJECT_TYPE));
        assert_eq!(ctx.subject_tenant_id(), Uuid::nil());
        // First-party wildcard scope clears the resolver's token-scope enforcer
        // (empty token_scopes fail-close before RBAC); RBAC is still the gate.
        assert_eq!(ctx.token_scopes(), ["*"]);
    }

    #[test]
    fn ctx_carries_custom_platform_tenant_id() {
        let tid = uuid::uuid!("11111111-2222-3333-4444-555555555555");
        let ctx = build_system_ctx(tid);
        assert_eq!(ctx.subject_id(), VP_IDP_PLUGIN_ACTOR_UUID);
        assert_eq!(ctx.subject_type(), Some(SUBJECT_TYPE));
        assert_eq!(ctx.subject_tenant_id(), tid);
    }
}
