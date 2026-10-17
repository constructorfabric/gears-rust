#![allow(clippy::expect_used)]
//! The snapshot read over the in-memory double: the request's bounds and
//! shape before the PDP, one PDP call carrying every target, the target
//! semantics (a user target reads the user's and the tenant's Quotas, a
//! tenant target the tenant's only), and what the page holds.

use std::sync::Arc;

use quota_enforcement_sdk::{
    EnforcementMode, PageResult, PeriodType, QuotaDraft, QuotaEnforcementStoragePluginV1, QuotaId,
    QuotaSnapshot, QuotaSource, QuotaType, SCOPE_TENANT, SCOPE_USER, SnapshotRequest,
    SnapshotSubject, SubjectRef, TenantId, ValidityWindow,
};
use time::macros::datetime;
use toolkit_security::AccessScope;
use uuid::Uuid;

use super::super::harness_tests::{Harness, type_id};
use crate::domain::error::DomainError;
use crate::domain::ports::metric_registry::MetricMode;
use crate::domain::ports::metrics::DenialReason;
use crate::domain::tokens;
use crate::test_support::{
    LLM_TENANT_PROJECTION, LLM_TOKEN_CONSTRAINT, LLM_USER_PROJECTION, METRIC_TOKENS,
    PermitFiltersPdp, PermitTenantsPdp, ctx, tenant,
};

/// A scope kind the catalogue knows but admits for no metric.
const GROUP_SCOPE: &str = "gts.cf.core.qe.scope.v1~cf.core.qe.group.v1";

fn user(id: &str) -> SubjectSnapshot {
    SubjectSnapshot::new(SCOPE_USER, id)
}

fn tenant_target(id: &str) -> SubjectSnapshot {
    SubjectSnapshot::new(SCOPE_TENANT, id)
}

/// A request subject, built fluently in the tests.
struct SubjectSnapshot(SnapshotSubject);

impl SubjectSnapshot {
    fn new(kind: &str, id: &str) -> Self {
        Self(SnapshotSubject {
            kind: kind.to_owned(),
            id: id.to_owned(),
            metric: METRIC_TOKENS.to_owned(),
        })
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
        constraint_contract: quota_enforcement_sdk::ContractRef {
            type_id: type_id(LLM_TOKEN_CONSTRAINT),
            version: 1,
        },
    }
}

fn user_ref(id: &str) -> SubjectRef {
    SubjectRef {
        projection_type: type_id(LLM_USER_PROJECTION),
        subject_id: id.to_owned(),
    }
}

fn tenant_ref() -> SubjectRef {
    SubjectRef {
        projection_type: type_id(LLM_TENANT_PROJECTION),
        subject_id: tenant().to_string(),
    }
}

async fn create(h: &Harness, draft: QuotaDraft) -> QuotaId {
    h.storage
        .create_quota(&ctx(), &AccessScope::allow_all(), draft, &[])
        .await
        .expect("quota")
}

fn ids(page: &PageResult<QuotaSnapshot>) -> Vec<QuotaId> {
    page.items
        .iter()
        .map(|snapshot| snapshot.quota_id)
        .collect()
}

/// A request and the refusal it must meet: subject index, field, reason.
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
        .snapshot(&ctx(), request(vec![user("u-1")]))
        .await
        .expect("snapshot");
    // The repeated tenant target overlaps the user target's tenant tier.
    let page = h
        .operations()
        .snapshot(
            &ctx(),
            request(vec![user("u-1"), tenant_target(&tenant().to_string())]),
        )
        .await
        .expect("snapshot");

    let mut expected = vec![mine, shared];
    expected.sort();
    assert_eq!(ids(&alone), expected, "a user target reads both tiers");
    assert_eq!(ids(&page), expected, "each once, in quota_id order");
    assert!(!ids(&page).contains(&theirs));
}

#[tokio::test]
async fn a_tenant_target_reads_the_tenants_own_quotas_only() {
    let h = Harness::new().await;
    create(&h, draft(user_ref("u-1"))).await;
    let shared = create(&h, draft(tenant_ref())).await;

    let page = h
        .operations()
        .snapshot(&ctx(), request(vec![tenant_target(&tenant().to_string())]))
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
        .snapshot(
            &ctx(),
            request(vec![
                user("u-1"),
                tenant_target(&Uuid::from_u128(0xbad).to_string()),
            ]),
        )
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
        .deactivate_quota(&ctx(), &AccessScope::allow_all(), gone, &[])
        .await
        .expect("deactivate");

    let page = h
        .operations()
        .snapshot(&ctx(), request(vec![user("u-1")]))
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
        .snapshot(&ctx(), request(vec![user("u-1")]))
        .await
        .expect("a snapshot is not a write");

    assert_eq!(ids(&page), vec![id]);
}

