// Created: 2026-04-07 by Constructor Tech
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

fn request_with(tenant: Option<Uuid>, group_selector: bool) -> EvaluationRequest {
    let mut properties = HashMap::new();
    if group_selector {
        properties.insert("group_ids".to_owned(), serde_json::json!([Uuid::nil()]));
    }
    EvaluationRequest {
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
            properties,
        },
        context: EvaluationRequestContext {
            tenant_context: tenant.map(|root| TenantContext {
                root_id: Some(root),
                ..TenantContext::default()
            }),
            token_scopes: vec![],
            require_constraints: false,
            capabilities: vec![],
            supported_properties: vec!["owner_tenant_id".to_owned()],
            bearer_token: None,
        },
    }
}

const TENANT: Uuid = Uuid::from_u128(0x3333);

fn mixed_requests() -> Vec<EvaluationRequest> {
    vec![
        request_with(Some(TENANT), false),
        request_with(None, false),
        request_with(Some(Uuid::nil()), false),
        request_with(Some(TENANT), true),
        request_with(Some(TENANT), false),
    ]
}

#[tokio::test]
async fn batch_results_equal_single_evaluations() {
    let service = Service::new();
    let plugin: &dyn AuthZResolverPluginClient = &service;

    let batch = plugin
        .evaluate_batch(
            mixed_requests(),
            authz_resolver_sdk::EvaluationsSemantic::ExecuteAll,
            std::num::NonZeroUsize::new(2).unwrap(),
            &authz_resolver_sdk::Semaphore::new(64),
        )
        .await
        .unwrap();

    assert_eq!(batch.len(), 5);
    for (request, result) in mixed_requests().into_iter().zip(&batch) {
        let single = plugin.evaluate(request).await.unwrap();
        assert_eq!(
            serde_json::to_value(result).unwrap(),
            serde_json::to_value(&single).unwrap()
        );
    }
    let decisions: Vec<bool> = batch.iter().map(|r| r.decision).collect();
    assert_eq!(decisions, [true, false, false, false, true]);
}

#[tokio::test]
async fn every_denial_carries_an_error_code() {
    let service = Service::new();
    let responses: Vec<EvaluationResponse> = mixed_requests()
        .iter()
        .map(|request| service.evaluate(request))
        .collect();
    let denied: Vec<usize> = (0..responses.len())
        .filter(|&i| !responses[i].decision)
        .collect();
    assert_eq!(denied, [1, 2, 3], "no tenant, nil tenant, group selector");
    for i in denied {
        let reason = responses[i]
            .context
            .deny_reason
            .as_ref()
            .expect("a deny reason");
        assert_eq!(
            reason.error_code,
            authz_resolver_sdk::DenyReason::INVALID_REQUEST
        );
    }
}

#[tokio::test]
async fn deny_on_first_deny_stops_after_the_first_denial() {
    let service = Service::new();
    let plugin: &dyn AuthZResolverPluginClient = &service;
    let batch = plugin
        .evaluate_batch(
            mixed_requests(),
            authz_resolver_sdk::EvaluationsSemantic::DenyOnFirstDeny,
            std::num::NonZeroUsize::MIN,
            &authz_resolver_sdk::Semaphore::new(64),
        )
        .await
        .unwrap();
    let decisions: Vec<bool> = batch.iter().map(|r| r.decision).collect();
    assert_eq!(decisions, [true, false]);
}
