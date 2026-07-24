use crate::domain::error::DomainError;
use crate::domain::model::Upstream;

use authz_resolver_sdk::PolicyEnforcer;
use authz_resolver_sdk::pep::{AccessRequest, ResourceType};
use credstore_sdk::CredStoreClientV1;
use oagw_sdk::UPSTREAM_SCHEMA;
use toolkit_security::SecurityContext;

/// Resource type for upstream binding permission checks.
const UPSTREAM_RESOURCE: ResourceType =
    ResourceType::from_static(UPSTREAM_SCHEMA, &["owner_tenant_id"]);

/// Permission action names for ancestor bind checks.
pub(in crate::domain::services) mod bind_actions {
    pub const BIND: &str = "bind";
    pub const OVERRIDE_AUTH: &str = "override_auth";
    pub const OVERRIDE_RATE: &str = "override_rate";
    pub const ADD_PLUGINS: &str = "add_plugins";
}

/// Describes the override fields a descendant is attempting to set.
/// Used by `validate_bind_constraints` so both create and update can share
/// the same validation logic.
#[allow(unknown_lints, de0309_must_have_domain_model)] // short-lived param container, not a domain entity
pub(in crate::domain::services) struct BindOverrides<'a> {
    pub auth: Option<&'a crate::domain::model::AuthConfig>,
    pub rate_limit: Option<&'a crate::domain::model::RateLimitConfig>,
    pub plugins: Option<&'a crate::domain::model::PluginsConfig>,
    pub cors: Option<&'a crate::domain::model::CorsConfig>,
}

/// Validate bind constraints when a descendant creates or updates an
/// upstream whose alias matches an ancestor's upstream.
///
/// Permissions (per `cpt-cf-oagw-algo-tenant-permission-check`):
/// - `oagw:upstream:bind` — always required
/// - `oagw:upstream:override_auth` — required when ancestor auth sharing is `inherit`
/// - `oagw:upstream:override_rate` — required when ancestor rate_limit sharing is `inherit`
/// - `oagw:upstream:add_plugins` — required when ancestor plugins sharing is not `private`
///
/// Sharing modes:
/// - `enforce` → blocked (plugins excepted: merge-time protection)
/// - `private` → shadow allowed, no permission
/// - `inherit` → allowed with permission
pub(in crate::domain::services) async fn validate_bind_constraints(
    ctx: &SecurityContext,
    enforcer: &PolicyEnforcer,
    ancestor: &Upstream,
    overrides: &BindOverrides<'_>,
) -> Result<(), DomainError> {
    use crate::domain::model::SharingMode;

    // 1. Check bind permission.
    let access_req = AccessRequest::new()
        .resource_property("owner_tenant_id", ancestor.tenant_id)
        .require_constraints(false);
    enforcer
        .access_scope_with(
            ctx,
            &UPSTREAM_RESOURCE,
            bind_actions::BIND,
            Some(ancestor.id),
            &access_req,
        )
        .await?;

    // 2. Check per-field override permissions and sharing mode constraints.

    // Auth override.
    if overrides.auth.is_some() {
        match ancestor.auth.as_ref().map(|a| a.sharing) {
            Some(SharingMode::Enforce) => {
                return Err(DomainError::validation(
                    "cannot override auth: ancestor upstream has sharing mode 'enforce'",
                ));
            }
            Some(SharingMode::Private) => {
                // Private = invisible → descendant is providing fresh config (shadow),
                // not overriding.  No permission check needed.
            }
            _ => {
                // Inherit / absent → real override, requires permission.
                enforcer
                    .access_scope_with(
                        ctx,
                        &UPSTREAM_RESOURCE,
                        bind_actions::OVERRIDE_AUTH,
                        Some(ancestor.id),
                        &access_req,
                    )
                    .await?;
            }
        }
    }

    // Rate-limit override.
    if overrides.rate_limit.is_some() {
        match ancestor.rate_limit.as_ref().map(|r| r.sharing) {
            Some(SharingMode::Enforce) => {
                return Err(DomainError::validation(
                    "cannot override rate_limit: ancestor upstream has sharing mode 'enforce'",
                ));
            }
            Some(SharingMode::Private) => {}
            _ => {
                enforcer
                    .access_scope_with(
                        ctx,
                        &UPSTREAM_RESOURCE,
                        bind_actions::OVERRIDE_RATE,
                        Some(ancestor.id),
                        &access_req,
                    )
                    .await?;
            }
        }
    }

    // Plugins override (Enforce handled at merge time, not here).
    if overrides.plugins.is_some() {
        match ancestor.plugins.as_ref().map(|p| p.sharing) {
            Some(SharingMode::Private) => {}
            _ => {
                enforcer
                    .access_scope_with(
                        ctx,
                        &UPSTREAM_RESOURCE,
                        bind_actions::ADD_PLUGINS,
                        Some(ancestor.id),
                        &access_req,
                    )
                    .await?;
            }
        }
    }

    // CORS override (no additional permission required, but Enforce blocks).
    if overrides.cors.is_some()
        && ancestor
            .cors
            .as_ref()
            .is_some_and(|c| c.sharing == SharingMode::Enforce)
    {
        return Err(DomainError::validation(
            "cannot override cors: ancestor upstream has sharing mode 'enforce'",
        ));
    }

    Ok(())
}

