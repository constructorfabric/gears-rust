use std::sync::Arc;

use quota_resolver_sdk::models::{
    PlatformCommitLeaseRequest, PlatformDebitRequest, PlatformQuotaStateRequest, PrincipalSubject,
    QuotaStateMetric, QuotaStateRequest, SubjectInput,
};
use quota_resolver_sdk::{PlatformQuotaResolverApi, QuotaResolverApi};
use toolkit_security::{PlatformIdentity, PlatformSecurityContext, SecurityContext};

use super::QuotaResolverLocalClient;
use crate::domain::test_support::*;

fn client(env: &Env) -> QuotaResolverLocalClient {
    QuotaResolverLocalClient::new(Arc::new(env.service()))
}

fn principal() -> SubjectInput {
    SubjectInput::Principal(PrincipalSubject {
        subject_type: USER_TYPE.to_owned(),
        id: principal_id(),
    })
}

fn billing() -> PlatformIdentity {
    PlatformIdentity::Shared {
        name: "billing".to_owned(),
    }
}

#[tokio::test]
async fn a_validated_platform_caller_reaches_the_plugin_as_the_caller() {
    let env = Env::new();
    let client = client(&env);
    let request = || PlatformDebitRequest {
        tenant_id: tenant(),
        subjects: vec![principal()],
        items: vec![item()],
        idempotency_key: "k".to_owned(),
    };

    client
        .debit(PlatformSecurityContext::new(billing()), request())
        .await
        .unwrap();
    client
        .debit(PlatformSecurityContext::outbound_marker(), request())
        .await
        .unwrap();
    client
        .commit_lease(
            PlatformSecurityContext::new(billing()),
            PlatformCommitLeaseRequest {
                tenant_id: tenant(),
                token: "t".to_owned(),
                amounts: None,
                idempotency_key: "k".to_owned(),
            },
        )
        .await
        .unwrap();

    let debits = env.plugin.debits.lock().unwrap();
    assert_eq!(debits[0].caller, Some(billing()));
    assert_eq!(debits[1].caller, None);
    assert_eq!(
        *env.plugin.settlements.lock().unwrap(),
        vec![(tenant(), Some(billing()))]
    );
}

#[tokio::test]
async fn both_contracts_read_quota_state_through_the_same_service() {
    let env = Env::new();
    let client = client(&env);

    let platform = PlatformQuotaResolverApi::quota_state(
        &client,
        PlatformSecurityContext::new(billing()),
        PlatformQuotaStateRequest {
            tenant_id: tenant(),
            subject: principal(),
            metrics: vec![QuotaStateMetric::of(METRIC_TYPE)],
            limit: None,
            cursor: None,
        },
    )
    .await
    .unwrap();
    let ctx = SecurityContext::builder()
        .subject_id(principal_id())
        .subject_type(USER_TYPE)
        .subject_tenant_id(tenant())
        .build()
        .unwrap();
    let tenant_page = QuotaResolverApi::quota_state(
        &client,
        ctx,
        QuotaStateRequest {
            metrics: vec![QuotaStateMetric::of(METRIC_TYPE)],
            limit: None,
            cursor: None,
        },
    )
    .await
    .unwrap();

    assert_eq!(platform.items.len(), 1);
    assert_eq!(tenant_page.items.len(), 1);
    let states = env.plugin.states.lock().unwrap();
    assert_eq!(states[0].caller, Some(billing()));
    assert_eq!(states[1].caller, None);
}

#[tokio::test]
async fn a_tenant_state_body_naming_another_tenant_and_subject_reads_the_callers_own() {
    let env = Env::new();
    let body = serde_json::json!({
        "metrics": [{ "type": METRIC_TYPE }],
        "tenant_id": uuid::Uuid::from_u128(77),
        "subject": { "kind": "quota", "type": USER_SUBJECT, "id": "someone-else" },
    });
    let request: QuotaStateRequest = serde_json::from_value(body).unwrap();
    let ctx = SecurityContext::builder()
        .subject_id(principal_id())
        .subject_type(USER_TYPE)
        .subject_tenant_id(tenant())
        .build()
        .unwrap();

    QuotaResolverApi::quota_state(&client(&env), ctx, request)
        .await
        .unwrap();

    let states = env.plugin.states.lock().unwrap();
    assert_eq!(states[0].tenant_id, tenant());
    assert_eq!(states[0].subject.subject_type, USER_SUBJECT);
    assert_eq!(states[0].subject.id, principal_id().to_string());
}
