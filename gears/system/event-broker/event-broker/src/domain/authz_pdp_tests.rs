//! The step-1 publish rows of #5492 through real policy.
//!
//! The recording double of `publish_authz_tests.rs` proves the request shapes,
//! the call order and the call counts; this file proves the answers. Every row
//! publishes through the harness's real ingest path under three PDPs: the
//! action-aware `PolicyAuthZ` double, the real tr-authz service over an
//! in-memory tenant tree, and the real static-authz service. A row's expected
//! answer for each PDP is the issue's Validation table, except that
//! static-authz allows any non-nil target tenant: it has no tree.
//!
//! The tree is the issue's example: root `0x123` -> `0x234` -> {`0xC`,
//! self-managed `0xB` -> `0xB1`}, and `0x999`, a second child of `0x123`.
//! `P` (subject `0x456`) is the platform publisher in `0x123`; `U` (subject
//! `0x890`) sits in `0x234`. Both hold the `produce` grant on the event type;
//! the `produce_ignore_barrier` grant is added per row.

use std::sync::Arc;

use authz_resolver_sdk::{AuthZResolverApi, PolicyEnforcer};
use chrono::{TimeZone, Utc};
use serde_json::json;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use super::pdp_fixtures::{
    B, InMemoryTenantTree, P, ROOT, StaticAuthz, T234, T999, TREE, TrAuthz, U, caller,
};
use crate::domain::error::{DomainError, ErrorCode};
use crate::domain::ingest::{BatchAck, PublishAck, PublishRequest};
use crate::test_support::{
    EventBrokerHarness, Grant, NeverStoringBackend, PolicyAuthZ, StaticTypesRegistry, Tenant,
};

const TOPIC: &str = "gts.cf.core.events.topic.v1~x.eb.pdp.topic.v1";
const EVENT_TYPE: &str = "gts.cf.core.events.event.v1~x.eb.pdp.foo.v1~";
const SUBJECT_TYPE: &str = "gts.x.eb.pdp.subject.v1~";

/// The PDP a row runs under; each assertion's message names the PDP and the row.
#[derive(Debug, Clone, Copy)]
enum Pdp {
    PolicyAuthZ,
    TrAuthz,
    StaticAuthz,
}

impl Pdp {
    /// The enforcer over this PDP. `PolicyAuthZ` holds the `produce` grant of
    /// `P` and `U` on the event type, and the `produce_ignore_barrier` grant
    /// of each subject in `barrier_grants`; the real services hold no grants.
    fn enforcer(self, barrier_grants: &[Uuid]) -> PolicyEnforcer {
        let authz: Arc<dyn AuthZResolverApi> = match self {
            Self::PolicyAuthZ => {
                let tenants =
                    TREE.iter()
                        .map(|&(id, parent, self_managed)| match (parent, self_managed) {
                            (None, _) => Tenant::root(id),
                            (Some(parent), false) => Tenant::child(id, parent),
                            (Some(parent), true) => Tenant::self_managed_child(id, parent),
                        });
                let mut policy = PolicyAuthZ::new(tenants)
                    .expect("the example tree is well formed")
                    .with_grant(Grant::new(P, EVENT_TYPE, "produce"))
                    .with_grant(Grant::new(U, EVENT_TYPE, "produce"));
                for subject in barrier_grants {
                    policy = policy.with_grant(Grant::new(
                        *subject,
                        EVENT_TYPE,
                        "produce_ignore_barrier",
                    ));
                }
                Arc::new(policy)
            }
            Self::TrAuthz => Arc::new(TrAuthz(tr_authz_plugin::domain::Service::new(Arc::new(
                InMemoryTenantTree,
            )))),
            Self::StaticAuthz => Arc::new(StaticAuthz(static_authz_plugin::domain::Service::new())),
        };
        PolicyEnforcer::new(authz)
    }

