mod alias;
mod bind;
mod budget;
mod merge;
mod resolution;
mod validation;

use std::sync::Arc;

use super::ControlPlaneService;
use crate::domain::ssrf::SsrfGuard;

use crate::domain::error::DomainError;
use crate::domain::model::{
    CreateRouteRequest, CreateUpstreamRequest, ListQuery, Route, UpdateRouteRequest,
    UpdateUpstreamRequest, Upstream,
};
use crate::domain::repo::{RouteRepository, UpstreamRepository};

#[cfg(test)]
use alias::compute_derived_alias;
use alias::{
    enforce_alias_create_derived, enforce_alias_create_with, enforce_alias_update_derived,
    enforce_alias_update_with, normalize_alias,
};
use bind::{BindOverrides, validate_ancestor_bind, validate_secret_ref_accessible};
use budget::validate_budget_config;
#[cfg(test)]
use merge::compute_effective_config;
#[cfg(test)]
use merge::merge_rate_limit;
use validation::{
    check_route_overlap, validate_endpoints, validate_endpoints_ssrf, validate_match_rules,
};

use async_trait::async_trait;
use authz_resolver_sdk::PolicyEnforcer;
use credstore_sdk::CredStoreClientV1;
use tenant_resolver_sdk::TenantResolverClient;
use toolkit_macros::domain_model;
use toolkit_security::SecurityContext;
use uuid::Uuid;

/// Control Plane service implementation backed by in-memory repositories.
#[domain_model]
pub(crate) struct ControlPlaneServiceImpl {
    upstreams: Arc<dyn UpstreamRepository>,
    routes: Arc<dyn RouteRepository>,
    tenant_resolver: Arc<dyn TenantResolverClient>,
    policy_enforcer: PolicyEnforcer,
    credstore: Arc<dyn CredStoreClientV1>,
    ssrf_guard: Arc<SsrfGuard>,
}

impl ControlPlaneServiceImpl {
    /// Create the control-plane service from its repositories and collaborators.
    #[must_use]
    pub(crate) fn new(
        upstreams: Arc<dyn UpstreamRepository>,
        routes: Arc<dyn RouteRepository>,
        tenant_resolver: Arc<dyn TenantResolverClient>,
        policy_enforcer: PolicyEnforcer,
        credstore: Arc<dyn CredStoreClientV1>,
        ssrf_guard: Arc<SsrfGuard>,
    ) -> Self {
        Self {
            upstreams,
            routes,
            tenant_resolver,
            policy_enforcer,
            credstore,
            ssrf_guard,
        }
    }
}

// ===========================================================================
// Trait implementation — public API surface
// ===========================================================================

#[async_trait]
impl ControlPlaneService for ControlPlaneServiceImpl {
    // -- Upstream CRUD --

    async fn create_upstream(
        &self,
        ctx: &SecurityContext,
        req: CreateUpstreamRequest,
    ) -> Result<Upstream, DomainError> {
        validate_endpoints(&req.server.endpoints)?;
        validate_endpoints_ssrf(&self.ssrf_guard, &req.server.endpoints)?;
        if let Some(ref cors) = req.cors {
            crate::domain::cors::validate_cors_config(cors)?;
        }
        if let Some(ref rl) = req.rate_limit
            && let Some(ref budget) = rl.budget
        {
            validate_budget_config(budget)?;
        }

        // Enforce alias derivation / explicit rules.
        let alias = match req.alias.as_deref() {
            Some(user_alias) => enforce_alias_create_with(user_alias, &req.server.endpoints)?,
            None => enforce_alias_create_derived(&req.server.endpoints)?,
        };

        let tenant_id = ctx.subject_tenant_id();
        let id = req.id.unwrap_or_else(Uuid::new_v4);
        let tenant_chain = self.build_tenant_chain(ctx).await?;

        // Check if an ancestor tenant has an upstream with this alias.
        // If so, this is a "bind" operation requiring ancestor bind validation.
        validate_ancestor_bind(
            ctx,
            &*self.upstreams,
            &self.policy_enforcer,
            &tenant_chain,
            &alias,
            &BindOverrides {
                auth: req.auth.as_ref(),
                rate_limit: req.rate_limit.as_ref(),
                plugins: req.plugins.as_ref(),
                cors: req.cors.as_ref(),
            },
        )
        .await?;

        // Verify the descendant's secret_ref is reachable (fail-closed).
        if let Some(ref auth) = req.auth
            && let Some(ref config) = auth.config
            && let Some(raw_ref) = config.get("secret_ref")
        {
            validate_secret_ref_accessible(ctx, self.credstore.as_ref(), raw_ref).await?;
        }

        self.validate_budget_allocation(ctx, &tenant_chain, &alias, req.rate_limit.as_ref())
            .await?;

        let upstream = Upstream {
            id,
            tenant_id,
            alias,
            server: req.server,
            protocol: req.protocol,
            enabled: req.enabled,
            auth: req.auth,
            headers: req.headers,
            plugins: req.plugins,
            rate_limit: req.rate_limit,
            cors: req.cors,
            tags: req.tags,
        };

        self.upstreams
            .create(upstream)
            .await
            .map_err(DomainError::from)
    }

