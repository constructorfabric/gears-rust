#![allow(clippy::expect_used)]
//! Snapshots keep only the contracts a policy reads, and activation is judged
//! against those alone.

use std::sync::Arc;

use gts::GtsTypeId;
use quota_enforcement_sdk::{
    EnvironmentInputs, MetricId, PolicyId, PolicyScope, PolicyVersion, PolicyVersionState,
};
use serde_json::json;
use time::OffsetDateTime;

use super::{CatalogPolicySchemas, SnapshotLimits};
use crate::domain::catalog::{CatalogBuilder, CatalogConfig, ProjectionContractCatalog};
use crate::domain::error::DomainError;
use crate::domain::policies::PolicySchemas;
use crate::test_support::{
    FakeContractRegistry, LLM_MODEL_RESOURCE, LLM_TENANT_PROJECTION, LLM_TOKEN_CONSTRAINT,
    LLM_TOKEN_REQUEST, LLM_USER_PROJECTION, METRIC_TOKENS, RecordingMetrics, policy_limits,
};

async fn catalog(with_resource: bool) -> Arc<ProjectionContractCatalog> {
    let registry = FakeContractRegistry::llm_gateway();
    let metrics = RecordingMetrics::default();
    let resource_projections = if with_resource {
        vec![GtsTypeId::new(LLM_MODEL_RESOURCE)]
    } else {
        Vec::new()
    };
    Arc::new(
        CatalogBuilder::new(&registry, &metrics)
            .build(&CatalogConfig {
                subject_projections: vec![
                    GtsTypeId::new(LLM_USER_PROJECTION),
                    GtsTypeId::new(LLM_TENANT_PROJECTION),
                ],
                resource_projections,
            })
            .await
            .expect("catalogue"),
    )
}

fn limits() -> SnapshotLimits {
    policy_limits().snapshot
}

fn scope() -> PolicyScope {
    PolicyScope::Metric {
        metric: MetricId::parse(METRIC_TOKENS).expect("metric"),
    }
}

#[tokio::test]
async fn a_snapshot_keeps_only_the_contracts_behind_the_inputs_it_was_built_for() {
    let schemas = CatalogPolicySchemas::new(catalog(true).await, limits());
    let arbitration_only = EnvironmentInputs {
        request: false,
        resource: false,
        arbitration: true,
    };
    let pruned = schemas
        .snapshot(&scope(), "cel", arbitration_only)
        .await
        .expect("snapshot");
    assert_eq!(pruned.inputs, arbitration_only);
    assert!(pruned.schemas.contains_key(LLM_TOKEN_CONSTRAINT));
    assert!(!pruned.schemas.contains_key(LLM_TOKEN_REQUEST));
    assert!(!pruned.schemas.contains_key(LLM_MODEL_RESOURCE));
    let environment = &pruned.environments[0].schema["properties"];
    assert_eq!(
        environment["request"],
        json!({"type": "object", "properties": {}}),
        "an unread input is present but empty"
    );
    assert_eq!(
        environment["resource"],
        json!({"anyOf": [{"type": "null"}]})
    );
    // The constraint metadata rides along in whatever `allOf` shape the
    // resolved contract has; what matters is that `regions` is declared.
    assert!(
        environment["arbitration"]
            .to_string()
            .contains("\"regions\"")
    );
    assert!(!environment["request"].to_string().contains("\"region\""));

    let full = schemas
        .snapshot(&scope(), "cel", EnvironmentInputs::ALL)
        .await
        .expect("snapshot");
    assert!(full.schemas.contains_key(LLM_TOKEN_REQUEST));
    assert!(full.schemas.contains_key(LLM_MODEL_RESOURCE));

    let mrw = schemas
        .snapshot(&scope(), "most-restrictive-wins", EnvironmentInputs::ALL)
        .await
        .expect("snapshot");
    assert_eq!(mrw.inputs, EnvironmentInputs::NONE);
    assert!(mrw.schemas.is_empty() && mrw.environments.is_empty());
}

