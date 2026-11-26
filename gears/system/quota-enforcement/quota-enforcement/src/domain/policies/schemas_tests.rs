#![allow(clippy::expect_used)]

use quota_enforcement_sdk::{EnvironmentInputs, MetricId, PolicyScope};
use serde_json::json;

use super::{CatalogPolicySchemas, SnapshotLimits};
use crate::domain::error::DomainError;
use crate::domain::policies::PolicySchemas;
use crate::test_support::{
    METRIC_OTHER, METRIC_TOKENS, metric_type_document, platform_registry, policy_limits,
    test_catalog,
};

fn limits() -> SnapshotLimits {
    policy_limits().snapshot
}

fn scope(metric: &str) -> PolicyScope {
    PolicyScope::Metric {
        metric: MetricId::parse(metric).expect("metric"),
    }
}

fn schemas(limits: SnapshotLimits) -> CatalogPolicySchemas {
    CatalogPolicySchemas::new(test_catalog().0, limits)
}

fn reason_of(err: DomainError) -> &'static str {
    match err {
        DomainError::InvalidPolicy { reason, .. } => reason,
        other => panic!("expected an invalid policy, got {other:?}"),
    }
}

#[tokio::test]
async fn a_metric_policy_reading_request_keeps_only_its_metric_type_schema() {
    let snapshot = schemas(limits())
        .snapshot(&scope(METRIC_TOKENS), "cel", EnvironmentInputs::ALL)
        .await
        .expect("snapshot");
    assert_eq!(snapshot.inputs, EnvironmentInputs::ALL);
    assert_eq!(
        snapshot.schemas.keys().collect::<Vec<_>>(),
        vec![METRIC_TOKENS]
    );
}

#[tokio::test]
async fn a_policy_that_reads_no_request_or_a_global_policy_keeps_no_schema() {
    let no_request = EnvironmentInputs { request: false };
    let schemas = schemas(limits());
    let narrowed = schemas
        .snapshot(&scope(METRIC_TOKENS), "cel", no_request)
        .await
        .expect("snapshot");
    assert_eq!(narrowed.inputs, no_request);
    assert!(narrowed.schemas.is_empty());
    let global = schemas
        .snapshot(&PolicyScope::Global, "cel", EnvironmentInputs::ALL)
        .await
        .expect("global");
    assert!(global.schemas.is_empty());
    let mrw = schemas
        .snapshot(
            &scope(METRIC_TOKENS),
            "most-restrictive-wins",
            EnvironmentInputs::ALL,
        )
        .await
        .expect("mrw");
    assert!(mrw.schemas.is_empty());
    assert_eq!(mrw.inputs, EnvironmentInputs::NONE);
}

#[tokio::test]
async fn a_metric_policy_needs_a_served_catalogue_entry_whatever_its_engine() {
    let schemas = schemas(limits());
    for engine in ["cel", "most-restrictive-wins"] {
        let err = schemas
            .snapshot(&scope(METRIC_OTHER), engine, EnvironmentInputs::ALL)
            .await
            .expect_err("unregistered");
        assert!(
            matches!(err, DomainError::MetricNotRegistered { .. }),
            "{engine}: {err:?}"
        );
    }

    let registry = platform_registry();
    let broken = "gts.cf.core.quota.metric.v1~cf.qetest.metric.broken.v1~";
    let mut abstract_type = metric_type_document(broken, json!({ "kind": "counter" }), json!({}));
    abstract_type["x-gts-abstract"] = json!(true);
    registry.seed(broken, abstract_type);
    let catalog = crate::test_support::catalog_over(
        &registry,
        std::sync::Arc::new(crate::test_support::RecordingMetrics::default()),
    );
    let err = CatalogPolicySchemas::new(catalog, limits())
        .snapshot(
            &scope(broken),
            "most-restrictive-wins",
            EnvironmentInputs::ALL,
        )
        .await
        .expect_err("unavailable");
    assert!(
        matches!(err, DomainError::ProjectionNotResolvable { .. }),
        "{err:?}"
    );
}

#[tokio::test]
async fn each_snapshot_bound_refuses_with_its_own_reason() {
    let generous = limits();
    let cases = [
        (
            SnapshotLimits {
                schemas: 0,
                ..generous
            },
            "POLICY_SCHEMA_TOO_LARGE",
        ),
        (
            SnapshotLimits {
                bytes: 16,
                ..generous
            },
            "POLICY_SCHEMA_TOO_LARGE",
        ),
        (
            SnapshotLimits {
                depth: 1,
                ..generous
            },
            "POLICY_SCHEMA_TOO_DEEP",
        ),
    ];
    for (bounds, expected) in cases {
        let err = schemas(bounds)
            .snapshot(&scope(METRIC_TOKENS), "cel", EnvironmentInputs::ALL)
            .await
            .expect_err(expected);
        assert_eq!(reason_of(err), expected);
    }
}
