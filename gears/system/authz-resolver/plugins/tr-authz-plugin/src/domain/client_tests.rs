// Created: 2026-04-16 by Constructor Tech
// Updated: 2026-04-29 by Constructor Tech

use std::num::NonZeroUsize;
use std::sync::Arc;

use authz_resolver_sdk::models::{
    Action, DenyReason, EvaluationRequest, EvaluationRequestContext, EvaluationResponse, Resource,
    Subject, TenantContext,
};
use authz_resolver_sdk::{AuthZResolverPluginClient, EvaluationsSemantic};
use uuid::Uuid;

use crate::domain::service::Service;
use crate::domain::test_support::{MockTr, setup_svc};

#[tokio::test]
async fn client_trait_delegates_to_service() {
    let svc = Arc::new(Service::new(Arc::new(MockTr::empty())));
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

fn list_request(subject_tenant: Option<Uuid>, root: Option<Uuid>) -> EvaluationRequest {
    let mut properties = std::collections::HashMap::default();
    if let Some(tenant) = subject_tenant {
        properties.insert(
            "tenant_id".to_owned(),
            serde_json::Value::String(tenant.to_string()),
        );
    }
    EvaluationRequest {
        subject: Subject {
            id: Uuid::now_v7(),
            subject_type: None,
            properties,
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
            tenant_context: root.map(|root_id| TenantContext {
                root_id: Some(root_id),
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

/// A response as JSON with every predicate's value list sorted, so two
/// evaluations that list the same tenants in another order compare equal.
fn normalized(response: &EvaluationResponse) -> serde_json::Value {
    let mut value = serde_json::to_value(response).unwrap();
    if let Some(constraints) = value["context"]["constraints"].as_array_mut() {
        for constraint in constraints {
            if let Some(predicates) = constraint["predicates"].as_array_mut() {
                for predicate in predicates {
                    if let Some(values) = predicate["values"].as_array_mut() {
                        values.sort_by_key(ToString::to_string);
                    }
                }
            }
        }
    }
    value
}

fn deny_code(response: &EvaluationResponse) -> Option<&str> {
    response
        .context
        .deny_reason
        .as_ref()
        .map(|r| r.error_code.as_str())
}

#[tokio::test]
async fn batch_results_equal_single_evaluations_and_denials_carry_codes() {
    let (svc, [r, t1, t2, _t3, _t4]) = setup_svc();
    let svc = Arc::new(svc);
    let client: Arc<dyn AuthZResolverPluginClient> = Arc::clone(&svc) as _;
    let requests = || {
        vec![
            list_request(Some(t1), Some(t1)),
            list_request(None, Some(t1)),
            list_request(Some(t2), Some(r)),
            list_request(Some(Uuid::nil()), None),
            list_request(Some(t1), Some(t2)),
        ]
    };

    let batch = client
        .evaluate_batch(
            requests(),
            EvaluationsSemantic::ExecuteAll,
            NonZeroUsize::new(3).unwrap(),
            &authz_resolver_sdk::Semaphore::new(64),
        )
        .await
        .unwrap();

    assert_eq!(batch.len(), 5);
    for (request, result) in requests().into_iter().zip(&batch) {
        let single = client.evaluate(request).await.unwrap();
        assert_eq!(normalized(result), normalized(&single));
    }
    let decisions: Vec<bool> = batch.iter().map(|r| r.decision).collect();
    assert_eq!(decisions, [true, false, false, false, true]);
    assert!(
        !batch[0].context.constraints.is_empty(),
        "a permit carries the subtree constraint"
    );
    assert_eq!(
        deny_code(&batch[1]),
        Some(DenyReason::INVALID_REQUEST),
        "a missing subject tenant is an invalid request"
    );
    assert_eq!(
        deny_code(&batch[2]),
        Some(DenyReason::INSUFFICIENT_PERMISSIONS),
        "a subject outside the root's ancestry is refused by policy"
    );
    assert_eq!(
        deny_code(&batch[3]),
        Some(DenyReason::INVALID_REQUEST),
        "a nil subject tenant is an invalid request"
    );
}

#[tokio::test]
async fn an_unknown_tenant_is_a_denial_not_a_failure() {
    let (svc, [_r, t1, ..]) = setup_svc();
    let unknown = Uuid::now_v7();
    let response = svc
        .evaluate(&list_request(Some(t1), Some(unknown)))
        .await
        .expect("an unknown tenant is a policy answer");
    assert!(!response.decision);
    assert_eq!(
        deny_code(&response),
        Some(DenyReason::INSUFFICIENT_PERMISSIONS)
    );
}

#[tokio::test]
async fn a_tenant_resolver_failure_fails_the_whole_batch() {
    let svc = Arc::new(Service::new(Arc::new(MockTr::failing_descendants())));
    let client: Arc<dyn AuthZResolverPluginClient> = svc;
    let tenant = Uuid::now_v7();
    let err = client
        .evaluate_batch(
            vec![
                list_request(Some(tenant), Some(tenant)),
                list_request(Some(tenant), Some(tenant)),
            ],
            EvaluationsSemantic::DenyOnFirstDeny,
            NonZeroUsize::MIN,
            &authz_resolver_sdk::Semaphore::new(64),
        )
        .await
        .expect_err("a technical failure is never a decision");
    assert!(
        matches!(err, authz_resolver_sdk::AuthZResolverError::Internal(_)),
        "{err:?}"
    );
}
