use async_trait::async_trait;
use toolkit_security::{AccessScope, SecurityContext};
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::planner::processing::ProcessingContexts;

/// [`ProcessingContexts`] as one configured identity, Construct's processing
/// service account, in the tenant at hand. It reads and writes graph storage
/// and reads the settings service for every tenant, so the deployment grants
/// it those permissions. No caller's token is kept: a connector's token can
/// expire before processing runs.
///
/// @cpt-dod:cpt-cf-construct-dod-profile-reader-wiring:p1
#[derive(Debug, Clone, Copy)]
pub struct ServiceIdentity {
    identity: Uuid,
}

impl ServiceIdentity {
    #[must_use]
    pub fn new(identity: Uuid) -> Self {
        Self { identity }
    }

    /// The security context of the identity in `tenant_id`.
    ///
    /// # Errors
    ///
    /// When the context cannot be built, a defect.
    pub fn context(&self, tenant_id: Uuid) -> Result<SecurityContext, DomainError> {
        SecurityContext::builder()
            .subject_id(self.identity)
            .subject_tenant_id(tenant_id)
            .build()
            .map_err(|err| DomainError::internal(format!("processing context: {err}")))
    }
}

#[async_trait]
impl ProcessingContexts for ServiceIdentity {
    async fn for_tenant(
        &self,
        tenant_id: Uuid,
    ) -> Result<(SecurityContext, AccessScope), DomainError> {
        Ok((self.context(tenant_id)?, AccessScope::for_tenant(tenant_id)))
    }
}