    async fn get_upstream(&self, ctx: &SecurityContext, id: Uuid) -> Result<Upstream, DomainError> {
        let tenant_id = ctx.subject_tenant_id();
        self.upstreams
            .get_by_id(tenant_id, id)
            .await
            .map_err(|_| DomainError::not_found("upstream", id))
    }

    async fn list_upstreams(
        &self,
        ctx: &SecurityContext,
        query: &ListQuery,
    ) -> Result<Vec<Upstream>, DomainError> {
        let tenant_id = ctx.subject_tenant_id();
        self.upstreams
            .list(tenant_id, query)
            .await
            .map_err(DomainError::from)
    }

    async fn update_upstream(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
        req: UpdateUpstreamRequest,
    ) -> Result<Upstream, DomainError> {
        let tenant_id = ctx.subject_tenant_id();
        let mut existing = self
            .upstreams
            .get_by_id(tenant_id, id)
            .await
            .map_err(|_| DomainError::not_found("upstream", id))?;

        // Snapshot old endpoints before applying server update (needed for alias enforcement).
        let old_endpoints = existing.server.endpoints.clone();

        // Full replacement: validate and apply server.
        validate_endpoints(&req.server.endpoints)?;
        validate_endpoints_ssrf(&self.ssrf_guard, &req.server.endpoints)?;
        existing.server = req.server;
        existing.protocol = req.protocol;

        // Enforce alias re-evaluation when endpoints change.
        let endpoints_changed = existing.server.endpoints != old_endpoints;
        if endpoints_changed {
            let alias = match req.alias.as_deref() {
                Some(user_alias) => enforce_alias_update_with(
                    user_alias,
                    &existing.server.endpoints,
                    &existing.alias,
                    &old_endpoints,
                )?,
                None => enforce_alias_update_derived(
                    &existing.server.endpoints,
                    &existing.alias,
                    &old_endpoints,
                )?,
            };
            existing.alias = alias;
        } else if let Some(ref user_alias) = req.alias {
            let normalized = normalize_alias(user_alias);
            // No endpoint change — alias can only be confirmed (exact match),
            // never changed. This applies to both hostname-derived and
            // IP-based upstreams: the alias is the routing key and renaming
            // it would break API clients.
            if normalized != existing.alias {
                return Err(DomainError::validation(format!(
                    "alias cannot be changed from '{}' to '{}'; \
                     delete and re-create the upstream instead",
                    existing.alias, normalized
                )));
            }
        }

        // Structural validation first (matches create_upstream ordering).
        if let Some(ref cors) = req.cors {
            crate::domain::cors::validate_cors_config(cors)?;
        }
        if let Some(ref rl) = req.rate_limit
            && let Some(ref budget) = rl.budget
        {
            validate_budget_config(budget)?;
        }

        // Full-replacement: always validate ancestor bind constraints and budget
        // allocation against the final state. Even None fields are meaningful —
        // setting rate_limit to None removes the allocation.
        let tenant_chain = self.build_tenant_chain(ctx).await?;
        validate_ancestor_bind(
            ctx,
            &*self.upstreams,
            &self.policy_enforcer,
            &tenant_chain,
            &existing.alias,
            &BindOverrides {
                auth: req.auth.as_ref(),
                rate_limit: req.rate_limit.as_ref(),
                plugins: req.plugins.as_ref(),
                cors: req.cors.as_ref(),
            },
        )
        .await?;

        // Verify the descendant's secret_ref is reachable (fail-closed).
        if let Some(ref auth) = req.auth
            && let Some(ref config) = auth.config
            && let Some(raw_ref) = config.get("secret_ref")
        {
            validate_secret_ref_accessible(ctx, self.credstore.as_ref(), raw_ref).await?;
        }

        self.validate_budget_allocation(
            ctx,
            &tenant_chain,
            &existing.alias,
            req.rate_limit.as_ref(),
        )
        .await?;

        // If this upstream's budget is being tightened (lower total, lower
        // overcommit_ratio, or mode changed to allocated), verify that existing
        // descendant allocations still fit within the proposed budget.
        if let Some(ref new_rl) = req.rate_limit {
            let old_budget = existing
                .rate_limit
                .as_ref()
                .and_then(|rl| rl.budget.as_ref());
            let new_budget = new_rl.budget.as_ref();
            let budget_changed = old_budget != new_budget;
            if budget_changed {
                self.validate_descendants_within_budget(ctx, &existing.alias, new_rl)
                    .await?;
            }
        }

        // Full replacement: directly assign all fields (None = unset).
        existing.auth = req.auth;
        existing.headers = req.headers;
        existing.plugins = req.plugins;
        existing.rate_limit = req.rate_limit;
        existing.cors = req.cors;
        existing.tags = req.tags;
        existing.enabled = req.enabled;

        self.upstreams
            .update(existing)
            .await
            .map_err(DomainError::from)
    }

