#![allow(clippy::expect_used)]

use std::collections::BTreeMap;
use std::sync::Arc;

use quota_enforcement_sdk::testing::InMemoryStorage;
use quota_enforcement_sdk::{
    AcquireLeaseOutcome, Actor, Decision, DecisionResult, EnforcementMode, LeaseToken, MetricId,
    PeriodWindow, QuotaDraft, QuotaEnforcementStoragePluginV1, QuotaId, QuotaSnapshot, QuotaSource,
    QuotaType, SubjectRef, ValidityWindow,
};
use quota_resolver_sdk::QuotaResolverPluginClient;
use quota_resolver_sdk::models::{
    ChargeOperation, ChargeRef, DenialReason, LeaseOutcome, MetricAmount, MetricItem, MetricObject,
    PlatformCommitLeaseRequest, PlatformReleaseLeaseRequest, PlatformReturnRequest,
    PluginAcquireLeaseRequest, PluginDebitRequest, PluginQuotaStateRequest, QuotaStateMetric,
    QuotaSubject,
};
use serde_json::{Map, json};
use time::macros::datetime;
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::{AccessScope, PlatformIdentity};
use uuid::Uuid;

use super::{
    DIAGNOSTIC_REASON, DIAGNOSTIC_VIOLATED_QUOTA_IDS, ResolverPlugin, attribution_of,
    lease_outcome, lease_token, quota_decision, quota_state, single_amount, snapshot_request,
};
use crate::domain::error::DomainError;
use crate::domain::tokens;
use crate::test_support::{
    DEVICE_SUBJECT_TYPE, METRIC_OTHER, METRIC_REQUESTS, METRIC_TOKENS, PermitTenantsPdp,
    TENANT_SUBJECT_TYPE, USER_SUBJECT_TYPE, actor, bound_service_with_registry, caller,
    metric_type_document, mrw_storage, platform_registry, tenant,
};

fn other_tenant() -> Uuid {
    Uuid::from_u128(0x7e57_0000_0000_0000_0000_0000_0000_0002)
}

fn billing() -> PlatformIdentity {
    PlatformIdentity::Shared {
        name: "billing".to_owned(),
    }
}

fn user(id: &str) -> QuotaSubject {
    QuotaSubject {
        subject_type: USER_SUBJECT_TYPE.to_owned(),
        id: id.to_owned(),
    }
}

fn item(metric: &str, amount: i64) -> MetricItem {
    MetricItem {
        metric: MetricObject {
            metric_type: metric.to_owned(),
            id: None,
            metadata: json!({ "region": "eu-west-1" })
                .as_object()
                .cloned()
                .expect("object"),
            extra: Map::new(),
        },
        amount,
    }
}

fn amount(metric: &str, amount: i64) -> MetricAmount {
    MetricAmount {
        metric_type: metric.to_owned(),
        amount,
    }
}

fn debit(key: &str, amount: i64) -> PluginDebitRequest {
    PluginDebitRequest {
        tenant_id: tenant().as_uuid(),
        subjects: vec![user("u-1")],
        items: vec![item(METRIC_TOKENS, amount)],
        idempotency_key: key.to_owned(),
        caller: Some(billing()),
    }
}

fn acquire(key: &str, amount: i64) -> PluginAcquireLeaseRequest {
    PluginAcquireLeaseRequest {
        tenant_id: tenant().as_uuid(),
        subjects: vec![user("u-1")],
        items: vec![item(METRIC_TOKENS, amount)],
        ttl_secs: 60,
        idempotency_key: key.to_owned(),
        caller: Some(billing()),
    }
}

fn commit(
    token: &str,
    amounts: Option<Vec<MetricAmount>>,
    key: &str,
) -> PlatformCommitLeaseRequest {
    PlatformCommitLeaseRequest {
        tenant_id: tenant().as_uuid(),
        token: token.to_owned(),
        amounts,
        idempotency_key: key.to_owned(),
    }
}

fn release(token: &str, key: &str) -> PlatformReleaseLeaseRequest {
    PlatformReleaseLeaseRequest {
        tenant_id: tenant().as_uuid(),
        token: token.to_owned(),
        idempotency_key: key.to_owned(),
    }
}

fn give_back(charge_key: &str, amounts: Vec<MetricAmount>, key: &str) -> PlatformReturnRequest {
    PlatformReturnRequest {
        tenant_id: tenant().as_uuid(),
        charge: ChargeRef {
            operation: ChargeOperation::Debit,
            idempotency_key: charge_key.to_owned(),
        },
        amounts,
        idempotency_key: key.to_owned(),
    }
}

