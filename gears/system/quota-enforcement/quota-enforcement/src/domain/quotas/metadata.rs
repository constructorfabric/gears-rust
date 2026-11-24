//! Quota metadata validation (`cpt-cf-quota-enforcement-algo-quota-metadata-validation`,
//! ADR-0013): the metadata of a Quota holds its conditions over the `metadata`
//! of the request's metric object, checked against the metric type.

use quota_enforcement_sdk::QuotaMetadataViolation;
use serde_json::{Map, Value};

use crate::domain::catalog::ServedMetric;
use crate::domain::error::DomainError;
use crate::domain::ports::metrics::{QeMetrics, ValidationReason, ValidationSurface};
use crate::domain::tokens;

const LOG_TARGET: &str = "qe.quotas";

/// The property types a condition may name.
const SCALARS: [&str; 3] = ["string", "integer", "boolean"];

/// Check `metadata` of a Quota on the metric `served`: the size limit, then
/// each condition against the metric type's `metadata` schema.
///
/// # Errors
///
/// - [`DomainError::InvalidArgument`] with `METADATA_TOO_LARGE`.
/// - [`DomainError::QuotaMetadataRejected`] with one violation per condition
///   that does not fit.
pub fn check(
    metrics: &dyn QeMetrics,
    served: &ServedMetric,
    metadata: &Map<String, Value>,
    max_bytes: usize,
) -> Result<(), DomainError> {
    // @cpt-begin:cpt-cf-quota-enforcement-algo-quota-metadata-validation:p1:inst-qmd-size-if
    // @cpt-begin:cpt-cf-quota-enforcement-algo-quota-metadata-validation:p1:inst-qmd-size
    let encoded = serde_json::to_vec(metadata)
        .map_err(|e| DomainError::Internal(format!("metadata does not serialize: {e}")))?;
    if encoded.len() > max_bytes {
        tracing::warn!(
            target: LOG_TARGET,
            size = encoded.len(),
            limit = max_bytes,
            "quota metadata exceeds the configured size limit"
        );
        return Err(DomainError::InvalidArgument {
            field: "metadata",
            reason: tokens::METADATA_TOO_LARGE,
        });
    }
    // @cpt-end:cpt-cf-quota-enforcement-algo-quota-metadata-validation:p1:inst-qmd-size
    // @cpt-end:cpt-cf-quota-enforcement-algo-quota-metadata-validation:p1:inst-qmd-size-if

    // @cpt-begin:cpt-cf-quota-enforcement-algo-quota-metadata-validation:p1:inst-qmd-conditions
    let violations = violations(served.schema(), metadata);
    // @cpt-end:cpt-cf-quota-enforcement-algo-quota-metadata-validation:p1:inst-qmd-conditions

    // @cpt-begin:cpt-cf-quota-enforcement-algo-quota-metadata-validation:p1:inst-qmd-rejected-if
    if violations.is_empty() {
        return Ok(());
    }
    // @cpt-begin:cpt-cf-quota-enforcement-algo-quota-metadata-validation:p1:inst-qmd-rejected
    metrics.record_contract_validation_failure(
        ValidationSurface::QuotaWrite,
        ValidationReason::SchemaViolation,
    );
    tracing::warn!(
        target: LOG_TARGET,
        metric = %served.id(),
        violations = violations.len(),
        "a condition of the quota metadata does not fit the metric type"
    );
    Err(DomainError::QuotaMetadataRejected { violations })
    // @cpt-end:cpt-cf-quota-enforcement-algo-quota-metadata-validation:p1:inst-qmd-rejected
    // @cpt-end:cpt-cf-quota-enforcement-algo-quota-metadata-validation:p1:inst-qmd-rejected-if
}

/// What is wrong with each condition of `metadata` under the resolved metric
/// type `schema`, in key order.
pub(super) fn violations(
    schema: &Value,
    metadata: &Map<String, Value>,
) -> Vec<QuotaMetadataViolation> {
    let mut declared = Map::new();
    metadata_properties(schema, &mut declared);
    metadata
        .iter()
        .filter_map(|(name, condition)| {
            let reason = reason(declared.get(name), condition)?;
            Some(QuotaMetadataViolation {
                path: format!("/{}", name.replace('~', "~0").replace('/', "~1")),
                reason: reason.to_owned(),
            })
        })
        .collect()
}

/// Why `condition` does not fit the declared `property`, if it does not.
fn reason(property: Option<&Value>, condition: &Value) -> Option<&'static str> {
    let Some(property) = property else {
        return Some("the metric type declares no such metadata property");
    };
    let Some(kind) = property
        .get("type")
        .and_then(Value::as_str)
        .filter(|kind| SCALARS.contains(kind))
    else {
        return Some("a condition names only a string, integer, or boolean property");
    };
    let fits = |value: &Value| {
        let typed = match kind {
            "string" => value.is_string(),
            "integer" => value.is_i64() || value.is_u64(),
            _ => value.is_boolean(),
        };
        typed
            && property
                .get("enum")
                .and_then(Value::as_array)
                .is_none_or(|admitted| admitted.contains(value))
    };
    match condition {
        Value::Array(values) if values.is_empty() => Some("a list of values is empty"),
        Value::Array(values) if values.iter().all(fits) => None,
        Value::Array(_) => Some("a value does not fit the property's schema"),
        value if fits(value) => None,
        _ => Some("the value does not fit the property's schema"),
    }
}

/// Collect the properties of `metadata` that `schema` declares, through every
/// `allOf` part, at the root and inside `metadata` alike.
fn metadata_properties(schema: &Value, into: &mut Map<String, Value>) {
    if let Some(metadata) = schema.pointer("/properties/metadata") {
        object_properties(metadata, into);
    }
    for part in schema
        .get("allOf")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        metadata_properties(part, into);
    }
}

fn object_properties(schema: &Value, into: &mut Map<String, Value>) {
    if let Some(properties) = schema.get("properties").and_then(Value::as_object) {
        into.extend(properties.iter().map(|(k, v)| (k.clone(), v.clone())));
    }
    for part in schema
        .get("allOf")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        object_properties(part, into);
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "metadata_tests.rs"]
mod metadata_tests;