    /// The topic and event type on a backend that never stores, so an
    /// admitted event stays in the outbox and `outbox_bodies` counts it.
    async fn harness(self, barrier_grants: &[Uuid]) -> EventBrokerHarness {
        EventBrokerHarness::builder()
            .with_backend(Arc::new(NeverStoringBackend::new(Arc::new(
                memory_event_broker_plugin::MemoryBackend::new(),
            ))))
            .with_type_registry(StaticTypesRegistry::of(json!([
                { "id": TOPIC, "partitions": 1 },
                {
                    "id": EVENT_TYPE,
                    "topic": TOPIC,
                    "allowed_subject_types": [SUBJECT_TYPE],
                },
            ])))
            .with_policy_enforcer(self.enforcer(barrier_grants))
            .build()
            .await
    }
}

/// `P`: the platform publisher, in the root tenant.
fn platform_publisher() -> SecurityContext {
    caller(P, ROOT)
}

/// `U`: a user in tenant `0x234`.
fn tenant_user() -> SecurityContext {
    caller(U, T234)
}

fn request(id: u128, tenant: Uuid) -> PublishRequest {
    PublishRequest {
        id: Uuid::from_u128(id),
        r#type: crate::test_support::event_type_id(EVENT_TYPE),
        subject_tenant: tenant,
        source: "authz-pdp-test".to_owned(),
        subject: "s1".to_owned(),
        subject_type: gts::GtsTypeId::new(SUBJECT_TYPE),
        subject_owner: None,
        occurred_at: Utc.with_ymd_and_hms(2026, 11, 12, 10, 0, 0).unwrap(),
        trace_parent: None,
        data: json!({}),
        meta: None,
    }
}

#[tokio::test]
async fn policy_authz_lets_the_platform_publisher_publish_to_a_tenant_in_its_subtree() {
    let harness = Pdp::PolicyAuthZ.harness(&[]).await;

    let result = harness
        .ingest()
        .publish_event(&platform_publisher(), request(0x1_01, T234))
        .await;

    let Ok(PublishAck::Accepted(event)) = result else {
        panic!("row 1, PolicyAuthZ: expected Accepted, got {result:?}");
    };
    assert_eq!(
        serde_json::to_value(&event).unwrap(),
        json!({
            "id": Uuid::from_u128(0x1_01),
            "type": EVENT_TYPE,
            "topic": TOPIC,
            "subject_tenant": T234,
            "source": "authz-pdp-test",
            "subject": "s1",
            "subject_type": SUBJECT_TYPE,
            "occurred_at": "2026-11-12T10:00:00Z",
            "trace_parent": null,
            "data": {},
            "meta": null,
            "ingest": { "tenant": ROOT, "actor": P },
            "partition": null,
            "sequence": null,
            "sequence_time": null,
        }),
        "row 1, PolicyAuthZ: the stored event"
    );
    assert_eq!(
        harness.outbox_bodies().await,
        1,
        "row 1, PolicyAuthZ: the event is enqueued"
    );
}

#[tokio::test]
async fn tr_authz_lets_the_platform_publisher_publish_to_a_tenant_in_its_subtree() {
    let harness = Pdp::TrAuthz.harness(&[]).await;

    let result = harness
        .ingest()
        .publish_event(&platform_publisher(), request(0x1_01, T234))
        .await;

    let Ok(PublishAck::Accepted(event)) = result else {
        panic!("row 1, tr-authz: expected Accepted, got {result:?}");
    };
    assert_eq!(
        serde_json::to_value(&event).unwrap(),
        json!({
            "id": Uuid::from_u128(0x1_01),
            "type": EVENT_TYPE,
            "topic": TOPIC,
            "subject_tenant": T234,
            "source": "authz-pdp-test",
            "subject": "s1",
            "subject_type": SUBJECT_TYPE,
            "occurred_at": "2026-11-12T10:00:00Z",
            "trace_parent": null,
            "data": {},
            "meta": null,
            "ingest": { "tenant": ROOT, "actor": P },
            "partition": null,
            "sequence": null,
            "sequence_time": null,
        }),
        "row 1, tr-authz: the stored event"
    );
    assert_eq!(
        harness.outbox_bodies().await,
        1,
        "row 1, tr-authz: the event is enqueued"
    );
}