fn state(subject: QuotaSubject, metrics: &[&str], limit: Option<u32>) -> PluginQuotaStateRequest {
    PluginQuotaStateRequest {
        tenant_id: tenant().as_uuid(),
        subject,
        metrics: metrics.iter().copied().map(QuotaStateMetric::of).collect(),
        limit,
        cursor: None,
        caller: Some(billing()),
    }
}

fn draft(subject_type: &str, subject_id: &str, cap: u64) -> QuotaDraft {
    QuotaDraft {
        tenant_id: tenant(),
        subject: SubjectRef {
            subject_type: gts::GtsTypeId::try_new(subject_type).expect("type"),
            subject_id: subject_id.to_owned(),
        },
        metric: MetricId::parse(METRIC_TOKENS).expect("metric"),
        quota_type: QuotaType::Allocation,
        period: None,
        enforcement_mode: EnforcementMode::Hard,
        cap: Some(cap),
        notification_thresholds: Vec::new(),
        validity_window: None,
        fail_open_hint: false,
        metadata: json!({ "region": ["eu-west-1"] })
            .as_object()
            .cloned()
            .expect("object"),
        source: QuotaSource::Operator,
    }
}

async fn create(storage: &InMemoryStorage, draft: QuotaDraft) -> QuotaId {
    storage
        .create_quota(&actor(), &AccessScope::allow_all(), draft, &[])
        .await
        .expect("quota")
}

struct Fixture {
    plugin: Arc<dyn QuotaResolverPluginClient>,
    service: Arc<crate::domain::Service>,
    storage: Arc<InMemoryStorage>,
    pdp: Arc<PermitTenantsPdp>,
    registry: Arc<types_registry_sdk::testing_platform::MockTypesRegistry>,
    quota: QuotaId,
}

async fn fixture() -> Fixture {
    let storage = mrw_storage().await;
    let quota = create(&storage, draft(USER_SUBJECT_TYPE, "u-1", 100)).await;
    let pdp = Arc::new(PermitTenantsPdp::new(vec![tenant().as_uuid()]));
    let registry = platform_registry();
    let service = bound_service_with_registry(pdp.clone(), Arc::clone(&storage), &registry).await;
    Fixture {
        plugin: Arc::new(ResolverPlugin::new(Arc::clone(&service), 100)),
        service,
        storage,
        pdp,
        registry,
        quota,
    }
}

fn token_of(outcome: LeaseOutcome) -> String {
    match outcome {
        LeaseOutcome::Acquired { token, .. } => token,
        other => panic!("not acquired: {other:?}"),
    }
}

/// A subject type of the right form that nobody registered.
const UNREGISTERED_SUBJECT_TYPE: &str = "gts.cf.core.quota.subj.v1~cf.qetest.subject.other.v1~";

fn is_invalid(err: &CanonicalError, reason: &str) -> bool {
    matches!(err, CanonicalError::InvalidArgument { .. }) && format!("{err:?}").contains(reason)
}

// --- request conversion ---------------------------------------------------------

#[test]
fn a_single_metric_item_becomes_the_qe_metric_and_amount_unchanged() {
    let mut single = item(METRIC_TOKENS, 7);
    single.metric.id = Some("gpt-4".to_owned());

    let (attribution, amount) =
        attribution_of(tenant().as_uuid(), vec![user("u-1")], vec![single.clone()])
            .expect("one item");

    assert_eq!(amount, 7);
    assert_eq!(attribution.tenant_id, tenant());
    assert_eq!(attribution.subjects.len(), 1);
    assert_eq!(attribution.subjects[0].subject_type, USER_SUBJECT_TYPE);
    assert_eq!(attribution.subjects[0].id, "u-1");
    assert_eq!(attribution.metric.metric_type, METRIC_TOKENS);
    assert_eq!(attribution.metric.id.as_deref(), Some("gpt-4"));
    assert_eq!(attribution.metric.metadata, Some(single.metric.metadata));
}

#[test]
fn several_items_none_or_an_unknown_metric_field_is_invalid() {
    for items in [
        vec![],
        vec![item(METRIC_TOKENS, 1), item(METRIC_REQUESTS, 1)],
    ] {
        let err =
            attribution_of(tenant().as_uuid(), vec![user("u-1")], items).expect_err("not one item");
        assert!(
            matches!(err, DomainError::InvalidArgument { reason, .. } if reason == tokens::ONE_METRIC_ITEM_REQUIRED),
            "{err:?}"
        );
    }
    let mut extended = item(METRIC_TOKENS, 1);
    extended.metric.extra.insert("unit".to_owned(), json!("k"));
    let err = attribution_of(tenant().as_uuid(), vec![user("u-1")], vec![extended])
        .expect_err("closed metric object");
    assert!(
        matches!(err, DomainError::InvalidArgument { reason, .. } if reason == tokens::METRIC_INVALID),
        "{err:?}"
    );
}