fn version(snapshot: quota_enforcement_sdk::PolicySchemaSnapshot) -> PolicyVersion {
    PolicyVersion {
        schema_snapshot: snapshot,
        policy_id: PolicyId::new("p".to_owned()),
        version: 1,
        scope: scope(),
        engine_id: "cel".to_owned(),
        engine_config: json!({ "expr": "{}" }),
        timeout_ms: None,
        description: None,
        state: PolicyVersionState::Superseded,
        created_at: OffsetDateTime::UNIX_EPOCH,
        created_by: "operator".to_owned(),
        comment: None,
    }
}

#[tokio::test]
async fn adding_a_resource_projection_strands_only_policies_that_read_resources() {
    // Saved when no resource projection was configured.
    let before = CatalogPolicySchemas::new(catalog(false).await, limits());
    let reads_arbitration = version(
        before
            .snapshot(
                &scope(),
                "cel",
                EnvironmentInputs {
                    request: true,
                    resource: false,
                    arbitration: true,
                },
            )
            .await
            .expect("snapshot"),
    );
    let reads_everything = version(
        before
            .snapshot(&scope(), "cel", EnvironmentInputs::ALL)
            .await
            .expect("snapshot"),
    );

    // Restarted with the model resource configured.
    let after = CatalogPolicySchemas::new(catalog(true).await, limits());
    after
        .check_activation(&reads_arbitration)
        .expect("a policy that never reads `resource` is untouched");
    let err = after
        .check_activation(&reads_everything)
        .expect_err("a policy type-checked against the old resource set is stale");
    assert!(
        matches!(
            err,
            DomainError::InvalidPolicy {
                reason: "POLICY_CATALOG_INCOMPATIBLE",
                ..
            }
        ),
        "{err:?}"
    );
}

fn reason_of(err: DomainError) -> &'static str {
    match err {
        DomainError::InvalidPolicy { reason, .. } => reason,
        other => panic!("expected an invalid policy, got {other:?}"),
    }
}

#[tokio::test]
async fn a_cel_snapshot_outside_or_without_the_catalogue_is_refused() {
    let full = CatalogPolicySchemas::new(catalog(false).await, limits());
    let foreign = PolicyScope::Metric {
        metric: MetricId::parse(crate::test_support::METRIC_OTHER).expect("metric"),
    };
    let err = full
        .snapshot(&foreign, "cel", EnvironmentInputs::ALL)
        .await
        .expect_err("a metric no projection admits");
    assert_eq!(reason_of(err), DomainError::PROJECTION_NOT_RESOLVABLE);

    let registry = FakeContractRegistry::llm_gateway();
    let metrics = RecordingMetrics::default();
    let empty = Arc::new(
        CatalogBuilder::new(&registry, &metrics)
            .build(&CatalogConfig {
                subject_projections: Vec::new(),
                resource_projections: Vec::new(),
            })
            .await
            .expect("an empty catalogue builds"),
    );
    let err = CatalogPolicySchemas::new(empty, limits())
        .snapshot(&PolicyScope::Global, "cel", EnvironmentInputs::ALL)
        .await
        .expect_err("nothing to type-check against");
    assert_eq!(reason_of(err), "POLICY_SCHEMA_EMPTY");
}

#[tokio::test]
async fn each_snapshot_bound_refuses_with_its_own_reason() {
    let catalogue = catalog(true).await;
    let bounded = |bytes, schemas, depth| {
        CatalogPolicySchemas::new(
            Arc::clone(&catalogue),
            SnapshotLimits {
                bytes,
                schemas,
                depth,
            },
        )
    };
    let generous = limits();
    let cases = [
        (
            bounded(generous.bytes, 0, generous.depth),
            "POLICY_SCHEMA_TOO_LARGE",
        ),
        (
            bounded(16, generous.schemas, generous.depth),
            "POLICY_SCHEMA_TOO_LARGE",
        ),
        (
            bounded(generous.bytes, generous.schemas, 1),
            "POLICY_SCHEMA_TOO_DEEP",
        ),
    ];
    for (schemas, expected) in cases {
        let err = schemas
            .snapshot(&scope(), "cel", EnvironmentInputs::ALL)
            .await
            .expect_err(expected);
        assert_eq!(reason_of(err), expected);
    }
}
