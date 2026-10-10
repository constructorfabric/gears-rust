//! An unconditional grant against the real Event Broker ingest service: its `event_type`
//! `produce` check is property-less, so only an exact unconditional grant can satisfy it, and
//! the separate tenant-scope check still needs its own ordinary, constrained rule.
#![allow(clippy::unwrap_used, clippy::expect_used)] // Fixed test fixtures must fail loudly.
use authz_resolver_sdk::{
    AuthZResolverApi, AuthZResolverPluginClient, EvaluationRequest, EvaluationResponse,
    PolicyEnforcer,
};
use chrono::Utc;
use event_broker::domain::error::{DomainError, ErrorCode};
use event_broker::domain::ingest::{PublishAck, PublishRequest};
use event_broker::test_support::{EventBrokerHarness, StaticTypesRegistry, event_type_id};
use event_broker_sdk::gts::{EVENT_TYPE_RESOURCE_TYPE, REQUEST_RESOURCE_TYPE};
use rules_authz_plugin::config::RulesAuthZPluginConfig;
use rules_authz_plugin::domain::Service;
use serde_json::json;
use std::sync::Arc;
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::{PlatformSecurityContext, SecurityContext};
use uuid::Uuid;

const TOPIC: &str = "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1";
const EVENT: &str = "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~";

struct ViaPluginClient(Arc<dyn AuthZResolverPluginClient>);
#[async_trait::async_trait]
impl AuthZResolverApi for ViaPluginClient {
    async fn evaluate(
        &self,
        _: PlatformSecurityContext,
        request: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
        self.0
            .evaluate(request)
            .await
            .map_err(|_| CanonicalError::service_unavailable().create())
    }
}

fn u(n: u128) -> Uuid {
    Uuid::from_u128(n)
}
/// The producer principal (subject 7 in home tenant 500) publishing into root tenant 900.
fn producer(subject: u128) -> SecurityContext {
    SecurityContext::builder()
        .subject_id(u(subject))
        .subject_tenant_id(u(500))
        .subject_type("gts.cf.core.security.subject_service.v1~")
        .build()
        .unwrap()
}
fn policy(grants: &serde_json::Value) -> PolicyEnforcer {
    let config: RulesAuthZPluginConfig = serde_json::from_value(json!({
        "vendor": "constructorfabric", "priority": 10, "policy_revision": "eb-produce-1",
        // The tenant-scope check carries `owner_tenant_id`: an ordinary constrained rule.
        "rules": [{"id": "producer-root-tenant", "subject": {"id": u(7), "tenant_id": u(500)},
            "resource_type": REQUEST_RESOURCE_TYPE, "actions": ["produce"],
            "paths": [{"predicates": [{"property": "owner_tenant_id", "values": [u(900)]}]}]}],
        "unconditional_grants": grants,
    }))
    .unwrap();
    let plugin: Arc<dyn AuthZResolverPluginClient> =
        Arc::new(Service::from_config(&config).unwrap());
    PolicyEnforcer::new(Arc::new(ViaPluginClient(plugin)))
}
async fn broker(grants: serde_json::Value) -> EventBrokerHarness {
    EventBrokerHarness::builder()
        .with_type_registry(StaticTypesRegistry::of(json!([
            { "id": TOPIC, "partitions": 1 },
            { "id": EVENT, "topic": TOPIC, "allowed_subject_types": ["gts.x.eb.t1.subject.v1~"] },
        ])))
        .with_policy_enforcer(policy(&grants))
        .build()
        .await
}
fn event() -> PublishRequest {
    PublishRequest {
        id: Uuid::new_v4(),
        r#type: event_type_id(EVENT),
        tenant_id: u(900),
        source: "rules-authz-plugin-test".to_owned(),
        subject: "s1".to_owned(),
        subject_type: gts::GtsTypeId::new("gts.x.eb.t1.subject.v1~"),
        occurred_at: Utc::now(),
        trace_parent: None,
        data: json!({}),
        meta: None,
    }
}
fn produce_grant() -> serde_json::Value {
    json!([{"id": "orders-producer-event-type", "subject": {"id": u(7), "tenant_id": u(500)},
        "resource_type": EVENT_TYPE_RESOURCE_TYPE, "action": "produce"}])
}
fn denied_code(result: Result<PublishAck, DomainError>) -> ErrorCode {
    match result {
        Err(DomainError::Forbidden { code, .. }) => code,
        other => panic!("expected a forbidden publish, got {other:?}"),
    }
}

#[tokio::test]
async fn the_event_type_produce_check_passes_with_an_exact_unconditional_grant() {
    let harness = broker(produce_grant()).await;
    let ack = harness
        .ingest()
        .publish_event(&producer(7), event())
        .await
        .unwrap();
    assert!(matches!(ack, PublishAck::Accepted(_)));
    // Another subject, even with the same home tenant, is still refused at the produce check.
    assert_eq!(
        denied_code(harness.ingest().publish_event(&producer(8), event()).await),
        ErrorCode::NotAuthorizedToProduce
    );
    // The grant never satisfies the constrained tenant-scope check: another tenant is refused.
    let mut elsewhere = event();
    elsewhere.tenant_id = u(901);
    assert_eq!(
        denied_code(
            harness
                .ingest()
                .publish_event(&producer(7), elsewhere)
                .await
        ),
        ErrorCode::TenantIdNotAuthorized
    );
}

#[tokio::test]
async fn the_event_type_produce_check_fails_without_the_grant() {
    let harness = broker(json!([])).await;
    assert_eq!(
        denied_code(harness.ingest().publish_event(&producer(7), event()).await),
        ErrorCode::NotAuthorizedToProduce
    );
    // An unconditional grant on the property-carrying tenant-scope resource never applies, so it
    // cannot replace the event-type grant or widen the tenant check either.
    let misplaced = json!([{"id": "misplaced", "subject": {"id": u(7), "tenant_id": u(500)},
        "resource_type": REQUEST_RESOURCE_TYPE, "action": "produce"}]);
    let harness = broker(misplaced).await;
    assert_eq!(
        denied_code(harness.ingest().publish_event(&producer(7), event()).await),
        ErrorCode::NotAuthorizedToProduce
    );
}