/// Validate that a `secret_ref` is accessible to the requesting tenant via
/// `cred_store`. Per `cpt-cf-oagw-principle-cred-isolation`, if the secret
/// is not accessible, the request is rejected (fail-closed).
pub(in crate::domain::services) async fn validate_secret_ref_accessible(
    ctx: &SecurityContext,
    credstore: &dyn CredStoreClientV1,
    raw_ref: &str,
) -> Result<(), DomainError> {
    let bare = raw_ref.strip_prefix("cred://").unwrap_or(raw_ref);
    let key = credstore_sdk::SecretRef::new(bare)
        .map_err(|e| DomainError::validation(format!("invalid secret_ref '{raw_ref}': {e}")))?;

    match credstore.get(ctx, &key).await {
        Ok(Some(_)) => Ok(()),
        // Not a malformed argument: the ref parses, the secret just isn't
        // there (yet) or isn't shared. Distinguished from `Validation` so
        // provisioning callers can defer-and-retry instead of treating it
        // as a deterministic misconfiguration.
        Ok(None) => Err(DomainError::SecretRefNotAccessible {
            detail: format!(
                "secret_ref '{raw_ref}' is not accessible to this tenant \
                 (not provisioned yet, or not shared)"
            ),
            instance: String::new(),
        }),
        Err(credstore_sdk::CredStoreError::Internal(msg)) => {
            // Fail-closed: cred_store unavailability → reject.
            tracing::warn!(secret_ref = raw_ref, error = %msg, "cred_store unavailable during secret_ref validation");
            Err(DomainError::Internal {
                message: format!("credential validation unavailable: {msg}"),
            })
        }
        // Same state as `Ok(None)`, reported as an error by some client
        // implementations (e.g. a metadata row whose value is still being
        // provisioned): not accessible *yet*, so provisioning callers must be
        // able to defer-and-retry rather than fail fast.
        Err(e) if e.is_not_found() => Err(DomainError::SecretRefNotAccessible {
            detail: format!(
                "secret_ref '{raw_ref}' is not accessible to this tenant \
                 (not provisioned yet, or not shared)"
            ),
            instance: String::new(),
        }),
        Err(e) => Err(DomainError::validation(format!(
            "secret_ref '{raw_ref}' validation failed: {e}"
        ))),
    }
}

/// Validate bind constraints against the **closest** ancestor with a matching
/// alias. Delegates to [`validate_bind_constraints`] for the actual checks.
///
/// No-op if no ancestor has the alias (fresh upstream, no bind needed).
pub(in crate::domain::services) async fn validate_ancestor_bind(
    ctx: &SecurityContext,
    upstreams: &dyn crate::domain::repo::UpstreamRepository,
    enforcer: &PolicyEnforcer,
    tenant_chain: &[uuid::Uuid],
    alias: &str,
    overrides: &BindOverrides<'_>,
) -> Result<(), DomainError> {
    for &ancestor_tid in &tenant_chain[1..] {
        match upstreams.get_by_alias(ancestor_tid, alias).await {
            Ok(ancestor_upstream) => {
                validate_bind_constraints(ctx, enforcer, &ancestor_upstream, overrides).await?;
                break; // Only check closest ancestor with matching alias.
            }
            Err(crate::domain::repo::RepositoryError::NotFound { .. }) => continue,
            Err(e) => return Err(DomainError::from(e)),
        }
    }
    Ok(())
}
