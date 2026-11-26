#![allow(clippy::expect_used)]

use std::sync::Arc;

use quota_enforcement_sdk::{
    EnforcementMode, PageResult, PeriodType, QuotaDraft, QuotaEnforcementStoragePluginV1, QuotaId,
    QuotaSnapshot, QuotaSource, QuotaType, SnapshotRequest, SnapshotSubject, SubjectRef, TenantId,
    ValidityWindow,
};
use serde_json::json;
use time::macros::datetime;
use toolkit_security::AccessScope;
use uuid::Uuid;

use super::super::harness_tests::{Harness, type_id};
use crate::domain::catalog::MetricMode;
use crate::domain::error::DomainError;
use crate::domain::ports::metrics::DenialReason;
use crate::domain::tokens;
use crate::test_support::{
    DEVICE_SUBJECT_TYPE, METRIC_OTHER, METRIC_TOKENS, PermitFiltersPdp, PermitTenantsPdp,
    TENANT_SUBJECT_TYPE, USER_SUBJECT_TYPE, UncallablePdp, actor, fail_registry_reads,
    metric_type_document, tenant,
};

fn user(id: &str) -> SubjectSnapshot {
    SubjectSnapshot::new(USER_SUBJECT_TYPE, id)
}

fn tenant_target(id: &str) -> SubjectSnapshot {
    SubjectSnapshot::new(TENANT_SUBJECT_TYPE, id)
}

struct SubjectSnapshot(SnapshotSubject);

impl SubjectSnapshot {
    fn new(subject_type: &str, id: &str) -> Self {
        Self(SnapshotSubject {
            subject_type: subject_type.to_owned(),
            id: id.to_owned(),
            metric: METRIC_TOKENS.to_owned(),
            metadata: None,
        })
    }

    fn metadata(mut self, metadata: &serde_json::Value) -> Self {
        self.0.metadata = metadata.as_object().cloned();
        self
    }

    fn metric(mut self, metric: &str) -> Self {
        metric.clone_into(&mut self.0.metric);
        self
    }
}

fn request(subjects: Vec<SubjectSnapshot>) -> SnapshotRequest {
    SnapshotRequest {
        tenant_id: tenant(),
        subjects: subjects.into_iter().map(|subject| subject.0).collect(),
        limit: None,
        cursor: None,
    }
}

fn draft(subject: SubjectRef) -> QuotaDraft {
    QuotaDraft {
        tenant_id: tenant(),
        subject,
        metric: quota_enforcement_sdk::MetricId::parse(METRIC_TOKENS).expect("metric"),
        quota_type: QuotaType::Allocation,
        period: None,
        enforcement_mode: EnforcementMode::Hard,
        cap: Some(100),
        notification_thresholds: Vec::new(),
        validity_window: None,
        fail_open_hint: false,
        metadata: serde_json::Map::new(),
        source: QuotaSource::Operator,
    }
}

fn user_ref(id: &str) -> SubjectRef {
    SubjectRef {
        subject_type: type_id(USER_SUBJECT_TYPE),
        subject_id: id.to_owned(),
    }
}

fn tenant_ref() -> SubjectRef {
    SubjectRef {
        subject_type: type_id(TENANT_SUBJECT_TYPE),
        subject_id: tenant().to_string(),
    }
}

async fn create(h: &Harness, draft: QuotaDraft) -> QuotaId {
    h.storage
        .create_quota(&actor(), &AccessScope::allow_all(), draft, &[])
        .await
        .expect("quota")
}

fn ids(page: &PageResult<QuotaSnapshot>) -> Vec<QuotaId> {
    page.items
        .iter()
        .map(|snapshot| snapshot.quota_id)
        .collect()
}

type Refusal = (SnapshotRequest, Option<usize>, &'static str, &'static str);

