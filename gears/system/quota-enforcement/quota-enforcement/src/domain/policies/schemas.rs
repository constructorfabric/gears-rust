//! The bounded schema snapshot of a Policy version: the resolved schema of the
//! metric type a `metric`-scope Policy is checked against, read through the
//! metric catalogue (ADR-0012).
use super::{PolicySchemas, invalid};
use crate::domain::DomainError;
use crate::domain::catalog::MetricCatalog;
use crate::domain::ports::metrics::ValidationSurface;
use async_trait::async_trait;
use quota_enforcement_sdk::{EnvironmentInputs, PolicySchemaSnapshot, PolicyScope};
use serde_json::Value;
use std::sync::Arc;
use toolkit_macros::domain_model;

/// Persisted schema limits, distinct from compiled artifact cache limits.
#[domain_model]
#[derive(Clone, Copy, Debug)]
pub struct SnapshotLimits {
    /// Total serialized snapshot size.
    pub bytes: usize,
    /// Distinct resolved schemas.
    pub schemas: usize,
    /// Maximum resolved JSON nesting.
    pub depth: usize,
}

/// Snapshots from the served catalogue entries, the ones admission uses.
#[domain_model]
pub struct CatalogPolicySchemas {
    catalog: Arc<MetricCatalog>,
    limits: SnapshotLimits,
}

impl CatalogPolicySchemas {
    /// Read through `catalog`, bounded by `limits`.
    #[must_use]
    pub fn new(catalog: Arc<MetricCatalog>, limits: SnapshotLimits) -> Self {
        Self { catalog, limits }
    }

    fn check_bounds(&self, snapshot: &PolicySchemaSnapshot) -> Result<(), DomainError> {
        if snapshot.schemas.len() > self.limits.schemas {
            return Err(invalid(
                "schema",
                "POLICY_SCHEMA_TOO_LARGE",
                "schema count exceeds configured bounds",
            ));
        }
        for schema in snapshot.schemas.values() {
            check_depth(schema, self.limits.depth)?;
        }
        let encoded = serde_json::to_vec(snapshot).map_err(|_| {
            invalid(
                "schema",
                "INVALID_POLICY_SCHEMA",
                "snapshot does not serialize",
            )
        })?;
        if encoded.len() > self.limits.bytes {
            return Err(invalid(
                "schema",
                "POLICY_SCHEMA_TOO_LARGE",
                "snapshot exceeds configured byte bound",
            ));
        }
        Ok(())
    }
}

#[async_trait]
impl PolicySchemas for CatalogPolicySchemas {
    async fn snapshot(
        &self,
        scope: &PolicyScope,
        engine: &str,
        inputs: EnvironmentInputs,
    ) -> Result<PolicySchemaSnapshot, DomainError> {
        let mut snapshot = PolicySchemaSnapshot {
            inputs,
            ..PolicySchemaSnapshot::default()
        };
        let PolicyScope::Metric { metric } = scope else {
            return Ok(snapshot);
        };
        // @cpt-begin:cpt-cf-quota-enforcement-algo-catalog-membership:p1:inst-mem-lookup
        // @cpt-begin:cpt-cf-quota-enforcement-algo-catalog-membership:p1:inst-mem-entry-if
        // @cpt-begin:cpt-cf-quota-enforcement-algo-catalog-membership:p1:inst-mem-entry
        let served = self
            .catalog
            .served(metric, ValidationSurface::Policy)
            .await?;
        // @cpt-end:cpt-cf-quota-enforcement-algo-catalog-membership:p1:inst-mem-entry
        // @cpt-end:cpt-cf-quota-enforcement-algo-catalog-membership:p1:inst-mem-entry-if
        // @cpt-end:cpt-cf-quota-enforcement-algo-catalog-membership:p1:inst-mem-lookup
        if engine == "most-restrictive-wins" || !inputs.request {
            snapshot.inputs = if engine == "most-restrictive-wins" {
                EnvironmentInputs::NONE
            } else {
                inputs
            };
            return Ok(snapshot);
        }
        snapshot
            .schemas
            .insert(metric.to_string(), served.schema().clone());
        self.check_bounds(&snapshot)?;
        Ok(snapshot)
    }
}

fn check_depth(root: &Value, max: usize) -> Result<(), DomainError> {
    let mut pending = vec![(root, 0)];
    while let Some((value, depth)) = pending.pop() {
        if depth > max {
            return Err(invalid(
                "schema",
                "POLICY_SCHEMA_TOO_DEEP",
                "resolved schema exceeds configured nesting bound",
            ));
        }
        match value {
            Value::Object(map) => pending.extend(map.values().map(|v| (v, depth + 1))),
            Value::Array(items) => pending.extend(items.iter().map(|v| (v, depth + 1))),
            _ => {}
        }
    }
    Ok(())
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "schemas_tests.rs"]
mod schemas_tests;