#[test]
fn a_commit_or_return_takes_exactly_one_amount() {
    assert_eq!(
        single_amount(vec![amount(METRIC_TOKENS, 3)]).expect("one"),
        amount(METRIC_TOKENS, 3)
    );
    for amounts in [
        Vec::new(),
        vec![amount(METRIC_TOKENS, 3), amount(METRIC_REQUESTS, 1)],
    ] {
        let err = single_amount(amounts).expect_err("not one");
        assert!(
            matches!(err, DomainError::InvalidArgument { reason, .. } if reason == tokens::ONE_METRIC_ITEM_REQUIRED),
            "{err:?}"
        );
    }
}

#[test]
fn a_token_that_is_not_a_uuid_names_no_lease() {
    let id = Uuid::now_v7();
    assert_eq!(
        lease_token(&id.to_string()).expect("uuid"),
        LeaseToken::new(id)
    );
    assert!(matches!(
        lease_token("lease-1"),
        Err(DomainError::NotFound { .. })
    ));
}

#[test]
fn a_quota_state_read_is_one_snapshot_target_per_metric_with_paging_unchanged() {
    let request = snapshot_request(
        tenant().as_uuid(),
        &user("u-1"),
        vec![
            QuotaStateMetric::of(METRIC_TOKENS),
            QuotaStateMetric::of(METRIC_REQUESTS),
        ],
        Some(5),
        Some("c".to_owned()),
    );

    assert_eq!(request.tenant_id, tenant());
    assert_eq!(request.limit, Some(5));
    assert_eq!(request.cursor.as_deref(), Some("c"));
    let targets: Vec<_> = request
        .subjects
        .iter()
        .map(|target| {
            (
                target.subject_type.as_str(),
                target.id.as_str(),
                target.metric.as_str(),
            )
        })
        .collect();
    assert_eq!(
        targets,
        [
            (USER_SUBJECT_TYPE, "u-1", METRIC_TOKENS),
            (USER_SUBJECT_TYPE, "u-1", METRIC_REQUESTS)
        ]
    );
}

// --- result conversion ----------------------------------------------------------

fn denied(reason: &str, violated: Vec<QuotaId>) -> Decision {
    Decision {
        result: DecisionResult::Denied {
            violated_quota_ids: violated,
            reason: reason.to_owned(),
        },
        debit_plan: Vec::new(),
        diagnostics: BTreeMap::from([("engine".to_owned(), json!("cel"))]),
    }
}

#[test]
fn an_allowed_decision_is_allowed_with_the_engine_diagnostics() {
    let mut decision = Decision::allowed_with_plan(Vec::new());
    decision
        .diagnostics
        .insert("engine".to_owned(), json!("mrw"));

    let converted = quota_decision(decision);

    assert!(converted.allowed);
    assert_eq!(converted.reason, None);
    assert_eq!(converted.diagnostics.get("engine"), Some(&json!("mrw")));
}

#[test]
fn no_applicable_quota_is_its_own_reason_and_every_other_denial_is_quota_exceeded() {
    let quota = QuotaId::new(Uuid::from_u128(9));

    let none = quota_decision(denied(
        quota_enforcement_sdk::NO_APPLICABLE_QUOTA,
        Vec::new(),
    ));
    let cel = quota_decision(denied("REGION_BLOCKED", vec![quota]));

    assert!(!none.allowed);
    assert_eq!(none.reason, Some(DenialReason::NoApplicableQuota));
    assert_eq!(
        none.diagnostics.get(DIAGNOSTIC_REASON),
        Some(&json!("NO_APPLICABLE_QUOTA"))
    );
    assert_eq!(
        none.diagnostics.get(DIAGNOSTIC_VIOLATED_QUOTA_IDS),
        Some(&json!([]))
    );
    assert!(!cel.allowed);
    assert_eq!(cel.reason, Some(DenialReason::QuotaExceeded));
    assert_eq!(
        cel.diagnostics.get(DIAGNOSTIC_REASON),
        Some(&json!("REGION_BLOCKED"))
    );
    assert_eq!(
        cel.diagnostics.get(DIAGNOSTIC_VIOLATED_QUOTA_IDS),
        Some(&json!([quota.to_string()]))
    );
    assert_eq!(cel.diagnostics.get("engine"), Some(&json!("cel")));
}

