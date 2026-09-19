//! Bounded, offline-rebuildable policy schemas from the immutable catalogue.
//! Registry references have already been resolved by catalogue bootstrap.
use super::{PolicySchemas, invalid};
use crate::domain::{DomainError, ProjectionContractCatalog};
use async_trait::async_trait;
use quota_enforcement_sdk::{
    EnvironmentInputs, MetricEnvironmentSchema, PolicySchemaSnapshot, PolicyScope, PolicyVersion,
};
use serde_json::{Value, json};
use std::sync::Arc;
use toolkit_macros::domain_model;

/// Persisted schema limits, distinct from compiled artifact cache limits.
#[domain_model]
#[derive(Clone, Copy, Debug)]
pub struct SnapshotLimits {
    /// Total serialized snapshot size.
    pub bytes: usize,
    /// Distinct resolved contract schemas.
    pub schemas: usize,
    /// Maximum resolved JSON nesting.
    pub depth: usize,
}

/// Shares the exact schemas used by admission; no live registry lookup can drift.
#[domain_model]
pub struct CatalogPolicySchemas {
    catalog: Arc<ProjectionContractCatalog>,
    limits: SnapshotLimits,
}

impl CatalogPolicySchemas {
    /// Construct from a successfully built catalogue and validated configuration.
    #[must_use]
    pub fn new(catalog: Arc<ProjectionContractCatalog>, limits: SnapshotLimits) -> Self {
        Self { catalog, limits }
    }

    /// The snapshot for `scope` and `engine`, keeping only the contracts behind
    /// `inputs`. An input the policy does not read is still present in each
    /// environment as an empty object (or `null` for the resource) so an
    /// offline rebuild type-checks exactly as the original save did.
    fn build(
        &self,
        scope: &PolicyScope,
        engine: &str,
        inputs: EnvironmentInputs,
    ) -> Result<PolicySchemaSnapshot, DomainError> {
        let mut metrics = match scope {
            PolicyScope::Global => self.catalog.admitted_metrics().cloned().collect::<Vec<_>>(),
            PolicyScope::Metric { metric } => {
                if self.catalog.request_contract(metric).is_none() {
                    return Err(invalid(
                        "scope",
                        DomainError::PROJECTION_NOT_RESOLVABLE,
                        "policy metric is outside the catalogue",
                    ));
                }
                vec![metric.clone()]
            }
        };
        if engine == "most-restrictive-wins" {
            // Reads no metadata at all, so no catalogue change can strand it.
            return Ok(PolicySchemaSnapshot {
                inputs: EnvironmentInputs::NONE,
                ..PolicySchemaSnapshot::default()
            });
        }
        metrics.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        if metrics.is_empty() {
            return Err(invalid(
                "scope",
                "POLICY_SCHEMA_EMPTY",
                "CEL requires at least one admitted metric",
            ));
        }
        let mut snapshot = PolicySchemaSnapshot {
            inputs,
            ..PolicySchemaSnapshot::default()
        };
        // Resource presence is optional; `null` is always one alternative.
        let mut resource_schemas = vec![json!({"type": "null"})];
        if inputs.resource {
            let mut resources: Vec<_> = self.catalog.resource_projections().collect();
            resources.sort_by(|a, b| a.type_id.as_ref().cmp(b.type_id.as_ref()));
            for resource in resources {
                self.insert(
                    &mut snapshot,
                    resource.type_id.as_ref(),
                    resource.contract.schema(),
                )?;
                resource_schemas.insert(0, resource.contract.schema().clone());
            }
        }
        for metric in metrics {
            let request = self.catalog.request_contract(&metric).ok_or_else(|| {
                invalid(
                    "scope",
                    DomainError::PROJECTION_NOT_RESOLVABLE,
                    "policy metric is outside the catalogue",
                )
            })?;
            let request_metadata = if inputs.request {
                self.insert(
                    &mut snapshot,
                    request.type_id.as_ref(),
                    request.contract.schema(),
                )?;
                metadata(request.contract.schema())?
            } else {
                unread()
            };
            let arbitration_metadata = if inputs.arbitration {
                self.insert(
                    &mut snapshot,
                    request.constraint.contract.type_id().as_ref(),
                    request.constraint.contract.schema(),
                )?;
                metadata(request.constraint.contract.schema())?
            } else {
                unread()
            };
            snapshot.environments.push(MetricEnvironmentSchema {
                metric,
                schema: json!({"type": "object", "additionalProperties": false,
                    "properties": {"request": request_metadata,
                        "resource": {"anyOf": resource_schemas}, "arbitration": arbitration_metadata}}),
            });
        }
        self.check_bounds(&snapshot)?;
        Ok(snapshot)
    }

    fn insert(
        &self,
        snapshot: &mut PolicySchemaSnapshot,
        id: &str,
        schema: &Value,
    ) -> Result<(), DomainError> {
        check_depth(schema, self.limits.depth)?;
        if !snapshot.schemas.contains_key(id) && snapshot.schemas.len() >= self.limits.schemas {
            return Err(invalid(
                "schema",
                "POLICY_SCHEMA_TOO_LARGE",
                "schema count exceeds configured bounds",
            ));
        }
        snapshot
            .schemas
            .entry(id.to_owned())
            .or_insert_with(|| schema.clone());
        self.check_bounds(snapshot)
    }

    fn check_bounds(&self, snapshot: &PolicySchemaSnapshot) -> Result<(), DomainError> {
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
        self.build(scope, engine, inputs)
    }

    fn check_activation(&self, version: &PolicyVersion) -> Result<(), DomainError> {
        // Rebuilt for the inputs the version actually reads: a resource added
        // since cannot strand a policy that never looked at resources.
        let current = self.build(
            &version.scope,
            &version.engine_id,
            version.schema_snapshot.inputs,
        )?;
        // Comparing the full validated set prevents a global policy from silently
        // covering newly admitted metrics that were never type checked. The
        // `inputs` are the recipe the set was built from, not part of it: a
        // version persisted before inputs were recorded rebuilds identically.
        if current.schemas != version.schema_snapshot.schemas
            || current.environments != version.schema_snapshot.environments
        {
            return Err(invalid(
                "schema",
                "POLICY_CATALOG_INCOMPATIBLE",
                "stored policy schemas differ from the active catalogue",
            ));
        }
        Ok(())
    }
}

/// The environment slot of an input the policy does not read: an object with
/// no properties, so any later reference is "absent" and the rebuild fails the
/// same way the original save would have.
fn unread() -> Value {
    json!({"type": "object", "properties": {}})
}

fn metadata(schema: &Value) -> Result<Value, DomainError> {
    // Resolved envelopes may use allOf; preserve all participating constraints.
    if let Some(metadata) = schema.pointer("/properties/metadata") {
        return Ok(metadata.clone());
    }
    if let Some(parts) = schema.get("allOf").and_then(Value::as_array) {
        let schemas: Vec<_> = parts
            .iter()
            .filter_map(|part| metadata(part).ok())
            .collect();
        if !schemas.is_empty() {
            return Ok(json!({"allOf": schemas}));
        }
    }
    Err(invalid(
        "schema",
        "INVALID_POLICY_SCHEMA",
        "contract has no metadata schema",
    ))
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
