use super::*;
use authz_resolver_sdk::{Action, EvaluationRequestContext, Resource, Subject, TenantContext};
use std::collections::HashMap;
use uuid::Uuid;

#[tokio::test]
async fn plugin_trait_evaluates_successfully() {
    let service = Service::new();
    let plugin: &dyn AuthZResolverPluginClient = &service;

    let request = EvaluationRequest {
        subject: Subject {
            id: Uuid::nil(),
            subject_type: None,
            properties: HashMap::new(),
        },
        action: Action {
            name: "list".to_owned(),
        },
        resource: Resource {
            resource_type: "test".to_owned(),
            id: None,
            properties: HashMap::new(),
        },
        context: EvaluationRequestContext {
            tenant_context: Some(TenantContext {
                root_id: Some(Uuid::parse_str("33333333-3333-3333-3333-333333333333").unwrap()),
                ..TenantContext::default()
            }),
            token_scopes: vec![],
            require_constraints: false,
            capabilities: vec![],
            supported_properties: vec![],
            bearer_token: None,
        },
    };

    let result = plugin.evaluate(request).await;
    assert!(result.is_ok());
    assert!(result.unwrap().decision);
}