#[test]
fn an_acquired_lease_carries_its_token_and_expiry_and_a_denial_its_decision() {
    let token = LeaseToken::new(Uuid::now_v7());
    let acquired = lease_outcome(AcquireLeaseOutcome::Acquired {
        token,
        expires_at: datetime!(2026-11-24 10:00:00.5 UTC),
    })
    .expect("in range");
    let refused = lease_outcome(AcquireLeaseOutcome::Denied {
        decision: denied("CAP_EXCEEDED", Vec::new()),
    })
    .expect("denial");

    assert_eq!(
        acquired,
        LeaseOutcome::Acquired {
            token: token.to_string(),
            expires_at: chrono::DateTime::parse_from_rfc3339("2026-11-24T10:00:00.5Z")
                .expect("time")
                .to_utc(),
        }
    );
    assert!(
        matches!(refused, LeaseOutcome::Denied { decision } if decision.reason == Some(DenialReason::QuotaExceeded))
    );
}

#[test]
fn a_snapshot_maps_to_quota_state_without_the_operator_fields() {
    let snapshot = QuotaSnapshot {
        quota_id: QuotaId::new(Uuid::from_u128(3)),
        subject: SubjectRef {
            subject_type: gts::GtsTypeId::try_new(USER_SUBJECT_TYPE).expect("type"),
            subject_id: "u-1".to_owned(),
        },
        metric: MetricId::parse(METRIC_TOKENS).expect("metric"),
        quota_type: QuotaType::Consumption,
        enforcement_mode: EnforcementMode::Hard,
        cap: Some(10),
        consumed: 4,
        remaining: Some(6),
        period: Some(PeriodWindow {
            start: datetime!(2026-11-24 00:00 UTC),
            end: datetime!(2026-11-25 00:00 UTC),
            next_reset: datetime!(2026-11-25 00:00 UTC),
        }),
        metadata: json!({ "region": ["eu-west-1"] })
            .as_object()
            .cloned()
            .expect("object"),
        validity_window: Some(ValidityWindow {
            start: None,
            end: Some(datetime!(2027-01-01 00:00 UTC)),
        }),
        currently_within_window: true,
    };

    let mapped = quota_state(snapshot).expect("in range");

    assert_eq!(
        serde_json::to_value(&mapped).expect("json"),
        json!({
            "quota_id": Uuid::from_u128(3).to_string(),
            "subject": { "type": USER_SUBJECT_TYPE, "id": "u-1" },
            "metric_type": METRIC_TOKENS,
            "cap": 10,
            "consumed": 4,
            "remaining": 6,
            "period": { "start": "2026-11-24T00:00:00Z", "resets_at": "2026-11-25T00:00:00Z" },
            "metadata": { "region": ["eu-west-1"] },
        }),
        "no enforcement mode or validity window"
    );
}

// --- through the plugin contract ------------------------------------------------

#[tokio::test]
async fn plugin_operations_make_no_pdp_call_while_the_api_still_calls_it() {
    let f = fixture().await;

    f.plugin.debit(debit("d-1", 10)).await.expect("debit");
    let token = token_of(
        f.plugin
            .acquire_lease(acquire("a-1", 5))
            .await
            .expect("acquire"),
    );
    f.plugin
        .commit_lease(commit(&token, None, "c-1"), Some(billing()))
        .await
        .expect("commit");
    let token = token_of(
        f.plugin
            .acquire_lease(acquire("a-2", 5))
            .await
            .expect("acquire"),
    );
    f.plugin
        .release_lease(release(&token, "r-1"), Some(billing()))
        .await
        .expect("release");
    f.plugin
        .return_amount(
            give_back("d-1", vec![amount(METRIC_TOKENS, 4)], "ret-1"),
            Some(billing()),
        )
        .await
        .expect("return");
    f.plugin
        .quota_state(state(user("u-1"), &[METRIC_TOKENS], None))
        .await
        .expect("quota state");
    assert_eq!(f.pdp.calls(), 0, "the plugin path calls no PDP");
    assert_eq!(f.storage.consumed(f.quota), 11, "10 - 4 + the 5 committed");

    let (attribution, _) = attribution_of(
        tenant().as_uuid(),
        vec![user("u-1")],
        vec![item(METRIC_TOKENS, 1)],
    )
    .expect("one item");
    let api = f.service.operations(caller()).expect("bound");
    api.debit(quota_enforcement_sdk::DebitRequest {
        attribution,
        amount: 1,
        idempotency_key: "d-api".to_owned(),
    })
    .await
    .expect("api debit");
    assert_eq!(f.pdp.calls(), 1, "QE's API keeps its PDP admission");
}

#[tokio::test]
async fn a_quota_state_metric_with_metadata_reads_only_the_quotas_that_apply_to_it() {
    let f = fixture().await;
    let read = |region: &str| {
        let mut request = state(user("u-1"), &[METRIC_TOKENS], None);
        request.metrics[0].metadata = json!({ "region": region }).as_object().cloned();
        let plugin = &f.plugin;
        async move {
            plugin
                .quota_state(request)
                .await
                .expect("quota state")
                .items
        }
    };
    // The fixture Quota is limited to `eu-west-1`.
    assert_eq!(read("eu-west-1").await.len(), 1);
    assert!(read("us-east-1").await.is_empty());
}

