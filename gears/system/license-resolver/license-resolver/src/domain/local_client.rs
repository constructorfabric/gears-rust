//! Local (in-process) client for the license resolver gear.

use std::sync::Arc;

use async_trait::async_trait;
use license_resolver_sdk::{
    IsLicensedRequest, LicenseCheckContext, LicenseCheckRequest, LicenseDecision,
    LicenseResolverApi, LicenseResolverError, SelfLicenseCheckRequest, SubjectInput,
};
use toolkit_canonical_errors::CanonicalError;
use toolkit_macros::domain_model;
use toolkit_security::{PlatformSecurityContext, SecurityContext};

use super::Service;

/// Local client wrapping the license resolver service.
///
/// Registered in `ClientHub` by the gear during `init()`.
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
impl LicenseResolverApi for LicenseResolverLocalClient {
    async fn is_licensed(
        &self,
        _ctx: PlatformSecurityContext,
        request: IsLicensedRequest,
    ) -> Result<LicenseDecision, CanonicalError> {
        let IsLicensedRequest {
            subject,
            resource,
            context,
            ..
        } = request;
        match subject {
            SubjectInput::License(subject) => {
                self.svc
                    .is_licensed(LicenseCheckRequest::new(subject, resource, context))
                    .await
            }
            SubjectInput::Principal(principal) => {
                self.svc
                    .is_principal_licensed(
                        Some(&principal.principal_type),
                        principal.id,
                        resource,
                        context,
                    )
                    .await
            }
        }
        .map_err(|e| LicenseResolverError::from(e).into())
    }

    async fn is_self_licensed(
        &self,
        ctx: SecurityContext,
        request: SelfLicenseCheckRequest,
    ) -> Result<LicenseDecision, CanonicalError> {
        self.svc
            .is_principal_licensed(
                ctx.subject_type(),
                ctx.subject_id(),
                request.resource,
                LicenseCheckContext::from_security_context(&ctx),
            )
            .await
            .map_err(|e| LicenseResolverError::from(e).into())
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "local_client_tests.rs"]
mod local_client_tests;