#[tokio::test]
async fn a_target_matching_nothing_is_an_empty_page() {
    let h = Harness::new().await;
    let page = h
        .operations()
        .snapshot(&ctx(), request(vec![user("nobody")]))
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
            request(vec![SubjectSnapshot::new("no such kind!", "a")]),
            Some(0),
            "kind",
            tokens::SUBJECT_KIND_INVALID,
        ),
    ];
    let count = cases.len();
    for (request, index, field, reason) in cases {
        let error = ops.snapshot(&ctx(), request).await.expect_err("refused");
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
        .snapshot(&ctx(), request(vec![user("u-1")]))
        .await
        .expect("default limit");
    assert_eq!(defaulted.items.len(), 2);
    let cursor = defaulted.next_cursor.clone().expect("a next page");
    let rest = ops
        .snapshot(
            &ctx(),
            SnapshotRequest {
                cursor: Some(cursor),
                ..request(vec![user("u-1")])
            },
        )
        .await
        .expect("next page");
    assert_eq!(rest.items.len(), 1);
    assert_eq!(rest.next_cursor, None);

    let at_most = ops
        .snapshot(
            &ctx(),
            SnapshotRequest {
                limit: Some(2),
                ..request(vec![user("u-1")])
            },
        )
        .await
        .expect("limit = page_size");
    assert_eq!(at_most.items.len(), 2);
    let over = ops
        .snapshot(
            &ctx(),
            SnapshotRequest {
                limit: Some(3),
                ..request(vec![user("u-1")])
            },
        )
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
async fn an_unadmitted_kind_is_named_after_the_pdp() {
    let pdp = Arc::new(PermitTenantsPdp::new(vec![tenant().as_uuid()]));
    let h = Harness::with_pdp(pdp.clone()).await;

    let error = h
        .operations()
        .snapshot(
            &ctx(),
            request(vec![user("u-1"), SubjectSnapshot::new(GROUP_SCOPE, "g-1")]),
        )
        .await
        .expect_err("group is not admitted");

    assert!(
        refused(&error, Some(1), "kind", tokens::SUBJECT_KIND_NOT_ADMITTED),
        "{error:?}"
    );
    assert_eq!(pdp.calls(), 1, "the PDP authorized the target first");
}

// --- authorization -------------------------------------------------------------------

#[tokio::test]
async fn one_pdp_call_carries_every_target() {
    let pdp = Arc::new(PermitTenantsPdp::new(vec![tenant().as_uuid()]));
    let h = Harness::with_pdp(pdp.clone()).await;

    h.operations()
        .snapshot(
            &ctx(),
            request(vec![user("u-1"), tenant_target(&tenant().to_string())]),
        )
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
            serde_json::json!({ "kind": SCOPE_USER, "id": "u-1", "metric": METRIC_TOKENS }),
            serde_json::json!({
                "kind": SCOPE_TENANT,
                "id": tenant().to_string(),
                "metric": METRIC_TOKENS
            }),
        ]
    );
}

#[tokio::test]
async fn one_target_outside_the_grant_denies_the_request_before_storage() {
    let pdp = Arc::new(PermitFiltersPdp::new(
        &[(SCOPE_USER, "u-1", METRIC_TOKENS)],
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
        .snapshot(&ctx(), request(vec![user("u-1"), user("u-2")]))
        .await
        .expect_err("u-2 is another user");
    assert!(matches!(error, DomainError::PdpDenied { .. }), "{error:?}");
    assert!(
        h.storage.period_rows(id).is_empty(),
        "nothing reached storage: the read would have opened the period"
    );

    let cross_tenant = h
        .operations()
        .snapshot(
            &ctx(),
            SnapshotRequest {
                tenant_id: TenantId::new(Uuid::from_u128(0xbad)),
                ..request(vec![user("u-1")])
            },
        )
        .await
        .expect_err("another tenant");
    assert!(matches!(cross_tenant, DomainError::PdpDenied { .. }));

    let allowed = h
        .operations()
        .snapshot(&ctx(), request(vec![user("u-1")]))
        .await
        .expect("the granted target alone");
    assert_eq!(ids(&allowed), vec![id]);
    assert_eq!(
        h.storage.period_rows(id).len(),
        1,
        "a permitted read opens it"
    );
}
