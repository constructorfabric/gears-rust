//! Authorization Service integration (M8): a [`PolicyEnforcerAuthorizer`] that
//! delegates per-type access decisions to the platform Authorization Service
//! over `gts.cf.fstorage.file.type.v1~` (`cpt-cf-file-storage-fr-authorization`).
//!
//! This implements the domain [`Authorizer`] abstraction; the gear can swap it
//! in for [`crate::domain::authz::TenantOnlyAuthorizer`] once the deployment's
//! PDP is configured to return tenant-scoped constraints. Tenant-boundary
//! enforcement (`cpt-cf-file-storage-fr-tenant-boundary`) is preserved either
//! way — point operations prefetch within the caller's tenant before the
//! decision, and listing applies the tenant scope.

use std::sync::Arc;

use async_trait::async_trait;
use authz_resolver_sdk::pep::{AccessRequest, ResourceType};
use authz_resolver_sdk::{AuthZResolverClient, EnforcerError, PolicyEnforcer};
use toolkit_security::{AccessScope, SecurityContext, pep_properties};
use uuid::Uuid;

use crate::domain::authz::Authorizer;
use crate::domain::error::DomainError;

/// Custom PEP property carrying the file's GTS type for per-type decisions.
const GTS_FILE_TYPE_PROP: &str = "gts_file_type";

/// The file-storage authorization resource and the properties the PDP may use.
const FILE_RESOURCE: ResourceType = ResourceType::from_static(
    "file_storage.file",
    &[
        pep_properties::OWNER_TENANT_ID,
        pep_properties::RESOURCE_ID,
        GTS_FILE_TYPE_PROP,
    ],
);

/// Authorizer backed by the platform Authorization Service via [`PolicyEnforcer`].
pub struct PolicyEnforcerAuthorizer {
    enforcer: PolicyEnforcer,
}

impl PolicyEnforcerAuthorizer {
    /// Build from the `ClientHub`-resolved `AuthZ` resolver client.
    #[must_use]
    pub fn new(authz: Arc<dyn AuthZResolverClient>) -> Self {
        Self {
            enforcer: PolicyEnforcer::new(authz),
        }
    }
}

#[async_trait]
impl Authorizer for PolicyEnforcerAuthorizer {
    async fn authorize(
        &self,
        ctx: &SecurityContext,
        action: &str,
        gts_file_type: &str,
        file_id: Option<Uuid>,
    ) -> Result<AccessScope, DomainError> {
        let request = AccessRequest::new()
            .resource_property(pep_properties::OWNER_TENANT_ID, ctx.subject_tenant_id())
            .resource_property(GTS_FILE_TYPE_PROP, gts_file_type.to_owned());

        self.enforcer
            .access_scope_with(ctx, &FILE_RESOURCE, action, file_id, &request)
            .await
            .map_err(|e| map_enforcer_error(&e))
    }
}

fn map_enforcer_error(e: &EnforcerError) -> DomainError {
    match e {
        EnforcerError::Denied { .. } | EnforcerError::CompileFailed(_) => DomainError::Forbidden,
        EnforcerError::EvaluationFailed(_) => DomainError::InternalError,
    }
}