    async fn delete_upstream(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
    ) -> Result<Vec<Uuid>, DomainError> {
        let tenant_id = ctx.subject_tenant_id();
        // Cascade delete routes before removing the upstream.
        let deleted_route_ids = self
            .routes
            .delete_by_upstream(tenant_id, id)
            .await
            .map_err(DomainError::from)?;
        self.upstreams
            .delete(tenant_id, id)
            .await
            .map_err(|_| DomainError::not_found("upstream", id))?;
        Ok(deleted_route_ids)
    }

    // -- Route CRUD --

    async fn create_route(
        &self,
        ctx: &SecurityContext,
        req: CreateRouteRequest,
    ) -> Result<Route, DomainError> {
        if let Some(ref cors) = req.cors {
            crate::domain::cors::validate_cors_config(cors)?;
        }

        let tenant_id = ctx.subject_tenant_id();
        // Validate that the upstream exists and belongs to this tenant.
        self.upstreams
            .get_by_id(tenant_id, req.upstream_id)
            .await
            .map_err(|_| {
                DomainError::validation(format!(
                    "upstream '{}' not found for this tenant",
                    req.upstream_id
                ))
            })?;

        let route = Route {
            id: req.id.unwrap_or_else(Uuid::new_v4),
            tenant_id,
            upstream_id: req.upstream_id,
            match_rules: req.match_rules,
            plugins: req.plugins,
            rate_limit: req.rate_limit,
            cors: req.cors,
            tags: req.tags,
            priority: req.priority,
            enabled: req.enabled,
        };

        validate_match_rules(&route.match_rules)?;
        check_route_overlap(&*self.routes, &route, None).await?;

        self.routes.create(route).await.map_err(DomainError::from)
    }

    async fn get_route(&self, ctx: &SecurityContext, id: Uuid) -> Result<Route, DomainError> {
        let tenant_id = ctx.subject_tenant_id();
        self.routes
            .get_by_id(tenant_id, id)
            .await
            .map_err(|_| DomainError::not_found("route", id))
    }

    async fn list_routes(
        &self,
        ctx: &SecurityContext,
        upstream_id: Option<Uuid>,
        query: &ListQuery,
    ) -> Result<Vec<Route>, DomainError> {
        let tenant_id = ctx.subject_tenant_id();
        self.routes
            .list(tenant_id, upstream_id, query)
            .await
            .map_err(DomainError::from)
    }

    async fn update_route(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
        req: UpdateRouteRequest,
    ) -> Result<Route, DomainError> {
        let tenant_id = ctx.subject_tenant_id();
        let mut existing = self
            .routes
            .get_by_id(tenant_id, id)
            .await
            .map_err(|_| DomainError::not_found("route", id))?;

        // Full replacement: directly assign all fields (None = unset).
        existing.match_rules = req.match_rules;
        existing.plugins = req.plugins;
        existing.rate_limit = req.rate_limit;
        if let Some(ref cors) = req.cors {
            crate::domain::cors::validate_cors_config(cors)?;
        }
        existing.cors = req.cors;
        existing.tags = req.tags;
        existing.priority = req.priority;
        existing.enabled = req.enabled;

        validate_match_rules(&existing.match_rules)?;
        check_route_overlap(&*self.routes, &existing, Some(existing.id)).await?;

        self.routes
            .update(existing)
            .await
            .map_err(DomainError::from)
    }

    async fn delete_route(&self, ctx: &SecurityContext, id: Uuid) -> Result<(), DomainError> {
        let tenant_id = ctx.subject_tenant_id();
        self.routes
            .delete(tenant_id, id)
            .await
            .map_err(|_| DomainError::not_found("route", id))
    }

    // -- Resolution --

    async fn resolve_proxy_target(
        &self,
        ctx: &SecurityContext,
        alias: &str,
        method: &str,
        path: &str,
    ) -> Result<(Upstream, Route), DomainError> {
        let tenant_chain = self.build_tenant_chain(ctx).await?;
        let (effective, route) = self
            .resolve_alias(ctx, &tenant_chain, alias, Some((method, path)))
            .await?;
        Ok((
            effective,
            route.ok_or_else(|| DomainError::Internal {
                message: "resolve_alias returned None route for method+path request".into(),
            })?,
        ))
    }
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
