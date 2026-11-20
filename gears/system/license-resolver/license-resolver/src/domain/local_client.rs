//! Local (in-process) client for the license resolver gear.

use std::sync::Arc;

use async_trait::async_trait;
use license_resolver_sdk::{
    IsLicensedRequest, LicenseCheckRequest, LicenseDecision, LicenseResolverApi,
    LicenseResolverError, PlatformIsLicensedRequest, PlatformLicenseResolverApi, PrincipalSubject,
    SubjectInput,
};
use toolkit_canonical_errors::CanonicalError;
use toolkit_macros::domain_model;
use toolkit_security::{PlatformSecurityContext, SecurityContext};

use super::Service;

/// Both public contracts over the license resolver service.
///
/// Registered in `ClientHub` for each by the gear during `init()`.
#[domain_model]
pub struct LicenseResolverLocalClient {
    svc: Arc<Service>,
}

impl LicenseResolverLocalClient {
    #[must_use]
    pub fn new(svc: Arc<Service>) -> Self {
        Self { svc }
    }
}

#[async_trait]
impl PlatformLicenseResolverApi for LicenseResolverLocalClient {
    async fn is_licensed(
        &self,
        _ctx: PlatformSecurityContext,
        request: PlatformIsLicensedRequest,
    ) -> Result<LicenseDecision, CanonicalError> {
        let PlatformIsLicensedRequest {
            tenant_id,
            subject,
            resource,
            context,
            ..
        } = request;
        match subject {
            SubjectInput::License(subject) => {
                self.svc
                    .is_licensed(LicenseCheckRequest::new(
                        tenant_id, subject, resource, context,
                    ))
                    .await
            }
            SubjectInput::Principal(principal) => {
                self.svc
                    .is_principal_licensed(tenant_id, principal, resource, context)
                    .await
            }
        }
        .map_err(|e| LicenseResolverError::from(e).into())
    }
}

/// The caller's principal in the caller's tenant, checked as on the platform plane.
#[async_trait]
impl LicenseResolverApi for LicenseResolverLocalClient {
    async fn is_licensed(
        &self,
        ctx: SecurityContext,
        request: IsLicensedRequest,
    ) -> Result<LicenseDecision, CanonicalError> {
        let IsLicensedRequest {
            resource, context, ..
        } = request;
        self.svc
            .is_principal_licensed(
                ctx.subject_tenant_id(),
                PrincipalSubject::from_security_context(&ctx),
                resource,
                context,
            )
            .await
            .map_err(|e| LicenseResolverError::from(e).into())
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "local_client_tests.rs"]
mod local_client_tests;