#[tokio::test]
async fn plugin_operations_reach_only_the_tenant_of_the_request() {
    let f = fixture().await;
    let token = token_of(
        f.plugin
            .acquire_lease(acquire("a-1", 5))
            .await
            .expect("acquire"),
    );
    let foreign = |mut request: PlatformCommitLeaseRequest| {
        request.tenant_id = other_tenant();
        request
    };

    let err = f
        .plugin
        .commit_lease(foreign(commit(&token, None, "c-1")), None)
        .await
        .expect_err("another tenant's token");
    assert!(matches!(err, CanonicalError::NotFound { .. }), "{err:?}");
    let err = f
        .plugin
        .release_lease(
            PlatformReleaseLeaseRequest {
                tenant_id: other_tenant(),
                ..release(&token, "r-1")
            },
            None,
        )
        .await
        .expect_err("another tenant's token");
    assert!(matches!(err, CanonicalError::NotFound { .. }), "{err:?}");

    assert!(
        f.storage.scopes().iter().all(|scope| *scope
            == AccessScope::for_tenant(tenant().as_uuid())
            || *scope == AccessScope::for_tenant(other_tenant())),
        "{:?}",
        f.storage.scopes()
    );
    assert_eq!(
        f.storage.scopes().last(),
        Some(&AccessScope::for_tenant(other_tenant()))
    );
    assert_eq!(f.storage.consumed(f.quota), 5, "the lease still holds");
}

#[tokio::test]
async fn a_return_naming_another_tenant_cannot_reach_its_charge() {
    let f = fixture().await;
    f.plugin.debit(debit("d-1", 10)).await.expect("debit");

    let err = f
        .plugin
        .return_amount(
            PlatformReturnRequest {
                tenant_id: other_tenant(),
                ..give_back("d-1", vec![amount(METRIC_TOKENS, 4)], "ret-1")
            },
            None,
        )
        .await
        .expect_err("another tenant's charge");

    assert!(matches!(err, CanonicalError::NotFound { .. }), "{err:?}");
    assert_eq!(
        f.storage.scopes().last(),
        Some(&AccessScope::for_tenant(other_tenant()))
    );
    assert_eq!(f.storage.consumed(f.quota), 10, "nothing was given back");
    f.plugin
        .return_amount(
            give_back("d-1", vec![amount(METRIC_TOKENS, 4)], "ret-1"),
            None,
        )
        .await
        .expect("the key stays free in the charge's own tenant");
    assert_eq!(f.storage.consumed(f.quota), 6);
}

#[tokio::test]
async fn the_operation_log_records_the_callers_platform_identity_or_no_module() {
    let f = fixture().await;
    f.plugin.debit(debit("d-1", 1)).await.expect("debit");
    f.plugin
        .debit(PluginDebitRequest {
            caller: None,
            ..debit("d-2", 1)
        })
        .await
        .expect("debit");

    assert_eq!(
        f.storage.operation_log_actors()[1..],
        [
            Actor::Platform {
                identity: Some(billing())
            },
            Actor::Platform { identity: None },
        ]
    );
}

#[tokio::test]
async fn acquire_returns_token_and_expiry_and_commit_without_amount_commits_the_hold() {
    let f = fixture().await;
    let before = chrono::Utc::now();

    let outcome = f
        .plugin
        .acquire_lease(acquire("a-1", 7))
        .await
        .expect("acquire");
    let LeaseOutcome::Acquired { token, expires_at } = outcome else {
        panic!("not acquired: {outcome:?}");
    };
    assert!(Uuid::parse_str(&token).is_ok(), "{token}");
    assert!(
        expires_at > before && expires_at <= chrono::Utc::now() + chrono::Duration::seconds(60),
        "{expires_at}"
    );

    f.plugin
        .commit_lease(commit(&token, None, "c-1"), None)
        .await
        .expect("commit");
    assert_eq!(f.storage.consumed(f.quota), 7);

    let token = token_of(
        f.plugin
            .acquire_lease(acquire("a-2", 7))
            .await
            .expect("acquire"),
    );
    f.plugin
        .commit_lease(
            commit(&token, Some(vec![amount(METRIC_TOKENS, 3)]), "c-2"),
            None,
        )
        .await
        .expect("commit of part");
    assert_eq!(f.storage.consumed(f.quota), 10);
}