#[tokio::test]
async fn static_authz_lets_the_platform_publisher_publish_to_a_tenant_in_its_subtree() {
    let harness = Pdp::StaticAuthz.harness(&[]).await;

    let result = harness
        .ingest()
        .publish_event(&platform_publisher(), request(0x1_01, T234))
        .await;

    let Ok(PublishAck::Accepted(event)) = result else {
        panic!("row 1, static-authz: expected Accepted, got {result:?}");
    };
    assert_eq!(
        serde_json::to_value(&event).unwrap(),
        json!({
            "id": Uuid::from_u128(0x1_01),
            "type": EVENT_TYPE,
            "topic": TOPIC,
            "subject_tenant": T234,
            "source": "authz-pdp-test",
            "subject": "s1",
            "subject_type": SUBJECT_TYPE,
            "occurred_at": "2026-11-12T10:00:00Z",
            "trace_parent": null,
            "data": {},
            "meta": null,
            "ingest": { "tenant": ROOT, "actor": P },
            "partition": null,
            "sequence": null,
            "sequence_time": null,
        }),
        "row 1, static-authz: the stored event"
    );
    assert_eq!(
        harness.outbox_bodies().await,
        1,
        "row 1, static-authz: the event is enqueued"
    );
}

#[tokio::test]
async fn policy_authz_refuses_a_publish_to_a_tenant_outside_the_callers_subtree() {
    let harness = Pdp::PolicyAuthZ.harness(&[]).await;

    let result = harness
        .ingest()
        .publish_event(&tenant_user(), request(0x2_01, T999))
        .await;

    let Err(DomainError::Forbidden { code, .. }) = &result else {
        panic!("row 2, PolicyAuthZ: expected Forbidden, got {result:?}");
    };
    assert_eq!(
        *code,
        ErrorCode::TenantIdNotAuthorized,
        "row 2, PolicyAuthZ: the refusal code"
    );
    assert_eq!(
        harness.outbox_bodies().await,
        0,
        "row 2, PolicyAuthZ: nothing is enqueued"
    );
}

#[tokio::test]
async fn tr_authz_refuses_a_publish_to_a_tenant_outside_the_callers_subtree() {
    let harness = Pdp::TrAuthz.harness(&[]).await;

    let result = harness
        .ingest()
        .publish_event(&tenant_user(), request(0x2_01, T999))
        .await;

    let Err(DomainError::Forbidden { code, .. }) = &result else {
        panic!("row 2, tr-authz: expected Forbidden, got {result:?}");
    };
    assert_eq!(
        *code,
        ErrorCode::TenantIdNotAuthorized,
        "row 2, tr-authz: the refusal code"
    );
    assert_eq!(
        harness.outbox_bodies().await,
        0,
        "row 2, tr-authz: nothing is enqueued"
    );
}

#[tokio::test]
async fn static_authz_has_no_tree_and_lets_a_publish_to_a_tenant_outside_the_callers_subtree_through()
 {
    let harness = Pdp::StaticAuthz.harness(&[]).await;

    // static-authz allows any non-nil target tenant: it has no tenant tree.
    let result = harness
        .ingest()
        .publish_event(&tenant_user(), request(0x2_01, T999))
        .await;

    let Ok(PublishAck::Accepted(event)) = result else {
        panic!("row 2, static-authz: expected Accepted, got {result:?}");
    };
    assert_eq!(
        serde_json::to_value(&event).unwrap(),
        json!({
            "id": Uuid::from_u128(0x2_01),
            "type": EVENT_TYPE,
            "topic": TOPIC,
            "subject_tenant": T999,
            "source": "authz-pdp-test",
            "subject": "s1",
            "subject_type": SUBJECT_TYPE,
            "occurred_at": "2026-11-12T10:00:00Z",
            "trace_parent": null,
            "data": {},
            "meta": null,
            "ingest": { "tenant": T234, "actor": U },
            "partition": null,
            "sequence": null,
            "sequence_time": null,
        }),
        "row 2, static-authz: the stored event"
    );
    assert_eq!(
        harness.outbox_bodies().await,
        1,
        "row 2, static-authz: the event is enqueued"
    );
}

