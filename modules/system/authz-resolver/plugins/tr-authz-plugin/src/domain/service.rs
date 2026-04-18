//! Service implementation for the TR `AuthZ` resolver plugin.
//!
//! Resolves tenant hierarchy via `TenantResolverClient` and produces
//! `In` / `InGroup` / `InGroupSubtree` predicates based on the caller's
//! tenant scope.

use std::sync::Arc;

use authz_resolver_sdk::{
    Constraint, EvaluationRequest, EvaluationResponse, EvaluationResponseContext, InGroupPredicate,
    InGroupSubtreePredicate, InPredicate, Predicate,
};
use modkit_security::pep_properties;
use tenant_resolver_sdk::{
    BarrierMode, GetDescendantsOptions, TenantId, TenantResolverClient, TenantResolverError,
};
use tracing::{debug, info, warn};
use uuid::Uuid;

/// TR-based `AuthZ` resolver service.
///
/// Resolves tenant hierarchy via `TenantResolverClient`:
/// 1. Extracts tenant root from request context
/// 2. Calls `get_descendants` to resolve the visible tenant subtree (barriers handled by TR)
/// 3. Returns `In(owner_tenant_id, visible_tenants)` + optional `InGroup`/`InGroupSubtree`
pub struct Service {
    tr: Arc<dyn TenantResolverClient>,
}

impl Service {
    pub fn new(tr: Arc<dyn TenantResolverClient>) -> Self {
        Self { tr }
    }

    /// Evaluate an authorization request with tenant hierarchy resolution.
    #[allow(clippy::cognitive_complexity)]
    pub async fn evaluate(&self, request: &EvaluationRequest) -> EvaluationResponse {
        info!(
            action = %request.action.name,
            resource_type = %request.resource.resource_type,
            "tr-authz: evaluate called"
        );

        let tenant_id = request
            .context
            .tenant_context
            .as_ref()
            .and_then(|t| t.root_id)
            .or_else(|| {
                request
                    .subject
                    .properties
                    .get("tenant_id")
                    .and_then(|v| v.as_str())
                    .and_then(|s| Uuid::parse_str(s).ok())
            });

        let Some(tid) = tenant_id else {
            warn!("tr-authz: No tenant resolvable -- deny");
            return Self::deny();
        };

        info!(tenant_id = %tid, "tr-authz: tenant resolved");

        if tid == Uuid::default() {
            debug!("Nil UUID tenant -- deny");
            return Self::deny();
        }

        // Root tenant provisioning: create + is_tenant + no parent.
        // Allow without hierarchy check -- root tenants bootstrap from empty DB.
        let props = &request.resource.properties;
        let is_tenant = props
            .get("is_tenant")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        let has_parent = props.contains_key("parent_id") && !props["parent_id"].is_null();
        if request.action.name == "create" && is_tenant && !has_parent {
            debug!(tenant_id = %tid, "Root tenant provisioning -- allow without hierarchy check");
            return Self::allow_single_tenant(tid);
        }

        // Resolve tenant subtree via Tenant Resolver.
        // Barriers are handled by TR (BarrierMode::Respect).
        // No fallback: if tenant not found or TR error -- deny (fail-closed).
        let ctx = modkit_security::SecurityContext::anonymous();
        let visible_tenants = match self.resolve_tenant_subtree(&ctx, tid).await {
            Ok(tenants) => tenants,
            Err(e) => {
                warn!(error = %e, tenant_id = %tid, "Failed to resolve tenant hierarchy -- deny");
                return Self::deny();
            }
        };

        if visible_tenants.is_empty() {
            warn!(tenant_id = %tid, "tr-authz: Empty tenant subtree -- deny");
            return Self::deny();
        }

        info!(tenant_id = %tid, visible = visible_tenants.len(), "tr-authz: allow");

        // Build predicates
        let mut predicates = vec![Predicate::In(InPredicate::new(
            pep_properties::OWNER_TENANT_ID,
            visible_tenants,
        ))];

        // If request includes group context, add group predicates
        let props = &request.resource.properties;
        if let Some(group_ids) = props.get("group_ids")
            && let Some(ids) = Self::parse_uuid_array(group_ids)
            && !ids.is_empty()
        {
            predicates.push(Predicate::InGroup(InGroupPredicate::new("id", ids)));
        }
        if let Some(ancestor_ids) = props.get("ancestor_group_ids")
            && let Some(ids) = Self::parse_uuid_array(ancestor_ids)
            && !ids.is_empty()
        {
            predicates.push(Predicate::InGroupSubtree(InGroupSubtreePredicate::new(
                "id", ids,
            )));
        }

        EvaluationResponse {
            decision: true,
            context: EvaluationResponseContext {
                constraints: vec![Constraint { predicates }],
                ..Default::default()
            },
        }
    }

    /// Resolve the visible tenant subtree via Tenant Resolver.
    ///
    /// Calls `get_descendants` with `BarrierMode::Respect` — barriers and their
    /// subtrees are excluded by the tenant resolver plugin internally.
    async fn resolve_tenant_subtree(
        &self,
        ctx: &modkit_security::SecurityContext,
        tenant_id: Uuid,
    ) -> Result<Vec<Uuid>, String> {
        let response = self
            .tr
            .get_descendants(
                ctx,
                TenantId(tenant_id),
                &GetDescendantsOptions {
                    status: vec![],
                    barrier_mode: BarrierMode::Respect,
                    max_depth: None,
                },
            )
            .await
            .map_err(|e| match e {
                TenantResolverError::TenantNotFound { .. } => {
                    format!("Tenant {tenant_id} not found")
                }
                other => format!("TR error: {other}"),
            })?;

        // Collect: root tenant + all visible descendants
        let mut visible = Vec::with_capacity(response.descendants.len() + 1);
        visible.push(response.tenant.id.0);
        visible.extend(response.descendants.iter().map(|t| t.id.0));

        debug!(
            tenant_id = %tenant_id,
            descendants = response.descendants.len(),
            visible = visible.len(),
            "Resolved tenant subtree via TR"
        );

        Ok(visible)
    }

    fn allow_single_tenant(tid: Uuid) -> EvaluationResponse {
        EvaluationResponse {
            decision: true,
            context: EvaluationResponseContext {
                constraints: vec![Constraint {
                    predicates: vec![Predicate::In(InPredicate::new(
                        pep_properties::OWNER_TENANT_ID,
                        [tid],
                    ))],
                }],
                ..Default::default()
            },
        }
    }

    fn deny() -> EvaluationResponse {
        EvaluationResponse {
            decision: false,
            context: EvaluationResponseContext::default(),
        }
    }

    fn parse_uuid_array(value: &serde_json::Value) -> Option<Vec<Uuid>> {
        value.as_array().map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().and_then(|s| Uuid::parse_str(s).ok()))
                .collect()
        })
    }
}

#[cfg(test)]
#[path = "service_tests.rs"]
mod service_tests;