#[tokio::test]
async fn a_quota_state_page_carries_no_operator_fields() {
    let f = fixture().await;

    let page = f
        .plugin
        .quota_state(state(user("u-1"), &[METRIC_TOKENS], None))
        .await
        .expect("quota state");

    assert_eq!(page.items.len(), 1);
    let json = serde_json::to_value(&page.items[0]).expect("json");
    assert_eq!(json["quota_id"], json!(f.quota.to_string()));
    for field in ["enforcement_mode", "validity_window"] {
        assert!(json.get(field).is_none(), "{field} in {json}");
    }
}

#[tokio::test]
async fn a_subject_type_the_metric_does_not_admit_or_two_of_one_type_is_invalid_on_a_write() {
    let f = fixture().await;
    let device = QuotaSubject {
        subject_type: DEVICE_SUBJECT_TYPE.to_owned(),
        id: "d-1".to_owned(),
    };

    let err = f
        .plugin
        .debit(PluginDebitRequest {
            subjects: vec![device.clone()],
            ..debit("d-1", 1)
        })
        .await
        .expect_err("not admitted");
    assert!(is_invalid(&err, tokens::METRIC_NOT_ADMITTED), "{err:?}");
    let err = f
        .plugin
        .acquire_lease(PluginAcquireLeaseRequest {
            subjects: vec![device],
            ..acquire("a-1", 1)
        })
        .await
        .expect_err("not admitted");
    assert!(is_invalid(&err, tokens::METRIC_NOT_ADMITTED), "{err:?}");
    let err = f
        .plugin
        .debit(PluginDebitRequest {
            subjects: vec![user("u-1"), user("u-2")],
            ..debit("d-2", 1)
        })
        .await
        .expect_err("two of one type");
    assert!(is_invalid(&err, tokens::SUBJECT_TYPE_DUPLICATE), "{err:?}");
    let err = f
        .plugin
        .acquire_lease(PluginAcquireLeaseRequest {
            subjects: vec![user("u-1"), user("u-2")],
            ..acquire("a-2", 1)
        })
        .await
        .expect_err("two of one type");
    assert!(is_invalid(&err, tokens::SUBJECT_TYPE_DUPLICATE), "{err:?}");
    assert_eq!(f.storage.consumed(f.quota), 0);
}

#[tokio::test]
async fn a_quota_state_read_for_a_type_the_metric_does_not_admit_returns_the_tenants_quotas() {
    let f = fixture().await;
    let shared = create(
        &f.storage,
        draft(TENANT_SUBJECT_TYPE, &tenant().to_string(), 50),
    )
    .await;

    let page = f
        .plugin
        .quota_state(state(
            QuotaSubject {
                subject_type: DEVICE_SUBJECT_TYPE.to_owned(),
                id: "d-1".to_owned(),
            },
            &[METRIC_TOKENS],
            None,
        ))
        .await
        .expect("narrowed, not refused");

    let ids: Vec<_> = page
        .items
        .iter()
        .map(|item| item.quota_id.clone())
        .collect();
    assert_eq!(ids, [shared.to_string()]);
}

#[tokio::test]
async fn quotas_outside_their_validity_window_are_left_out_and_pages_stay_full() {
    let f = fixture().await;
    for _ in 0..2 {
        create(
            &f.storage,
            QuotaDraft {
                validity_window: Some(ValidityWindow {
                    start: None,
                    end: Some(datetime!(2020-01-01 00:00 UTC)),
                }),
                ..draft(TENANT_SUBJECT_TYPE, &tenant().to_string(), 10)
            },
        )
        .await;
    }
    create(
        &f.storage,
        QuotaDraft {
            validity_window: Some(ValidityWindow {
                start: Some(datetime!(2999-01-01 00:00 UTC)),
                end: None,
            }),
            ..draft(USER_SUBJECT_TYPE, "u-1", 10)
        },
    )
    .await;
    let current = [
        f.quota,
        create(
            &f.storage,
            draft(TENANT_SUBJECT_TYPE, &tenant().to_string(), 10),
        )
        .await,
    ];

    let first = f
        .plugin
        .quota_state(state(user("u-1"), &[METRIC_TOKENS], Some(1)))
        .await
        .expect("page 1");
    let second = f
        .plugin
        .quota_state(PluginQuotaStateRequest {
            cursor: first.page_info.next_cursor.clone(),
            ..state(user("u-1"), &[METRIC_TOKENS], Some(1))
        })
        .await
        .expect("page 2");

    assert_eq!(first.items.len(), 1, "a full page");
    assert_eq!(second.items.len(), 1, "a full page");
    assert_eq!(second.page_info.next_cursor, None);
    let mut read = vec![
        first.items[0].quota_id.clone(),
        second.items[0].quota_id.clone(),
    ];
    read.sort();
    let mut expected: Vec<_> = current.iter().map(ToString::to_string).collect();
    expected.sort();
    assert_eq!(read, expected);
}