#[tokio::test]
async fn policy_authz_refuses_a_publish_to_a_barrier_tenant_without_the_barrier_grant() {
    let harness = Pdp::PolicyAuthZ.harness(&[]).await;

    let result = harness
        .ingest()
        .publish_event(&platform_publisher(), request(0x3_01, B))
        .await;

    let Err(DomainError::Forbidden { code, .. }) = &result else {
        panic!("row 3, PolicyAuthZ: expected Forbidden, got {result:?}");
    };
    assert_eq!(
        *code,
        ErrorCode::TenantIdNotAuthorized,
        "row 3, PolicyAuthZ: the refusal code"
    );
    assert_eq!(
        harness.outbox_bodies().await,
        0,
        "row 3, PolicyAuthZ: nothing is enqueued"
    );
}

#[tokio::test]
async fn tr_authz_lets_a_publish_to_a_barrier_tenant_through_without_the_barrier_grant() {
    let harness = Pdp::TrAuthz.harness(&[]).await;

    // tr-authz grants barrier-ignoring reach without checking the action (#5830).
    let result = harness
        .ingest()
        .publish_event(&platform_publisher(), request(0x3_01, B))
        .await;

    let Ok(PublishAck::Accepted(event)) = result else {
        panic!("row 3, tr-authz: expected Accepted, got {result:?}");
    };
    assert_eq!(
        serde_json::to_value(&event).unwrap(),
        json!({
            "id": Uuid::from_u128(0x3_01),
            "type": EVENT_TYPE,
            "topic": TOPIC,
            "subject_tenant": B,
            "source": "authz-pdp-test",
            "subject": "s1",
            "subject_type": SUBJECT_TYPE,
            "occurred_at": "2026-11-12T10:00:00Z",
            "trace_parent": null,
            "data": {},
            "meta": null,
            "ingest": { "tenant": ROOT, "actor": P },
            "partition": null,
            "sequence": null,
            "sequence_time": null,
        }),
        "row 3, tr-authz: the stored event"
    );
    assert_eq!(
        harness.outbox_bodies().await,
        1,
        "row 3, tr-authz: the event is enqueued"
    );
}

#[tokio::test]
async fn static_authz_has_no_tree_and_lets_a_publish_to_a_barrier_tenant_through_without_the_barrier_grant()
 {
    let harness = Pdp::StaticAuthz.harness(&[]).await;

    // static-authz allows any non-nil target tenant: it has no tenant tree.
    let result = harness
        .ingest()
        .publish_event(&platform_publisher(), request(0x3_01, B))
        .await;

    let Ok(PublishAck::Accepted(event)) = result else {
        panic!("row 3, static-authz: expected Accepted, got {result:?}");
    };
    assert_eq!(
        serde_json::to_value(&event).unwrap(),
        json!({
            "id": Uuid::from_u128(0x3_01),
            "type": EVENT_TYPE,
            "topic": TOPIC,
            "subject_tenant": B,
            "source": "authz-pdp-test",
            "subject": "s1",
            "subject_type": SUBJECT_TYPE,
            "occurred_at": "2026-11-12T10:00:00Z",
            "trace_parent": null,
            "data": {},
            "meta": null,
            "ingest": { "tenant": ROOT, "actor": P },
            "partition": null,
            "sequence": null,
            "sequence_time": null,
        }),
        "row 3, static-authz: the stored event"
    );
    assert_eq!(
        harness.outbox_bodies().await,
        1,
        "row 3, static-authz: the event is enqueued"
    );
}

