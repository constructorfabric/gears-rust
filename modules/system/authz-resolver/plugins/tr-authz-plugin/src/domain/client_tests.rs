use std::sync::Arc;

use async_trait::async_trait;
use authz_resolver_sdk::AuthZResolverPluginClient;
use authz_resolver_sdk::models::{
    Action, EvaluationRequest, EvaluationRequestContext, Resource, Subject,
};
use modkit_security::SecurityContext;
use tenant_resolver_sdk::{
    GetAncestorsOptions, GetAncestorsResponse, GetDescendantsOptions, GetDescendantsResponse,
    GetTenantsOptions, IsAncestorOptions, TenantId, TenantInfo, TenantRef, TenantResolverClient,
    TenantResolverError, TenantStatus,
};
use uuid::Uuid;

use crate::domain::service::Service;

struct EmptyTr;

#[async_trait]
impl TenantResolverClient for EmptyTr {
    async fn get_tenant(
        &self,
        _ctx: &SecurityContext,
        id: TenantId,
    ) -> Result<TenantInfo, TenantResolverError> {
        Err(TenantResolverError::TenantNotFound { tenant_id: id })
    }

    async fn get_root_tenant(
        &self,
        _ctx: &SecurityContext,
    ) -> Result<TenantInfo, TenantResolverError> {
        Err(TenantResolverError::TenantNotFound {
            tenant_id: TenantId(Uuid::nil()),
        })
    }

    async fn get_tenants(
        &self,
        _ctx: &SecurityContext,
        _ids: &[TenantId],
        _opts: &GetTenantsOptions,
    ) -> Result<Vec<TenantInfo>, TenantResolverError> {
        Ok(vec![])
    }

    async fn get_descendants(
        &self,
        _ctx: &SecurityContext,
        id: TenantId,
        _opts: &GetDescendantsOptions,
    ) -> Result<GetDescendantsResponse, TenantResolverError> {
        Ok(GetDescendantsResponse {
            tenant: TenantRef {
                id,
                status: TenantStatus::Active,
                tenant_type: None,
                parent_id: None,
                self_managed: false,
            },
            descendants: vec![],
        })
    }

    async fn get_ancestors(
        &self,
        _ctx: &SecurityContext,
        id: TenantId,
        _opts: &GetAncestorsOptions,
    ) -> Result<GetAncestorsResponse, TenantResolverError> {
        Ok(GetAncestorsResponse {
            tenant: TenantRef {
                id,
                status: TenantStatus::Active,
                tenant_type: None,
                parent_id: None,
                self_managed: false,
            },
            ancestors: vec![],
        })
    }

    async fn is_ancestor(
        &self,
        _ctx: &SecurityContext,
        _ancestor: TenantId,
        _descendant: TenantId,
        _opts: &IsAncestorOptions,
    ) -> Result<bool, TenantResolverError> {
        Ok(false)
    }
}

#[tokio::test]
async fn client_trait_delegates_to_service() {
    let svc = Arc::new(Service::new(Arc::new(EmptyTr)));
    let client: Arc<dyn AuthZResolverPluginClient> = svc;

    let req = EvaluationRequest {
        subject: Subject {
            id: Uuid::now_v7(),
            subject_type: None,
            properties: std::collections::HashMap::default(),
        },
        action: Action {
            name: "list".to_owned(),
        },
        resource: Resource {
            resource_type: "test".to_owned(),
            id: None,
            properties: std::collections::HashMap::default(),
        },
        context: EvaluationRequestContext {
            tenant_context: None,
            token_scopes: vec![],
            require_constraints: false,
            capabilities: vec![],
            supported_properties: vec![],
            bearer_token: None,
        },
    };

    let resp = client
        .evaluate(req)
        .await
        .expect("evaluate should not error");
    assert!(!resp.decision, "no tenant -> deny");
}