#[tokio::test]
async fn an_unavailable_metric_fails_the_quota_state_read() {
    let f = fixture().await;
    let broken = "gts.cf.core.quota.metric.v1~cf.qetest.metric.broken.v1~";
    f.registry.seed(broken, {
        let mut document = metric_type_document(broken, json!({ "kind": "counter" }), json!({}));
        document["x-gts-abstract"] = json!(true);
        document
    });

    let err = f
        .plugin
        .quota_state(state(user("u-1"), &[METRIC_TOKENS, broken], None))
        .await
        .expect_err("unavailable");

    assert!(
        matches!(err, CanonicalError::FailedPrecondition { .. }),
        "{err:?}"
    );
}

#[tokio::test]
async fn anything_but_one_metric_item_or_amount_is_invalid_and_nothing_runs() {
    let f = fixture().await;
    f.plugin.debit(debit("d-1", 2)).await.expect("debit");
    let token = token_of(
        f.plugin
            .acquire_lease(acquire("a-1", 5))
            .await
            .expect("acquire"),
    );
    let two_items = vec![item(METRIC_TOKENS, 1), item(METRIC_REQUESTS, 1)];
    let two_amounts = vec![amount(METRIC_TOKENS, 1), amount(METRIC_REQUESTS, 1)];

    let errors = [
        f.plugin
            .debit(PluginDebitRequest {
                items: two_items.clone(),
                ..debit("d-2", 1)
            })
            .await
            .expect_err("debit"),
        f.plugin
            .acquire_lease(PluginAcquireLeaseRequest {
                items: two_items,
                ..acquire("a-2", 1)
            })
            .await
            .expect_err("acquire"),
        f.plugin
            .commit_lease(commit(&token, Some(two_amounts.clone()), "c-1"), None)
            .await
            .expect_err("commit"),
        f.plugin
            .commit_lease(commit(&token, Some(Vec::new()), "c-2"), None)
            .await
            .expect_err("commit of nothing"),
        f.plugin
            .return_amount(give_back("d-1", two_amounts, "ret-1"), None)
            .await
            .expect_err("return"),
        f.plugin
            .return_amount(give_back("d-1", Vec::new(), "ret-2"), None)
            .await
            .expect_err("return of nothing"),
    ];

    for err in &errors {
        assert!(is_invalid(err, tokens::ONE_METRIC_ITEM_REQUIRED), "{err:?}");
    }
    assert_eq!(
        f.storage.consumed(f.quota),
        7,
        "the debit and the held lease only"
    );
}

#[tokio::test]
async fn a_commit_or_return_for_another_metric_type_is_a_metric_type_mismatch() {
    let f = fixture().await;
    f.plugin.debit(debit("d-1", 2)).await.expect("debit");
    let token = token_of(
        f.plugin
            .acquire_lease(acquire("a-1", 5))
            .await
            .expect("acquire"),
    );

    let commit_err = f
        .plugin
        .commit_lease(
            commit(&token, Some(vec![amount(METRIC_REQUESTS, 1)]), "c-1"),
            None,
        )
        .await
        .expect_err("commit");
    let return_err = f
        .plugin
        .return_amount(
            give_back("d-1", vec![amount(METRIC_REQUESTS, 1)], "ret-1"),
            None,
        )
        .await
        .expect_err("return");

    assert!(
        is_invalid(&commit_err, "METRIC_TYPE_MISMATCH"),
        "{commit_err:?}"
    );
    assert!(
        is_invalid(&return_err, "METRIC_TYPE_MISMATCH"),
        "{return_err:?}"
    );
    assert_eq!(f.storage.consumed(f.quota), 7, "nothing changed");
    f.plugin
        .commit_lease(
            commit(&token, Some(vec![amount(METRIC_TOKENS, 5)]), "c-2"),
            None,
        )
        .await
        .expect("the lease is still active");
}

#[tokio::test]
async fn denials_reach_the_caller_with_the_closed_reason_and_qe_diagnostics() {
    let f = fixture().await;

    let none = f
        .plugin
        .debit(PluginDebitRequest {
            items: vec![MetricItem {
                metric: MetricObject {
                    metadata: Map::new(),
                    ..item(METRIC_REQUESTS, 1).metric
                },
                amount: 1,
            }],
            ..debit("d-1", 1)
        })
        .await
        .expect("decision");
    let over = f.plugin.debit(debit("d-2", 500)).await.expect("decision");
    let LeaseOutcome::Denied { decision: held } = f
        .plugin
        .acquire_lease(acquire("a-1", 500))
        .await
        .expect("decision")
    else {
        panic!("acquired over the cap");
    };

    assert_eq!(none.reason, Some(DenialReason::NoApplicableQuota));
    for decision in [&over, &held] {
        assert!(!decision.allowed);
        assert_eq!(decision.reason, Some(DenialReason::QuotaExceeded));
        let reason = decision.diagnostics[DIAGNOSTIC_REASON]
            .as_str()
            .expect("QE's reason text");
        assert_ne!(reason, quota_enforcement_sdk::NO_APPLICABLE_QUOTA);
        assert_eq!(
            decision.diagnostics[DIAGNOSTIC_VIOLATED_QUOTA_IDS],
            json!([f.quota.to_string()])
        );
    }
}

