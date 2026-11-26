#![allow(clippy::expect_used)]

use std::sync::Arc;

use quota_enforcement_sdk::MetricId;
use serde_json::{Map, Value, json};

use super::{check, violations};
use crate::domain::catalog::ServedMetric;
use crate::domain::error::DomainError;
use crate::domain::ports::metrics::{ValidationReason, ValidationSurface};
use crate::domain::tokens;
use crate::test_support::{METRIC_TOKENS, RecordingMetrics, test_catalog};

fn object(value: &Value) -> Map<String, Value> {
    value.as_object().cloned().expect("object")
}

/// The fixture metric: `metadata` declares `region` and `model`, both strings.
async fn tokens() -> Arc<ServedMetric> {
    test_catalog()
        .0
        .served(
            &MetricId::parse(METRIC_TOKENS).expect("metric"),
            ValidationSurface::QuotaWrite,
        )
        .await
        .expect("served")
}

fn paths(schema: &Value, metadata: &Value) -> Vec<String> {
    violations(schema, &object(metadata))
        .into_iter()
        .map(|v| v.path)
        .collect()
}

#[tokio::test]
async fn conditions_on_declared_properties_of_the_registered_type_pass() {
    let metrics = RecordingMetrics::default();
    let served = tokens().await;
    for metadata in [
        json!({}),
        json!({ "region": "eu" }),
        json!({ "region": ["eu", "us"], "model": "m-1" }),
    ] {
        check(&metrics, &served, &object(&metadata), 4096).expect("fits");
    }
    assert!(metrics.contract_failures().is_empty());
}

#[tokio::test]
async fn each_condition_that_does_not_fit_is_one_violation_and_nothing_else_is_checked() {
    let metrics = RecordingMetrics::default();
    let served = tokens().await;
    let err = check(
        &metrics,
        &served,
        &object(&json!({ "regions": ["eu"], "region": 7, "model": "m-1" })),
        4096,
    )
    .expect_err("does not fit");
    let DomainError::QuotaMetadataRejected { violations } = err else {
        panic!("rejection expected: {err:?}");
    };
    let paths: Vec<&str> = violations.iter().map(|v| v.path.as_str()).collect();
    assert_eq!(paths, ["/region", "/regions"]);
    assert_eq!(
        metrics.contract_failures(),
        vec![(
            ValidationSurface::QuotaWrite,
            ValidationReason::SchemaViolation
        )]
    );
}

#[tokio::test]
async fn the_size_limit_is_checked_first() {
    let metrics = RecordingMetrics::default();
    let served = tokens().await;
    let err = check(
        &metrics,
        &served,
        &object(&json!({ "blob": "x".repeat(64) })),
        16,
    )
    .expect_err("oversize");
    assert_eq!(
        err,
        DomainError::InvalidArgument {
            field: "metadata",
            reason: tokens::METADATA_TOO_LARGE
        }
    );
    assert!(metrics.contract_failures().is_empty());
}

#[test]
fn a_condition_takes_a_scalar_property_and_values_of_its_type_and_enum() {
    let schema = json!({
        "allOf": [
            { "$ref": "gts://base" },
            { "properties": { "metadata": { "allOf": [
                { "properties": {
                    "tier": { "type": "string", "enum": ["gold", "silver"] },
                    "gpus": { "type": "integer" },
                } },
                { "properties": {
                    "spot": { "type": "boolean" },
                    "rate": { "type": "number" },
                    "tags": { "type": "array", "items": { "type": "string" } },
                } },
            ] } } },
        ]
    });
    assert!(
        paths(
            &schema,
            &json!({ "tier": ["gold"], "gpus": 8, "spot": false })
        )
        .is_empty()
    );
    assert_eq!(
        paths(
            &schema,
            &json!({
                "tier": "bronze",
                "gpus": [1, "2"],
                "spot": [],
                "rate": 1,
                "tags": "a",
                "a/b": 1,
            })
        ),
        ["/a~1b", "/gpus", "/rate", "/spot", "/tags", "/tier"]
    );
}