fn refused(error: &DomainError, index: Option<usize>, field: &str, reason: &str) -> bool {
    matches!(
        error,
        DomainError::InvalidSnapshot { index: i, field: f, reason: r }
            if *i == index && *f == field && *r == reason
    )
}

// --- target semantics ------------------------------------------------------------

#[tokio::test]
async fn a_user_target_reads_the_users_and_the_tenants_quotas_once_each() {
    let h = Harness::new().await;
    let mine = create(&h, draft(user_ref("u-1"))).await;
    let theirs = create(&h, draft(user_ref("u-2"))).await;
    let shared = create(&h, draft(tenant_ref())).await;

    let alone = h
        .operations()
        .snapshot(request(vec![user("u-1")]))
        .await
        .expect("snapshot");
    // The repeated tenant target overlaps the user target's tenant tier.
    let page = h
        .operations()
        .snapshot(request(vec![
            user("u-1"),
            tenant_target(&tenant().to_string()),
        ]))
        .await
        .expect("snapshot");

    let mut expected = vec![mine, shared];
    expected.sort();
    assert_eq!(ids(&alone), expected, "a user target reads both tiers");
    assert_eq!(ids(&page), expected, "each once, in quota_id order");
    assert!(!ids(&page).contains(&theirs));
}

#[tokio::test]
async fn a_target_with_metadata_reads_only_the_quotas_that_apply_to_it() {
    let h = Harness::new().await;
    let conditioned = |region: &str| {
        let mut draft = draft(user_ref("u-1"));
        draft.metadata.insert("region".into(), region.into());
        draft
    };
    let any = create(&h, draft(user_ref("u-1"))).await;
    let eu = create(&h, conditioned("eu")).await;
    let us = create(&h, conditioned("us")).await;
    let read = |target: SubjectSnapshot| {
        let h = &h;
        async move {
            let page = h.operations().snapshot(request(vec![target])).await;
            ids(&page.expect("snapshot"))
        }
    };

    let mut all = vec![any, eu, us];
    all.sort();
    assert_eq!(read(user("u-1")).await, all, "no metadata: every Quota");
    let mut in_eu = vec![any, eu];
    in_eu.sort();
    assert_eq!(
        read(user("u-1").metadata(&json!({ "region": "eu" }))).await,
        in_eu
    );
    assert_eq!(
        read(user("u-1").metadata(&json!({}))).await,
        [any],
        "a request without the property meets no condition on it"
    );

    let error = h
        .operations()
        .snapshot(request(vec![user("u-1").metadata(&json!({ "region": 7 }))]))
        .await
        .expect_err("metadata that does not fit the metric type");
    assert!(
        refused(&error, Some(0), "metadata", tokens::CONTRACT_VIOLATION),
        "{error:?}"
    );
}

#[tokio::test]
async fn a_tenant_target_reads_the_tenants_own_quotas_only() {
    let h = Harness::new().await;
    create(&h, draft(user_ref("u-1"))).await;
    let shared = create(&h, draft(tenant_ref())).await;

    let page = h
        .operations()
        .snapshot(request(vec![tenant_target(&tenant().to_string())]))
        .await
        .expect("snapshot");

    assert_eq!(ids(&page), vec![shared]);
}

#[tokio::test]
async fn a_tenant_target_naming_another_tenant_is_refused_before_the_pdp() {
    let pdp = Arc::new(PermitTenantsPdp::new(vec![tenant().as_uuid()]));
    let h = Harness::with_pdp(pdp.clone()).await;

    let error = h
        .operations()
        .snapshot(request(vec![
            user("u-1"),
            tenant_target(&Uuid::from_u128(0xbad).to_string()),
        ]))
        .await
        .expect_err("mismatch");

    assert!(
        refused(&error, Some(1), "id", tokens::SNAPSHOT_TENANT_MISMATCH),
        "{error:?}"
    );
    assert_eq!(pdp.calls(), 0);
}