#[tokio::test]
async fn policy_authz_lets_the_platform_publisher_publish_to_a_barrier_tenant_with_the_barrier_grant()
 {
    let harness = Pdp::PolicyAuthZ.harness(&[P]).await;

    let result = harness
        .ingest()
        .publish_event(&platform_publisher(), request(0x4_01, B))
        .await;

    let Ok(PublishAck::Accepted(event)) = result else {
        panic!("row 4, PolicyAuthZ: expected Accepted, got {result:?}");
    };
    assert_eq!(
        serde_json::to_value(&event).unwrap(),
        json!({
            "id": Uuid::from_u128(0x4_01),
            "type": EVENT_TYPE,
            "topic": TOPIC,
            "subject_tenant": B,
            "source": "authz-pdp-test",
            "subject": "s1",
            "subject_type": SUBJECT_TYPE,
            "occurred_at": "2026-11-12T10:00:00Z",
            "trace_parent": null,
            "data": {},
            "meta": null,
            "ingest": { "tenant": ROOT, "actor": P },
            "partition": null,
            "sequence": null,
            "sequence_time": null,
        }),
        "row 4, PolicyAuthZ: the stored event"
    );
    assert_eq!(
        harness.outbox_bodies().await,
        1,
        "row 4, PolicyAuthZ: the event is enqueued"
    );
}

#[tokio::test]
async fn tr_authz_lets_the_platform_publisher_publish_to_a_barrier_tenant_with_the_barrier_grant() {
    let harness = Pdp::TrAuthz.harness(&[P]).await;

    let result = harness
        .ingest()
        .publish_event(&platform_publisher(), request(0x4_01, B))
        .await;

    let Ok(PublishAck::Accepted(event)) = result else {
        panic!("row 4, tr-authz: expected Accepted, got {result:?}");
    };
    assert_eq!(
        serde_json::to_value(&event).unwrap(),
        json!({
            "id": Uuid::from_u128(0x4_01),
            "type": EVENT_TYPE,
            "topic": TOPIC,
            "subject_tenant": B,
            "source": "authz-pdp-test",
            "subject": "s1",
            "subject_type": SUBJECT_TYPE,
            "occurred_at": "2026-11-12T10:00:00Z",
            "trace_parent": null,
            "data": {},
            "meta": null,
            "ingest": { "tenant": ROOT, "actor": P },
            "partition": null,
            "sequence": null,
            "sequence_time": null,
        }),
        "row 4, tr-authz: the stored event"
    );
    assert_eq!(
        harness.outbox_bodies().await,
        1,
        "row 4, tr-authz: the event is enqueued"
    );
}

#[tokio::test]
async fn static_authz_lets_the_platform_publisher_publish_to_a_barrier_tenant_with_the_barrier_grant()
 {
    let harness = Pdp::StaticAuthz.harness(&[P]).await;

    let result = harness
        .ingest()
        .publish_event(&platform_publisher(), request(0x4_01, B))
        .await;

    let Ok(PublishAck::Accepted(event)) = result else {
        panic!("row 4, static-authz: expected Accepted, got {result:?}");
    };
    assert_eq!(
        serde_json::to_value(&event).unwrap(),
        json!({
            "id": Uuid::from_u128(0x4_01),
            "type": EVENT_TYPE,
            "topic": TOPIC,
            "subject_tenant": B,
            "source": "authz-pdp-test",
            "subject": "s1",
            "subject_type": SUBJECT_TYPE,
            "occurred_at": "2026-11-12T10:00:00Z",
            "trace_parent": null,
            "data": {},
            "meta": null,
            "ingest": { "tenant": ROOT, "actor": P },
            "partition": null,
            "sequence": null,
            "sequence_time": null,
        }),
        "row 4, static-authz: the stored event"
    );
    assert_eq!(
        harness.outbox_bodies().await,
        1,
        "row 4, static-authz: the event is enqueued"
    );
}

