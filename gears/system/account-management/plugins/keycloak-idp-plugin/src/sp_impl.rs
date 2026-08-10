//! `ServicePrincipalClientV1` implementation — thin delegation to
//! [`ServicePrincipalFacade`] + `PluginError` → SDK failure translation.

use async_trait::async_trait;
use service_principal_sdk::{
    CreateServicePrincipalRequest, ServicePrincipalClientV1, ServicePrincipalCredentials,
    ServicePrincipalFailure, ServicePrincipalSummary, TenantId,
};
use toolkit_security::SecurityContext;

use crate::domain::error::PluginError;
use crate::idp_impl::KeycloakIdpPlugin;

fn translate_sp_failure(e: PluginError) -> ServicePrincipalFailure {
    match e {
        PluginError::SpInvalidInput { detail, field } => {
            ServicePrincipalFailure::InvalidInput { detail, field }
        }
        PluginError::SpNotFound { detail } => ServicePrincipalFailure::NotFound { detail },
        PluginError::AmbiguousCreated { stage, detail } => ServicePrincipalFailure::Ambiguous {
            detail: format!("{stage}: {detail}"),
        },
        // Everything else is a pre-mutation failure (reads, pre-flight
        // validation, config): no vendor state was retained. Post-mutation
        // escapes are re-tagged Ambiguous in the facade before reaching here.
        other => ServicePrincipalFailure::CleanFailure {
            detail: other.to_string(),
        },
    }
}

#[async_trait]
impl ServicePrincipalClientV1 for KeycloakIdpPlugin {
    async fn create(
        &self,
        ctx: &SecurityContext,
        req: &CreateServicePrincipalRequest,
    ) -> Result<ServicePrincipalCredentials, ServicePrincipalFailure> {
        self.sp_facade
            .create_inner(ctx, req)
            .await
            .map_err(translate_sp_failure)
    }

    async fn rotate_secret(
        &self,
        ctx: &SecurityContext,
        tenant_id: TenantId,
        client_id: &str,
    ) -> Result<ServicePrincipalCredentials, ServicePrincipalFailure> {
        self.sp_facade
            .rotate_secret_inner(ctx, tenant_id.0, client_id)
            .await
            .map_err(translate_sp_failure)
    }

    async fn revoke(
        &self,
        ctx: &SecurityContext,
        tenant_id: TenantId,
        client_id: &str,
    ) -> Result<(), ServicePrincipalFailure> {
        self.sp_facade
            .revoke_inner(ctx, tenant_id.0, client_id)
            .await
            .map_err(translate_sp_failure)
    }

    async fn list(
        &self,
        ctx: &SecurityContext,
        tenant_id: TenantId,
    ) -> Result<Vec<ServicePrincipalSummary>, ServicePrincipalFailure> {
        self.sp_facade
            .list_inner(ctx, tenant_id.0)
            .await
            .map_err(translate_sp_failure)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn translate_maps_plugin_errors_to_sp_failures() {
        use crate::domain::error::{AmbiguousStage, KcStatusKind, PluginError};
        let cases = [
            (
                PluginError::SpInvalidInput {
                    detail: "d".into(),
                    field: None,
                },
                "invalid_input",
            ),
            (PluginError::SpNotFound { detail: "d".into() }, "not_found"),
            (
                PluginError::AmbiguousCreated {
                    stage: AmbiguousStage::KcClientDelete,
                    detail: "d".into(),
                },
                "ambiguous",
            ),
            (PluginError::Config { detail: "d".into() }, "clean_failure"),
            (
                PluginError::KcRest {
                    method: "GET",
                    path_template: "/admin/realms/{realm}/clients".into(),
                    status: KcStatusKind::Http(401),
                    body_first_2kb: String::new(),
                },
                "clean_failure",
            ),
        ];
        for (e, label) in cases {
            assert_eq!(translate_sp_failure(e).as_metric_label(), label);
        }
    }
}