#[tokio::test]
async fn out_of_window_quotas_are_read_and_deactivated_ones_are_not() {
    let h = Harness::new().await;
    let lapsed = create(
        &h,
        QuotaDraft {
            validity_window: Some(ValidityWindow {
                start: None,
                end: Some(datetime!(2020-01-01 00:00:00 UTC)),
            }),
            ..draft(user_ref("u-1"))
        },
    )
    .await;
    let gone = create(&h, draft(user_ref("u-1"))).await;
    h.storage
        .deactivate_quota(&actor(), &AccessScope::allow_all(), gone, &[])
        .await
        .expect("deactivate");

    let page = h
        .operations()
        .snapshot(request(vec![user("u-1")]))
        .await
        .expect("snapshot");

    assert_eq!(ids(&page), vec![lapsed]);
    assert!(!page.items[0].currently_within_window);
}

#[tokio::test]
async fn a_directly_recorded_metric_is_still_readable() {
    let pdp = Arc::new(PermitTenantsPdp::new(vec![tenant().as_uuid()]));
    let h = Harness::over(pdp, MetricMode::Direct).await;
    let id = create(&h, draft(user_ref("u-1"))).await;

    let page = h
        .operations()
        .snapshot(request(vec![user("u-1")]))
        .await
        .expect("a snapshot is not a write");

    assert_eq!(ids(&page), vec![id]);
}

#[tokio::test]
async fn a_target_matching_nothing_is_an_empty_page() {
    let h = Harness::new().await;
    let page = h
        .operations()
        .snapshot(request(vec![user("nobody")]))
        .await
        .expect("snapshot");
    assert!(page.items.is_empty());
    assert_eq!(page.next_cursor, None);
}

// --- the request's bounds and shape -------------------------------------------------

#[tokio::test]
async fn the_request_is_refused_in_order_before_any_pdp_call() {
    let pdp = Arc::new(PermitTenantsPdp::new(vec![tenant().as_uuid()]));
    let h = Harness::with_pdp(pdp.clone()).await;
    let mut ops = h.operations();
    ops.snapshot.max_filters = 2;

    let cases: Vec<Refusal> = vec![
        (
            request(Vec::new()),
            None,
            "subjects",
            tokens::SNAPSHOT_SUBJECTS_REQUIRED,
        ),
        (
            request(vec![user("a"), user("b"), user("c")]),
            None,
            "subjects",
            tokens::SNAPSHOT_TOO_MANY_SUBJECTS,
        ),
        (
            SnapshotRequest {
                limit: Some(0),
                ..request(vec![user("a")])
            },
            None,
            "limit",
            tokens::SNAPSHOT_LIMIT_OUT_OF_RANGE,
        ),
        (
            SnapshotRequest {
                tenant_id: TenantId::new(Uuid::nil()),
                ..request(vec![user("a")])
            },
            None,
            "tenant_id",
            tokens::TENANT_ID_REQUIRED,
        ),
        (
            request(vec![user("a"), user("b").metric("not a metric")]),
            Some(1),
            "metric",
            tokens::METRIC_INVALID,
        ),
        (
            request(vec![user(" ")]),
            Some(0),
            "id",
            tokens::SUBJECT_ID_REQUIRED,
        ),
        (
            request(vec![user(&"a".repeat(257))]),
            Some(0),
            "id",
            tokens::SUBJECT_ID_TOO_LONG,
        ),
        (
            request(vec![SubjectSnapshot::new("no such type!", "a")]),
            Some(0),
            "type",
            tokens::SUBJECT_TYPE_INVALID,
        ),
        (
            request(vec![user("a").metric(USER_SUBJECT_TYPE)]),
            Some(0),
            "metric",
            tokens::METRIC_INVALID,
        ),
    ];
    let count = cases.len();
    for (request, index, field, reason) in cases {
        let error = ops.snapshot(request).await.expect_err("refused");
        assert!(
            refused(&error, index, field, reason),
            "{field}/{reason}: {error:?}"
        );
    }

    assert_eq!(pdp.calls(), 0, "every refusal happens before the PDP");
    assert_eq!(
        h.metrics.denials(),
        vec![DenialReason::InvalidArgument; count],
        "each counts as one invalid-argument denial"
    );
}