#[tokio::test]
async fn policy_authz_refuses_a_publish_outside_the_callers_subtree_even_with_the_barrier_grant() {
    let harness = Pdp::PolicyAuthZ.harness(&[U]).await;

    let result = harness
        .ingest()
        .publish_event(&tenant_user(), request(0x5_01, T999))
        .await;

    let Err(DomainError::Forbidden { code, .. }) = &result else {
        panic!("row 5, PolicyAuthZ: expected Forbidden, got {result:?}");
    };
    assert_eq!(
        *code,
        ErrorCode::TenantIdNotAuthorized,
        "row 5, PolicyAuthZ: the refusal code"
    );
    assert_eq!(
        harness.outbox_bodies().await,
        0,
        "row 5, PolicyAuthZ: nothing is enqueued"
    );
}

#[tokio::test]
async fn tr_authz_refuses_a_publish_outside_the_callers_subtree_even_with_the_barrier_grant() {
    let harness = Pdp::TrAuthz.harness(&[U]).await;

    let result = harness
        .ingest()
        .publish_event(&tenant_user(), request(0x5_01, T999))
        .await;

    let Err(DomainError::Forbidden { code, .. }) = &result else {
        panic!("row 5, tr-authz: expected Forbidden, got {result:?}");
    };
    assert_eq!(
        *code,
        ErrorCode::TenantIdNotAuthorized,
        "row 5, tr-authz: the refusal code"
    );
    assert_eq!(
        harness.outbox_bodies().await,
        0,
        "row 5, tr-authz: nothing is enqueued"
    );
}

#[tokio::test]
async fn static_authz_has_no_tree_and_lets_a_publish_outside_the_callers_subtree_through_with_the_barrier_grant()
 {
    let harness = Pdp::StaticAuthz.harness(&[U]).await;

    // static-authz allows any non-nil target tenant: it has no tenant tree.
    let result = harness
        .ingest()
        .publish_event(&tenant_user(), request(0x5_01, T999))
        .await;

    let Ok(PublishAck::Accepted(event)) = result else {
        panic!("row 5, static-authz: expected Accepted, got {result:?}");
    };
    assert_eq!(
        serde_json::to_value(&event).unwrap(),
        json!({
            "id": Uuid::from_u128(0x5_01),
            "type": EVENT_TYPE,
            "topic": TOPIC,
            "subject_tenant": T999,
            "source": "authz-pdp-test",
            "subject": "s1",
            "subject_type": SUBJECT_TYPE,
            "occurred_at": "2026-11-12T10:00:00Z",
            "trace_parent": null,
            "data": {},
            "meta": null,
            "ingest": { "tenant": T234, "actor": U },
            "partition": null,
            "sequence": null,
            "sequence_time": null,
        }),
        "row 5, static-authz: the stored event"
    );
    assert_eq!(
        harness.outbox_bodies().await,
        1,
        "row 5, static-authz: the event is enqueued"
    );
}

#[tokio::test]
async fn policy_authz_refuses_a_publish_with_a_nil_event_tenant_before_any_decision() {
    let harness = Pdp::PolicyAuthZ.harness(&[]).await;

    let result = harness
        .ingest()
        .publish_event(&platform_publisher(), request(0x66, Uuid::nil()))
        .await;

    let Err(DomainError::Validation { code, message }) = &result else {
        panic!("row 6, PolicyAuthZ: expected Validation, got {result:?}");
    };
    assert_eq!(
        (*code, message.as_str()),
        (
            ErrorCode::InvalidTenant,
            "event 0: the event's tenant must not be the nil UUID"
        ),
        "row 6, PolicyAuthZ: the refusal"
    );
    assert_eq!(
        harness.outbox_bodies().await,
        0,
        "row 6, PolicyAuthZ: nothing is enqueued"
    );
}