#[tokio::test]
async fn a_key_reused_with_another_payload_is_already_exists() {
    let f = fixture().await;
    f.plugin.debit(debit("d-1", 2)).await.expect("debit");

    let err = f
        .plugin
        .debit(debit("d-1", 3))
        .await
        .expect_err("another payload");

    assert!(
        matches!(err, CanonicalError::AlreadyExists { .. }),
        "{err:?}"
    );
}

#[tokio::test]
async fn an_invalid_field_is_named_as_the_caller_sent_it() {
    let f = fixture().await;
    f.plugin.debit(debit("d-1", 2)).await.expect("debit");
    let token = token_of(
        f.plugin
            .acquire_lease(acquire("a-1", 5))
            .await
            .expect("acquire"),
    );
    let cases = [
        (
            f.plugin.debit(debit("d-2", 0)).await.expect_err("debit"),
            "items[0].amount",
        ),
        (
            f.plugin
                .acquire_lease(PluginAcquireLeaseRequest {
                    ttl_secs: 0,
                    ..acquire("a-2", 1)
                })
                .await
                .expect_err("acquire"),
            "ttl_secs",
        ),
        (
            f.plugin
                .commit_lease(
                    commit(&token, Some(vec![amount("not-a-metric", 1)]), "c-1"),
                    None,
                )
                .await
                .expect_err("commit"),
            "amounts[0].metric_type",
        ),
        (
            f.plugin
                .commit_lease(
                    commit(&token, Some(vec![amount(METRIC_TOKENS, -1)]), "c-2"),
                    None,
                )
                .await
                .expect_err("commit"),
            "amounts[0].amount",
        ),
        (
            f.plugin
                .return_amount(
                    give_back("d-1", vec![amount(METRIC_TOKENS, 0)], "r-1"),
                    None,
                )
                .await
                .expect_err("return"),
            "amounts[0].amount",
        ),
        (
            f.plugin
                .return_amount(give_back(" ", vec![amount(METRIC_TOKENS, 1)], "r-2"), None)
                .await
                .expect_err("return"),
            "charge.idempotency_key",
        ),
        // Errors that QE gives a field only when it lifts them.
        (
            f.plugin
                .debit(PluginDebitRequest {
                    items: vec![item(METRIC_OTHER, 1)],
                    ..debit("d-3", 1)
                })
                .await
                .expect_err("an unregistered metric type"),
            "items[0].metric.type",
        ),
        (
            f.plugin
                .debit(PluginDebitRequest {
                    subjects: vec![QuotaSubject {
                        subject_type: UNREGISTERED_SUBJECT_TYPE.to_owned(),
                        id: "x-1".to_owned(),
                    }],
                    // A metric that admits every subject type.
                    items: vec![item(METRIC_REQUESTS, 1)],
                    ..debit("d-4", 1)
                })
                .await
                .expect_err("an unregistered subject type"),
            "subjects[0].type",
        ),
        (
            f.plugin
                .debit(PluginDebitRequest {
                    subjects: vec![QuotaSubject {
                        subject_type: DEVICE_SUBJECT_TYPE.to_owned(),
                        id: "d-1".to_owned(),
                    }],
                    ..debit("d-5", 1)
                })
                .await
                .expect_err("a subject type the metric does not admit"),
            "subjects",
        ),
        (
            f.plugin
                .commit_lease(
                    commit(&token, Some(vec![amount(METRIC_REQUESTS, 1)]), "c-3"),
                    None,
                )
                .await
                .expect_err("another metric type"),
            "amounts[0].metric_type",
        ),
        (
            f.plugin
                .quota_state(state(user("u-1"), &["not-a-metric"], None))
                .await
                .expect_err("quota state"),
            "metrics[0].type",
        ),
        (
            f.plugin
                .quota_state(state(user("u-1"), &[METRIC_TOKENS, METRIC_OTHER], None))
                .await
                .expect_err("an unregistered metric type"),
            "metrics[1].type",
        ),
    ];
    for (err, field) in &cases {
        assert!(
            matches!(err, CanonicalError::InvalidArgument { .. })
                && format!("{err:?}").contains(&format!("\"{field}\"")),
            "{field}: {err:?}"
        );
    }
}