#[tokio::test]
async fn the_limit_defaults_to_the_page_size_and_never_exceeds_it() {
    let h = Harness::new().await;
    for subject in ["u-1", "u-1", "u-1"] {
        create(&h, draft(user_ref(subject))).await;
    }
    let mut ops = h.operations();
    ops.snapshot.page_size = 2;

    let defaulted = ops
        .snapshot(request(vec![user("u-1")]))
        .await
        .expect("default limit");
    assert_eq!(defaulted.items.len(), 2);
    let cursor = defaulted.next_cursor.clone().expect("a next page");
    let rest = ops
        .snapshot(SnapshotRequest {
            cursor: Some(cursor),
            ..request(vec![user("u-1")])
        })
        .await
        .expect("next page");
    assert_eq!(rest.items.len(), 1);
    assert_eq!(rest.next_cursor, None);

    let at_most = ops
        .snapshot(SnapshotRequest {
            limit: Some(2),
            ..request(vec![user("u-1")])
        })
        .await
        .expect("limit = page_size");
    assert_eq!(at_most.items.len(), 2);
    let over = ops
        .snapshot(SnapshotRequest {
            limit: Some(3),
            ..request(vec![user("u-1")])
        })
        .await
        .expect_err("limit above page_size");
    assert!(refused(
        &over,
        None,
        "limit",
        tokens::SNAPSHOT_LIMIT_OUT_OF_RANGE
    ));
}

#[tokio::test]
async fn a_subject_type_the_metric_does_not_admit_is_named_after_the_pdp() {
    let pdp = Arc::new(PermitTenantsPdp::new(vec![tenant().as_uuid()]));
    let h = Harness::with_pdp(pdp.clone()).await;

    let error = h
        .operations()
        .snapshot(request(vec![
            user("u-1"),
            SubjectSnapshot::new(DEVICE_SUBJECT_TYPE, "d-1"),
        ]))
        .await
        .expect_err("the device type is not admitted");

    assert!(
        refused(&error, Some(1), "type", tokens::METRIC_NOT_ADMITTED),
        "{error:?}"
    );
    assert!(matches!(
        toolkit_canonical_errors::CanonicalError::from(error),
        toolkit_canonical_errors::CanonicalError::InvalidArgument { .. }
    ));
    assert_eq!(pdp.calls(), 1, "the PDP authorized the target first");
}

#[tokio::test]
async fn in_trusted_mode_a_subject_type_the_metric_does_not_admit_reads_the_tenants_quotas() {
    let h = Harness::with_pdp(Arc::new(UncallablePdp)).await;
    let shared = create(&h, draft(tenant_ref())).await;
    create(&h, draft(user_ref("u-1"))).await;

    let page = h
        .trusted(None)
        .snapshot(request(vec![SubjectSnapshot::new(
            DEVICE_SUBJECT_TYPE,
            "d-1",
        )]))
        .await
        .expect("narrowed, not refused");

    assert_eq!(ids(&page), vec![shared]);
}

#[tokio::test]
async fn one_unavailable_metric_fails_the_whole_read() {
    let h = Harness::new().await;
    let broken = "gts.cf.core.quota.metric.v1~cf.qetest.metric.broken.v1~";
    h.registry.seed(broken, {
        let mut document = metric_type_document(broken, json!({ "kind": "counter" }), json!({}));
        document["x-gts-abstract"] = json!(true);
        document
    });
    create(&h, draft(user_ref("u-1"))).await;

    let error = h
        .operations()
        .snapshot(request(vec![user("u-1"), user("u-1").metric(broken)]))
        .await
        .expect_err("one target names an unavailable metric");

    assert!(
        matches!(error, DomainError::ProjectionNotResolvable { .. }),
        "{error:?}"
    );
    assert!(matches!(
        toolkit_canonical_errors::CanonicalError::from(error),
        toolkit_canonical_errors::CanonicalError::FailedPrecondition { .. }
    ));
}