#[tokio::test]
async fn tr_authz_refuses_a_publish_with_a_nil_event_tenant_before_any_decision() {
    let harness = Pdp::TrAuthz.harness(&[]).await;

    let result = harness
        .ingest()
        .publish_event(&platform_publisher(), request(0x66, Uuid::nil()))
        .await;

    let Err(DomainError::Validation { code, message }) = &result else {
        panic!("row 6, tr-authz: expected Validation, got {result:?}");
    };
    assert_eq!(
        (*code, message.as_str()),
        (
            ErrorCode::InvalidTenant,
            "event 0: the event's tenant must not be the nil UUID"
        ),
        "row 6, tr-authz: the refusal"
    );
    assert_eq!(
        harness.outbox_bodies().await,
        0,
        "row 6, tr-authz: nothing is enqueued"
    );
}

#[tokio::test]
async fn static_authz_refuses_a_publish_with_a_nil_event_tenant_before_any_decision() {
    let harness = Pdp::StaticAuthz.harness(&[]).await;

    let result = harness
        .ingest()
        .publish_event(&platform_publisher(), request(0x66, Uuid::nil()))
        .await;

    let Err(DomainError::Validation { code, message }) = &result else {
        panic!("row 6, static-authz: expected Validation, got {result:?}");
    };
    assert_eq!(
        (*code, message.as_str()),
        (
            ErrorCode::InvalidTenant,
            "event 0: the event's tenant must not be the nil UUID"
        ),
        "row 6, static-authz: the refusal"
    );
    assert_eq!(
        harness.outbox_bodies().await,
        0,
        "row 6, static-authz: nothing is enqueued"
    );
}

#[tokio::test]
async fn policy_authz_refuses_a_batch_with_one_event_outside_the_callers_subtree_and_enqueues_nothing()
 {
    let harness = Pdp::PolicyAuthZ.harness(&[]).await;

    let result = harness
        .ingest()
        .publish_batch(
            &tenant_user(),
            vec![
                request(0x7_01, T234),
                request(0x7_02, T999),
                request(0x7_03, T234),
            ],
        )
        .await;

    let Err(DomainError::Forbidden { code, .. }) = &result else {
        panic!("row 7, PolicyAuthZ: expected Forbidden, got {result:?}");
    };
    assert_eq!(
        *code,
        ErrorCode::TenantIdNotAuthorized,
        "row 7, PolicyAuthZ: the refusal code"
    );
    assert_eq!(
        harness.outbox_bodies().await,
        0,
        "row 7, PolicyAuthZ: nothing is enqueued"
    );
}

#[tokio::test]
async fn tr_authz_refuses_a_batch_with_one_event_outside_the_callers_subtree_and_enqueues_nothing()
{
    let harness = Pdp::TrAuthz.harness(&[]).await;

    let result = harness
        .ingest()
        .publish_batch(
            &tenant_user(),
            vec![
                request(0x7_01, T234),
                request(0x7_02, T999),
                request(0x7_03, T234),
            ],
        )
        .await;

    let Err(DomainError::Forbidden { code, .. }) = &result else {
        panic!("row 7, tr-authz: expected Forbidden, got {result:?}");
    };
    assert_eq!(
        *code,
        ErrorCode::TenantIdNotAuthorized,
        "row 7, tr-authz: the refusal code"
    );
    assert_eq!(
        harness.outbox_bodies().await,
        0,
        "row 7, tr-authz: nothing is enqueued"
    );
}

#[tokio::test]
async fn static_authz_has_no_tree_and_lets_a_batch_with_one_event_outside_the_callers_subtree_through()
 {
    let harness = Pdp::StaticAuthz.harness(&[]).await;

    let result = harness
        .ingest()
        .publish_batch(
            &tenant_user(),
            vec![
                request(0x7_01, T234),
                request(0x7_02, T999),
                request(0x7_03, T234),
            ],
        )
        .await;

    assert!(
        matches!(result, Ok(BatchAck::Accepted)),
        "row 7, static-authz: expected the batch to be accepted, got {result:?}"
    );
    assert_eq!(
        harness.outbox_bodies().await,
        3,
        "row 7, static-authz: the three events are enqueued"
    );
}
