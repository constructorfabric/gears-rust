//! Shared "resolve the target tenant, then prove it is usable" guard
//! run before every `IdpPluginClient` call that operates on an existing
//! tenant.
//!
//! Both pass-through services — [`crate::domain::user::service::UserService`]
//! and
//! [`crate::domain::service_account::service::ServiceAccountService`]
//! — must apply exactly the same precondition ladder before touching the
//! provider:
//!
//! 1. Read the tenant through the PDP-compiled [`AccessScope`], so an
//!    out-of-subtree target is invisible rather than merely rejected.
//! 2. Reject a miss with [`DomainError::NotFound`] and a non-`Active`
//!    status with [`DomainError::Validation`] — before any provider
//!    round-trip.
//! 3. Resolve the chained `tenant_type` mandatorily; a Types Registry
//!    outage is a [`DomainError::ServiceUnavailable`], not an `Option`
//!    leaking through the plugin contract.
//! 4. Replay the plugin's own opaque per-tenant `metadata`.
//!
//! It lives here as one free function rather than as a private method
//! per service because the ladder is security-critical and reviewers
//! check it once: a second copy is a second thing to keep in step.

use account_management_sdk::gts::TENANT_RESOURCE_TYPE;
use toolkit_security::AccessScope;
use types_registry_sdk::TypesRegistryClient;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::tenant::TenantContext;
use crate::domain::tenant::model::TenantStatus;
use crate::domain::tenant::repo::TenantRepo;

const TENANT_TYPE_RESOLUTION_UNAVAILABLE: &str =
    "tenant type resolution is temporarily unavailable";

/// Resolve `tenant_id` to an [`TenantStatus::Active`] tenant and build
/// the [`TenantContext`] forwarded to the `IdP` plugin.
///
/// `tenant_type` on the returned context is mandatory: AM resolves the
/// chained GTS identifier via [`TypesRegistryClient`] and surfaces an
/// outage as [`DomainError::service_unavailable`] rather than leaking an
/// `Option` through the plugin contract — plugins may route on the type
/// (Keycloak realm name, Zitadel organization selection, vendor-side org
/// id derivation). The opaque `metadata` blob is loaded from
/// `tenant_idp_metadata` (whatever the plugin returned at
/// `provision_tenant` time and AM persisted via `activate_tenant`) so
/// every `IdP` call receives the plugin's own per-tenant state inline.
///
/// # Errors
///
/// * [`DomainError::NotFound`] — `tenant_id` does not resolve within
///   `scope`.
/// * [`DomainError::Validation`] — the tenant exists but is not
///   [`TenantStatus::Active`] (provisioning, suspended, deleted).
/// * [`DomainError::ServiceUnavailable`] — Types Registry or DB
///   transport failure.
pub(crate) async fn resolve_active_tenant(
    tenant_repo: &dyn TenantRepo,
    types_registry: &dyn TypesRegistryClient,
    scope: &AccessScope,
    tenant_id: Uuid,
) -> Result<TenantContext, DomainError> {
    let tenant = tenant_repo
        .find_by_id(scope, tenant_id)
        .await?
        .ok_or_else(|| DomainError::NotFound {
            detail: format!("tenant {tenant_id} not found"),
            resource: tenant_id.to_string(),
        })?;

    if !matches!(tenant.status, TenantStatus::Active) {
        return Err(DomainError::Validation {
            detail: format!(
                "tenant {} is not active (status={})",
                tenant.id,
                tenant.status.as_str()
            ),
        });
    }

    // `get_type_schema_by_uuid` returns the typed `GtsTypeId` directly
    // so no string round-trip is needed.
    let tenant_type = match types_registry
        .get_type_schema_by_uuid(tenant.tenant_type_uuid)
        .await
    {
        Ok(schema) => schema.type_id,
        Err(err) => {
            tracing::warn!(
                target: "am.tenant.resolve",
                resource_type = TENANT_RESOURCE_TYPE,
                tenant_type_uuid = %tenant.tenant_type_uuid,
                error = %err,
                "tenant_type uuid -> chained-id resolution failed; surfacing ServiceUnavailable"
            );
            return Err(DomainError::service_unavailable(
                TENANT_TYPE_RESOLUTION_UNAVAILABLE,
            ));
        }
    };

    // AM does not interpret the plugin-private blob; the value flows
    // verbatim into `TenantContext::metadata` so the plugin sees its own
    // state.
    let metadata = tenant_repo.find_idp_metadata(scope, tenant.id).await?;
    Ok(TenantContext::new(
        tenant.id,
        tenant.name,
        tenant_type,
        metadata,
    ))
}