#[tokio::test]
async fn an_unavailable_metric_wins_over_a_subject_type_not_admitted_in_either_order() {
    let h = Harness::new().await;
    let broken = "gts.cf.core.quota.metric.v1~cf.qetest.metric.broken.v1~";
    h.registry.seed(broken, {
        let mut document = metric_type_document(broken, json!({ "kind": "counter" }), json!({}));
        document["x-gts-abstract"] = json!(true);
        document
    });
    let not_admitted = || SubjectSnapshot::new(DEVICE_SUBJECT_TYPE, "d-1");
    let unavailable = || user("u-1").metric(broken);

    for subjects in [
        vec![not_admitted(), unavailable()],
        vec![unavailable(), not_admitted()],
    ] {
        let error = h
            .operations()
            .snapshot(request(subjects))
            .await
            .expect_err("two faulty targets");
        assert!(
            matches!(error, DomainError::ProjectionNotResolvable { .. }),
            "{error:?}"
        );
    }
}

#[tokio::test]
async fn an_unregistered_metric_is_invalid_and_a_silent_registry_is_unavailable() {
    let h = Harness::new().await;
    let unregistered = h
        .operations()
        .snapshot(request(vec![user("u-1").metric(METRIC_OTHER)]))
        .await
        .expect_err("unregistered");
    assert!(
        matches!(unregistered, DomainError::MetricNotRegistered { .. }),
        "{unregistered:?}"
    );

    h.operations()
        .snapshot(request(vec![user("u-1")]))
        .await
        .expect("the metric is now held");
    fail_registry_reads(&h.registry);
    h.operations()
        .snapshot(request(vec![tenant_target(&tenant().to_string())]))
        .await
        .expect("a held metric needs no registry read");
    let down = h
        .operations()
        .snapshot(request(vec![
            tenant_target(&tenant().to_string()).metric(crate::test_support::METRIC_REQUESTS),
        ]))
        .await
        .expect_err("a metric not held while the registry is down");
    assert!(
        matches!(down, DomainError::TypesRegistryUnavailable(_)),
        "{down:?}"
    );
}

// --- authorization -------------------------------------------------------------------

#[tokio::test]
async fn one_pdp_call_carries_every_target() {
    let pdp = Arc::new(PermitTenantsPdp::new(vec![tenant().as_uuid()]));
    let h = Harness::with_pdp(pdp.clone()).await;

    h.operations()
        .snapshot(request(vec![
            user("u-1"),
            tenant_target(&tenant().to_string()),
        ]))
        .await
        .expect("snapshot");

    assert_eq!(pdp.calls(), 1);
    let resource = pdp.last_resource().expect("a PDP request");
    let filters = resource
        .properties
        .get(crate::domain::pep::properties::FILTERS)
        .and_then(serde_json::Value::as_array)
        .expect("the filters property");
    assert_eq!(
        filters,
        &vec![
            json!({ "type": USER_SUBJECT_TYPE, "id": "u-1", "metric": METRIC_TOKENS }),
            json!({
                "type": TENANT_SUBJECT_TYPE,
                "id": tenant().to_string(),
                "metric": METRIC_TOKENS
            }),
        ]
    );
}

