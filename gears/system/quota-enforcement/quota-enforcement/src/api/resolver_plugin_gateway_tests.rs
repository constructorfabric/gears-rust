#![allow(clippy::expect_used)]

use std::collections::BTreeMap;
use std::sync::Arc;

use quota_enforcement_sdk::{
    Actor, EnforcementMode, MetricId, QuotaDraft, QuotaEnforcementStoragePluginV1 as _,
    QuotaSource, QuotaType, SubjectRef,
};
use quota_resolver::domain::{NoopMetrics, QuotaResolverLocalClient, Service as Resolver};
use quota_resolver_sdk::models::{
    ChargeOperation, ChargeRef, LeaseOutcome, MetricAmount, MetricItem, MetricObject,
    PlatformAcquireLeaseRequest, PlatformCommitLeaseRequest, PlatformDebitRequest,
    PlatformReturnRequest, PrincipalSubject, QuotaStateMetric, QuotaStateRequest, SubjectInput,
};
use quota_resolver_sdk::{
    PlatformQuotaResolverApi, QuotaResolverApi, QuotaResolverPluginClient,
    QuotaResolverPluginSpecV1,
};
use serde_json::{Map, json};
use toolkit::client_hub::ClientHub;
use toolkit::gts::PluginV1;
use toolkit::plugins::bind_plugin_instances;
use toolkit_security::{PlatformSecurityContext, SecurityContext};
use uuid::Uuid;

use super::ResolverPlugin;
use crate::gear::RESOLVER_PLUGIN_SEGMENT;
use crate::test_support::{
    METRIC_TOKENS, USER_SUBJECT_TYPE, UncallablePdp, bound_service_with_registry, mrw_storage,
    platform_registry, tenant,
};

const PRINCIPAL_TYPE: &str = "gts.cf.core.qetest.principal.v1~cf.core.qetest.user.v1~";

fn principal() -> Uuid {
    Uuid::from_u128(0x5eed_0000_0000_0000_0000_0000_0000_0042)
}

fn subjects() -> Vec<SubjectInput> {
    vec![SubjectInput::Principal(PrincipalSubject {
        subject_type: PRINCIPAL_TYPE.to_owned(),
        id: principal(),
    })]
}

fn items(amount: i64) -> Vec<MetricItem> {
    vec![MetricItem {
        metric: MetricObject {
            metric_type: METRIC_TOKENS.to_owned(),
            id: None,
            metadata: Map::new(),
            extra: Map::new(),
        },
        amount,
    }]
}

#[tokio::test]
async fn the_gateway_finds_the_qe_plugin_and_runs_every_operation_through_it() {
    let storage = mrw_storage().await;
    let quota = storage
        .create_quota(
            &crate::test_support::actor(),
            &toolkit_security::AccessScope::allow_all(),
            QuotaDraft {
                tenant_id: tenant(),
                subject: SubjectRef {
                    subject_type: gts::GtsTypeId::try_new(USER_SUBJECT_TYPE).expect("type"),
                    subject_id: principal().to_string(),
                },
                metric: MetricId::parse(METRIC_TOKENS).expect("metric"),
                quota_type: QuotaType::Allocation,
                period: None,
                enforcement_mode: EnforcementMode::Hard,
                cap: Some(100),
                notification_thresholds: Vec::new(),
                validity_window: None,
                fail_open_hint: false,
                metadata: Map::new(),
                source: QuotaSource::Operator,
            },
            &[],
        )
        .await
        .expect("quota");
    let registry = platform_registry();
    let hub = Arc::new(ClientHub::new());
    registry.install(&hub);
    let service =
        bound_service_with_registry(Arc::new(UncallablePdp), Arc::clone(&storage), &registry).await;
    let (id, document) = PluginV1::<QuotaResolverPluginSpecV1>::build_registration(
        RESOLVER_PLUGIN_SEGMENT,
        "constructorfabric",
        100,
    )
    .expect("instance");
    registry.seed(id.as_ref(), document.clone());
    let plugin: Arc<dyn QuotaResolverPluginClient> = Arc::new(ResolverPlugin::new(service, 100));
    bind_plugin_instances::<QuotaResolverPluginSpecV1, _>(
        &hub,
        [(id.as_ref().to_owned(), document)].iter(),
        &plugin,
    )
    .expect("bound");
    let resolver = QuotaResolverLocalClient::new(Arc::new(Resolver::new(
        hub,
        "constructorfabric".to_owned(),
        BTreeMap::from([(PRINCIPAL_TYPE.to_owned(), USER_SUBJECT_TYPE.to_owned())]),
        Arc::new(NoopMetrics),
    )));
    let platform = PlatformSecurityContext::outbound_marker;

    let decision = PlatformQuotaResolverApi::debit(
        &resolver,
        platform(),
        PlatformDebitRequest {
            tenant_id: tenant().as_uuid(),
            subjects: subjects(),
            items: items(10),
            idempotency_key: "d-1".to_owned(),
        },
    )
    .await
    .expect("debit");
    assert!(decision.allowed, "{decision:?}");
    let LeaseOutcome::Acquired { token, .. } = resolver
        .acquire_lease(
            platform(),
            PlatformAcquireLeaseRequest {
                tenant_id: tenant().as_uuid(),
                subjects: subjects(),
                items: items(5),
                ttl_secs: 60,
                idempotency_key: "a-1".to_owned(),
            },
        )
        .await
        .expect("acquire")
    else {
        panic!("denied");
    };
    resolver
        .commit_lease(
            platform(),
            PlatformCommitLeaseRequest {
                tenant_id: tenant().as_uuid(),
                token,
                amounts: Some(vec![MetricAmount {
                    metric_type: METRIC_TOKENS.to_owned(),
                    amount: 3,
                }]),
                idempotency_key: "c-1".to_owned(),
            },
        )
        .await
        .expect("commit");
    resolver
        .return_amount(
            platform(),
            PlatformReturnRequest {
                tenant_id: tenant().as_uuid(),
                charge: ChargeRef {
                    operation: ChargeOperation::Debit,
                    idempotency_key: "d-1".to_owned(),
                },
                amounts: vec![MetricAmount {
                    metric_type: METRIC_TOKENS.to_owned(),
                    amount: 4,
                }],
                idempotency_key: "ret-1".to_owned(),
            },
        )
        .await
        .expect("return");
    let end_user = SecurityContext::builder()
        .subject_id(principal())
        .subject_tenant_id(tenant().as_uuid())
        .subject_type(PRINCIPAL_TYPE)
        .build()
        .expect("end user");
    let page = QuotaResolverApi::quota_state(
        &resolver,
        end_user,
        QuotaStateRequest {
            metrics: vec![QuotaStateMetric::of(METRIC_TOKENS)],
            limit: None,
            cursor: None,
        },
    )
    .await
    .expect("quota state");

    assert_eq!(storage.consumed(quota), 9, "10 - 4 + 3");
    assert_eq!(page.items.len(), 1);
    assert_eq!(
        serde_json::to_value(&page.items[0]).expect("json"),
        json!({
            "quota_id": quota.to_string(),
            "subject": { "type": USER_SUBJECT_TYPE, "id": principal().to_string() },
            "metric_type": METRIC_TOKENS,
            "cap": 100,
            "consumed": 9,
            "remaining": 91,
        })
    );
    assert!(
        storage.operation_log_actors()[1..]
            .iter()
            .all(|actor| *actor == Actor::Platform { identity: None }),
        "{:?}",
        storage.operation_log_actors()
    );
}
