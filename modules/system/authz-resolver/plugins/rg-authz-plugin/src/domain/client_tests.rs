use std::sync::Arc;

use async_trait::async_trait;
use authz_resolver_sdk::AuthZResolverPluginClient;
use authz_resolver_sdk::models::{
    Action, EvaluationRequest, EvaluationRequestContext, Resource, Subject,
};
use modkit_odata::{ODataQuery, Page, PageInfo};
use modkit_security::SecurityContext;
use resource_group_sdk::api::ResourceGroupReadHierarchy;
use resource_group_sdk::error::ResourceGroupError;
use resource_group_sdk::models::ResourceGroupWithDepth;
use uuid::Uuid;

use crate::domain::service::Service;

struct EmptyRg;

#[async_trait]
impl ResourceGroupReadHierarchy for EmptyRg {
    async fn get_group_descendants(
        &self,
        _ctx: &SecurityContext,
        _group_id: Uuid,
        _query: &ODataQuery,
    ) -> Result<Page<ResourceGroupWithDepth>, ResourceGroupError> {
        Ok(Page {
            items: vec![],
            page_info: PageInfo {
                next_cursor: None,
                prev_cursor: None,
                limit: 25,
            },
        })
    }

    async fn get_group_ancestors(
        &self,
        _ctx: &SecurityContext,
        _group_id: Uuid,
        _query: &ODataQuery,
    ) -> Result<Page<ResourceGroupWithDepth>, ResourceGroupError> {
        Ok(Page {
            items: vec![],
            page_info: PageInfo {
                next_cursor: None,
                prev_cursor: None,
                limit: 25,
            },
        })
    }
}

#[tokio::test]
async fn client_trait_delegates_to_service() {
    let svc = Arc::new(Service::new(Arc::new(EmptyRg)));
    let client: Arc<dyn AuthZResolverPluginClient> = svc;

    let req = EvaluationRequest {
        subject: Subject {
            id: Uuid::now_v7(),
            subject_type: None,
            properties: Default::default(),
        },
        action: Action {
            name: "list".to_owned(),
        },
        resource: Resource {
            resource_type: "test".to_owned(),
            id: None,
            properties: Default::default(),
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
    assert!(!resp.decision, "no tenant → deny");
}