#[tokio::test]
async fn one_target_outside_the_grant_denies_the_request_before_storage() {
    let pdp = Arc::new(PermitFiltersPdp::new(
        &[(USER_SUBJECT_TYPE, "u-1", METRIC_TOKENS)],
        vec![tenant().as_uuid()],
    ));
    let h = Harness::with_pdp(pdp.clone()).await;
    let id = create(
        &h,
        QuotaDraft {
            quota_type: QuotaType::Consumption,
            period: Some(PeriodType::Day),
            ..draft(user_ref("u-1"))
        },
    )
    .await;

    let error = h
        .operations()
        .snapshot(request(vec![user("u-1"), user("u-2")]))
        .await
        .expect_err("u-2 is another user");
    assert!(matches!(error, DomainError::PdpDenied { .. }), "{error:?}");

    let cross_tenant = h
        .operations()
        .snapshot(SnapshotRequest {
            tenant_id: TenantId::new(Uuid::from_u128(0xbad)),
            ..request(vec![user("u-1")])
        })
        .await
        .expect_err("another tenant");
    assert!(matches!(cross_tenant, DomainError::PdpDenied { .. }));

    let allowed = h
        .operations()
        .snapshot(request(vec![user("u-1")]))
        .await
        .expect("the granted target alone");
    assert_eq!(ids(&allowed), vec![id]);
}

#[tokio::test]
async fn with_the_registry_down_a_subject_type_not_admitted_is_refused_or_narrowed() {
    let h = Harness::with_pdp(Arc::new(PermitTenantsPdp::new(vec![tenant().as_uuid()]))).await;
    let shared = create(&h, draft(tenant_ref())).await;
    h.operations()
        .snapshot(request(vec![tenant_target(&tenant().to_string())]))
        .await
        .expect("holds the metric");
    fail_registry_reads(&h.registry);
    let device = || request(vec![SubjectSnapshot::new(DEVICE_SUBJECT_TYPE, "d-1")]);

    let error = h
        .operations()
        .snapshot(device())
        .await
        .expect_err("the device type is not admitted");
    assert!(
        refused(&error, Some(0), "type", tokens::METRIC_NOT_ADMITTED),
        "{error:?}"
    );
    let page = h
        .trusted(None)
        .snapshot(device())
        .await
        .expect("narrowed without a subject read");
    assert_eq!(ids(&page), vec![shared]);
}

#[tokio::test]
async fn with_the_registry_down_an_unavailable_metric_is_a_failed_precondition() {
    let h = Harness::new().await;
    let broken = "gts.cf.core.quota.metric.v1~cf.qetest.metric.broken.v1~";
    h.registry.seed(broken, {
        let mut document = metric_type_document(broken, json!({ "kind": "counter" }), json!({}));
        document["x-gts-abstract"] = json!(true);
        document
    });
    h.catalog
        .served(
            &quota_enforcement_sdk::MetricId::parse(broken).expect("metric"),
            crate::domain::ports::metrics::ValidationSurface::RequestMetric,
        )
        .await
        .expect_err("holds the unavailable entry");
    fail_registry_reads(&h.registry);

    let error = h
        .operations()
        .snapshot(request(vec![
            SubjectSnapshot::new(DEVICE_SUBJECT_TYPE, "d-1").metric(broken),
        ]))
        .await
        .expect_err("unavailable");

    assert!(
        matches!(error, DomainError::ProjectionNotResolvable { .. }),
        "{error:?}"
    );
}

#[tokio::test]
async fn held_metrics_read_the_subject_types_not_cached_with_one_read() {
    let h = Harness::new().await;
    h.operations()
        .snapshot(request(vec![
            tenant_target(&tenant().to_string()),
            tenant_target(&tenant().to_string()).metric(crate::test_support::METRIC_REQUESTS),
        ]))
        .await
        .expect("holds both metrics");
    let reads = crate::test_support::registry_reads(&h.registry);

    h.operations()
        .snapshot(request(vec![
            user("u-1"),
            SubjectSnapshot::new(DEVICE_SUBJECT_TYPE, "d-1")
                .metric(crate::test_support::METRIC_REQUESTS),
        ]))
        .await
        .expect("snapshot");

    assert_eq!(crate::test_support::registry_reads(&h.registry), reads + 1);
}
