//! The service-principal facade service.
//!
//! Per request: authorize the explicit target tenant against the PDP, resolve the
//! SPI client lazily from the `ClientHub` (pluggable adapter), delegate, and map
//! failures. Stateless — a gear restart loses nothing.

use std::sync::Arc;

use authz_resolver_sdk::pep::{AccessRequest, PolicyEnforcer};
use service_principal_sdk::{
    CreateServicePrincipalRequest, ServicePrincipalClientV1, ServicePrincipalCredentials,
    ServicePrincipalFailure, ServicePrincipalSummary, TenantId,
};
use toolkit::ClientHub;
use toolkit_macros::domain_model;
use toolkit_security::{SecurityContext, pep_properties};
use tracing::warn;

use crate::domain::authz::{self, actions};
use crate::domain::error::DomainError;

/// The facade service. Holds the PDP enforcer and the `ClientHub` (for lazy SPI
/// resolution). No SPI client is held directly, keeping the adapter pluggable.
///
/// This is the gear's application/facade service and belongs in the domain layer
/// (mirroring e.g. usage-collector's identically-shaped `Service`). `#[domain_model]`
/// (DE0309) marks it as a domain type and enforces at compile time that no concrete
/// infrastructure type (HTTP/DB clients, etc.) leaks into its fields; the PEP
/// `PolicyEnforcer` and the `ClientHub` are SDK abstractions, not such infra, so the
/// marker applies cleanly.
#[domain_model]
pub struct Service {
    enforcer: PolicyEnforcer,
    hub: Arc<ClientHub>,
}

impl Service {
    #[must_use]
    pub fn new(enforcer: PolicyEnforcer, hub: Arc<ClientHub>) -> Self {
        Self { enforcer, hub }
    }

    /// Create a `client_credentials` service principal owned by `tenant`.
    ///
    /// # Errors
    /// [`DomainError`] on authorization failure, absent provider, or SPI failure.
    pub async fn create(
        &self,
        ctx: &SecurityContext,
        tenant: TenantId,
        name: String,
        scopes: Vec<String>,
    ) -> Result<ServicePrincipalCredentials, DomainError> {
        self.authorize(ctx, tenant, actions::CREATE).await?;
        let sp = self.sp_client()?;
        let req = CreateServicePrincipalRequest {
            tenant_id: tenant,
            name,
            scopes,
        };
        sp.create(ctx, &req).await.map_err(DomainError::from)
    }

    /// List the tenant's service principals (no secrets).
    ///
    /// # Errors
    /// [`DomainError`] on authorization failure, absent provider, or SPI failure.
    pub async fn list(
        &self,
        ctx: &SecurityContext,
        tenant: TenantId,
    ) -> Result<Vec<ServicePrincipalSummary>, DomainError> {
        self.authorize(ctx, tenant, actions::READ).await?;
        let sp = self.sp_client()?;
        sp.list(ctx, tenant).await.map_err(DomainError::from)
    }

    /// Rotate the principal's secret; the old one stops working.
    ///
    /// # Errors
    /// [`DomainError`] on authorization failure, absent provider, or SPI failure.
    pub async fn rotate_secret(
        &self,
        ctx: &SecurityContext,
        tenant: TenantId,
        client_id: &str,
    ) -> Result<ServicePrincipalCredentials, DomainError> {
        self.authorize(ctx, tenant, actions::ROTATE_SECRET).await?;
        let sp = self.sp_client()?;
        sp.rotate_secret(ctx, tenant, client_id)
            .await
            .map_err(DomainError::from)
    }

    /// Revoke (delete) the principal. Idempotent: a missing principal is success.
    ///
    /// # Errors
    /// [`DomainError`] on authorization failure, absent provider, or SPI failure
    /// other than `NotFound`.
    pub async fn revoke(
        &self,
        ctx: &SecurityContext,
        tenant: TenantId,
        client_id: &str,
    ) -> Result<(), DomainError> {
        self.authorize(ctx, tenant, actions::REVOKE).await?;
        let sp = self.sp_client()?;
        match sp.revoke(ctx, tenant, client_id).await {
            Ok(()) | Err(ServicePrincipalFailure::NotFound { .. }) => Ok(()),
            Err(other) => Err(DomainError::from(other)),
        }
    }

    /// Authorize `action` on the explicit target `tenant`, fail-closed.
    async fn authorize(
        &self,
        ctx: &SecurityContext,
        tenant: TenantId,
        action: &str,
    ) -> Result<(), DomainError> {
        let request = AccessRequest::new()
            .resource_property(pep_properties::OWNER_TENANT_ID, tenant.0)
            .require_constraints(true);
        let scope = self
            .enforcer
            .access_scope_with(ctx, &authz::SERVICE_PRINCIPAL, action, None, &request)
            .await
            .map_err(authz::map_enforcer_err)?;
        authz::ensure_scope_permits(&scope, tenant.0)
    }

    /// Resolve the SPI client lazily; absent registration → `ProviderUnavailable`.
    ///
    /// `DomainError::ProviderUnavailable` carries no field (it maps to a generic
    /// `503` at the REST boundary), so the underlying `ClientHubError` — which
    /// distinguishes "adapter simply not installed" (`NotFound`) from a genuine
    /// misregistration (`TypeMismatch`) — is logged here rather than silently
    /// discarded.
    fn sp_client(&self) -> Result<Arc<dyn ServicePrincipalClientV1>, DomainError> {
        self.hub
            .get::<dyn ServicePrincipalClientV1>()
            .map_err(|err| {
                warn!(error = %err, "service-principal SPI client unavailable");
                DomainError::ProviderUnavailable
            })
    }
}

#[cfg(test)]
#[path = "service_tests.rs"]
mod tests;
