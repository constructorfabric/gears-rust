//! Plugin-owned `SecurityContext` builder. Stable for the lifetime of the
//! process.
//!
//! Built once at `Gear::init` and cloned (via `Arc`) into every component
//! that performs `CredStore` reads or writes. See DESIGN "Security and Data
//! Protection".
//!
//! Mirrors the shape of AM's `system_actor::for_module_init` (see
//! `gears/system/account-management/account-management/src/domain/system_actor.rs`).
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

use toolkit_security::SecurityContext;
use uuid::Uuid;

/// Fixed UUID identifying the plugin as a system actor in audit /
/// `CredStore` ownership. **MUST remain stable across processes, replicas,
/// and upgrades** — changing it breaks secret ownership in `OpenBao` for
/// tenants whose `Created` realm secrets were written under the previous
/// value.
pub const KEYCLOAK_IDP_PLUGIN_ACTOR_UUID: Uuid =
    uuid::uuid!("1d70b6d4-6e2e-4f3c-9aa3-7d8c2e3f5b91");

/// `subject_type` recorded on every `CredStore` op the plugin performs.
///
/// The same value the plugin stamps on the `user_type` attribute of each
/// service-account user it mints, so it is spelled once — in
/// [`crate::config::SUBJECT_SERVICE_TYPE`] — rather than duplicated here
/// and kept in sync by hand.
///
/// **Advisory, not a gate.** `SecurityContext::subject_type` is an
/// `Option<String>` that the `AuthN` resolver populates and the platform
/// carries through unvalidated: no GTS type schema declares the
/// `cf.core.security` namespace (`gts_id!` builds and shape-checks an id at
/// compile time; it does not register one), and no authz plugin in this
/// workspace reads the field — the PEP copies it into the PDP request and
/// nothing branches on it. So this string cannot open or close an
/// authorization gate. What actually authorizes the plugin's credstore
/// writes is ordinary RBAC on [`KEYCLOAK_IDP_PLUGIN_ACTOR_UUID`] — an
/// Owner/credstore grant seeded for that UUID — rather than a PEP bypass.
///
/// It is still externally visible, so it is not free to change: realm
/// bootstrap's usermodel-attribute mappers lift `user_type` into issued
/// tokens (see [`crate::domain::service_account_facade`]). Treat it as a
/// wire value under the platform convention in
/// `docs/arch/authorization/DESIGN.md` — `subject_type` is a GTS type id,
/// the `user` arm being `gts.cf.core.security.subject_user.v1~`.
///
/// Audit attribution back to the plugin is keyed on the stable
/// [`KEYCLOAK_IDP_PLUGIN_ACTOR_UUID`], NOT on this string — see [`crate::domain::audit`].
pub const SUBJECT_TYPE: &str = crate::config::SUBJECT_SERVICE_TYPE;

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
/// `KeycloakIdpPluginConfig.security_context.platform_tenant_id` (defaults to the
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
        target: "keycloak_idp_plugin.system_actor",
        site = "module_init",
        platform_tenant_id = %platform_tenant_id,
        "keycloak-idp-plugin system actor constructed",
    );
    let ctx = SecurityContext::builder()
        .subject_id(KEYCLOAK_IDP_PLUGIN_ACTOR_UUID)
        .subject_type(SUBJECT_TYPE)
        .subject_tenant_id(platform_tenant_id)
        // First-party wildcard scope: this in-process ctx carries no bearer
        // token, so present "*" to clear the resolver's token-scope enforcer
        // (which fail-closes on empty token_scopes before RBAC runs). RBAC is
        // still the authoritative gate. See FIRST_PARTY_TOKEN_SCOPE.
        .token_scopes(vec![FIRST_PARTY_TOKEN_SCOPE.to_owned()])
        .build()
        .expect("KEYCLOAK_IDP_PLUGIN_ACTOR_UUID + platform_tenant_id are always present");
    Arc::new(ctx)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ctx_has_expected_identity() {
        let ctx = build_system_ctx(Uuid::nil());
        assert_eq!(ctx.subject_id(), KEYCLOAK_IDP_PLUGIN_ACTOR_UUID);
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
        assert_eq!(ctx.subject_id(), KEYCLOAK_IDP_PLUGIN_ACTOR_UUID);
        assert_eq!(ctx.subject_type(), Some(SUBJECT_TYPE));
        assert_eq!(ctx.subject_tenant_id(), tid);
    }
}
