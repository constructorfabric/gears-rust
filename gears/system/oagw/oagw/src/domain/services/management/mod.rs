mod alias;
mod ancestor;
mod bind;
mod budget;
mod canonical;
mod merge;
mod resolution;
mod validation;

use std::sync::Arc;

use super::{ControlPlaneService, RegistryProvisioner};
use crate::domain::ssrf::SsrfGuard;

use crate::domain::error::DomainError;
use crate::domain::model::{
    CreateRouteRequest, CreateUpstreamRequest, ListQuery, ManagedBy, Route, UpdateRouteRequest,
    UpdateUpstreamRequest, Upstream,
};
use crate::domain::repo::{RepositoryError, RouteRepository, RowKey, Tags, UpstreamRepository};

#[cfg(test)]
pub(crate) use alias::MAX_ALIAS_LENGTH;
#[cfg(test)]
use alias::compute_derived_alias;
use alias::{
    enforce_alias_create_derived, enforce_alias_create_with, enforce_alias_update_derived,
    enforce_alias_update_with, normalize_alias,
};
use ancestor::closest_ancestor_upstream;
use bind::{BindOverrides, validate_ancestor_bind, validate_secret_ref_accessible};
use budget::validate_budget_config;
use canonical::{canonical_tags, canonicalize_match_rules};
pub(crate) use canonical::{route_drift, upstream_drift};
#[cfg(test)]
use merge::compute_effective_config;
#[cfg(test)]
use merge::merge_rate_limit;
pub(crate) use validation::check_registry_route_overlaps;
#[cfg(test)]
pub(crate) use validation::{MAX_GRPC_NAME_BYTES, MAX_PATH_BYTES, MAX_REF_BYTES, MAX_TAG_BYTES};
use validation::{
    check_route_overlap, validate_endpoints, validate_endpoints_ssrf, validate_json_text,
    validate_match_fields, validate_match_rules, validate_plugin_refs, validate_tags,
    validate_upstream_refs,
};

use async_trait::async_trait;
use authz_resolver_sdk::PolicyEnforcer;
use credstore_sdk::CredStoreClientV1;
use tenant_resolver_sdk::TenantResolverClient;
use toolkit_macros::domain_model;
use toolkit_security::SecurityContext;
use uuid::Uuid;

/// Control Plane service implementation over the upstream and route
/// repositories (in memory or in the database, ADR-0018).
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
        self.create_upstream_as(ctx, req, ManagedBy::Api).await
    }

    async fn get_upstream(&self, ctx: &SecurityContext, id: Uuid) -> Result<Upstream, DomainError> {
        let tenant_id = ctx.subject_tenant_id();
        self.upstreams
            .get_by_id(tenant_id, id)
            .await
            .map_err(DomainError::from)
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
        self.update_upstream_as(ctx, id, req, ManagedBy::Api).await
    }

    async fn delete_upstream(&self, ctx: &SecurityContext, id: Uuid) -> Result<(), DomainError> {
        self.delete_upstream_as(ctx, id, ManagedBy::Api).await
    }

    // -- Route CRUD --

    async fn create_route(
        &self,
        ctx: &SecurityContext,
        req: CreateRouteRequest,
    ) -> Result<Route, DomainError> {
        self.create_route_as(ctx, req, ManagedBy::Api).await
    }

    async fn get_route(&self, ctx: &SecurityContext, id: Uuid) -> Result<Route, DomainError> {
        let tenant_id = ctx.subject_tenant_id();
        self.routes
            .get_by_id(tenant_id, id)
            .await
            .map_err(DomainError::from)
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
        self.update_route_as(ctx, id, req, ManagedBy::Api).await
    }

    async fn delete_route(&self, ctx: &SecurityContext, id: Uuid) -> Result<(), DomainError> {
        self.delete_route_as(ctx, id, ManagedBy::Api).await
    }

    // -- Resolution --

    async fn resolve_proxy_target(
        &self,
        ctx: &SecurityContext,
        alias: &str,
        method: &str,
        path: &str,
        tags: Tags,
    ) -> Result<(Upstream, Route), DomainError> {
        let tenant_chain = self.build_tenant_chain(ctx).await?;
        let (effective, route) = self
            .resolve_alias(ctx, &tenant_chain, alias, Some((method, path)), tags)
            .await?;
        Ok((
            effective,
            route.ok_or_else(|| DomainError::Internal {
                message: "resolve_alias returned None route for method+path request".into(),
            })?,
        ))
    }
}

// ===========================================================================
// Writes on behalf of an owner: the Management API or the registry reconcile
// ===========================================================================

/// A row changes only through its owner: the Management API and the SDK for
/// rows they created, the types-registry reconcile for registry rows.
fn ensure_owner(
    entity: &'static str,
    id: Uuid,
    owner: ManagedBy,
    writer: ManagedBy,
) -> Result<(), DomainError> {
    match (owner, writer) {
        (ManagedBy::Registry, ManagedBy::Api) => Err(DomainError::registry_managed(entity, id)),
        (ManagedBy::Api, ManagedBy::Registry) => Err(DomainError::conflict(
            entity,
            id.to_string(),
            format!("{entity} {id} was created through the Management API, not the types registry"),
        )),
        (ManagedBy::Api, ManagedBy::Api) | (ManagedBy::Registry, ManagedBy::Registry) => Ok(()),
    }
}

impl ControlPlaneServiceImpl {
    async fn create_upstream_as(
        &self,
        ctx: &SecurityContext,
        req: CreateUpstreamRequest,
        managed_by: ManagedBy,
    ) -> Result<Upstream, DomainError> {
        validate_tags(&req.tags)?;
        validate_upstream_refs(&req.protocol, req.auth.as_ref())?;
        validate_plugin_refs(req.plugins.as_ref())?;
        validate_json_text(
            req.auth.as_ref(),
            req.headers.as_ref(),
            req.cors.as_ref(),
            req.plugins.as_ref(),
        )?;
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
        let ancestor = closest_ancestor_upstream(&*self.upstreams, &tenant_chain, &alias).await?;
        validate_ancestor_bind(
            ctx,
            &self.policy_enforcer,
            ancestor.as_ref(),
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
            tenant_id,
            ancestor.as_ref(),
            &alias,
            req.rate_limit.as_ref(),
        )
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
            tags: canonical_tags(req.tags),
            managed_by,
        };

        self.upstreams
            .create(upstream)
            .await
            .map_err(DomainError::from)
    }

    async fn update_upstream_as(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
        req: UpdateUpstreamRequest,
        writer: ManagedBy,
    ) -> Result<Upstream, DomainError> {
        let tenant_id = ctx.subject_tenant_id();
        let mut existing = self
            .upstreams
            .get_by_id(tenant_id, id)
            .await
            .map_err(DomainError::from)?;
        ensure_owner("upstream", id, existing.managed_by, writer)?;

        // Snapshot old endpoints before applying server update (needed for alias enforcement).
        let old_endpoints = existing.server.endpoints.clone();

        validate_tags(&req.tags)?;
        validate_upstream_refs(&req.protocol, req.auth.as_ref())?;
        validate_plugin_refs(req.plugins.as_ref())?;
        validate_json_text(
            req.auth.as_ref(),
            req.headers.as_ref(),
            req.cors.as_ref(),
            req.plugins.as_ref(),
        )?;

        // Full replacement: validate and apply server.
        validate_endpoints(&req.server.endpoints)?;
        validate_endpoints_ssrf(&self.ssrf_guard, &req.server.endpoints)?;
        existing.server = req.server;
        existing.protocol = req.protocol;

        // Enforce alias re-evaluation when endpoints change.
        let endpoints_changed = existing.server.endpoints != old_endpoints;
        if writer == ManagedBy::Registry {
            // A types-registry upstream takes the alias its instance now
            // gives, by the create rules: the registry cannot delete and
            // re-create the row without cascading away API routes on it.
            existing.alias = match req.alias.as_deref() {
                Some(user_alias) => {
                    enforce_alias_create_with(user_alias, &existing.server.endpoints)?
                }
                None => enforce_alias_create_derived(&existing.server.endpoints)?,
            };
        } else if endpoints_changed {
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
        let ancestor =
            closest_ancestor_upstream(&*self.upstreams, &tenant_chain, &existing.alias).await?;
        validate_ancestor_bind(
            ctx,
            &self.policy_enforcer,
            ancestor.as_ref(),
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
            tenant_id,
            ancestor.as_ref(),
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
        existing.tags = canonical_tags(req.tags);
        existing.enabled = req.enabled;

        self.upstreams
            .update(existing)
            .await
            .map_err(DomainError::from)
    }

    async fn delete_upstream_as(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
        writer: ManagedBy,
    ) -> Result<(), DomainError> {
        let tenant_id = ctx.subject_tenant_id();
        let existing = self
            .upstreams
            .get_by_id(tenant_id, id)
            .await
            .map_err(DomainError::from)?;
        ensure_owner("upstream", id, existing.managed_by, writer)?;
        // Upstream first. On a database this one statement cascades to the
        // routes, and the foreign key rejects any later route on it. In memory
        // the second call removes the routes; a route create already past its
        // upstream check can still land after it and stay orphaned.
        self.upstreams
            .delete(tenant_id, id)
            .await
            .map_err(DomainError::from)?;
        self.routes
            .delete_by_upstream(tenant_id, id)
            .await
            .map_err(DomainError::from)
    }

    async fn create_route_as(
        &self,
        ctx: &SecurityContext,
        req: CreateRouteRequest,
        managed_by: ManagedBy,
    ) -> Result<Route, DomainError> {
        validate_tags(&req.tags)?;
        validate_match_fields(&req.match_rules)?;
        validate_plugin_refs(req.plugins.as_ref())?;
        validate_json_text(None, None, req.cors.as_ref(), req.plugins.as_ref())?;
        if let Some(ref cors) = req.cors {
            crate::domain::cors::validate_cors_config(cors)?;
        }

        let tenant_id = ctx.subject_tenant_id();
        let upstream_id = req.upstream_id;
        let upstream_missing = || {
            DomainError::validation(format!(
                "upstream '{upstream_id}' not found for this tenant"
            ))
        };
        // Validate that the upstream exists and belongs to this tenant.
        let upstream = self
            .upstreams
            .get_by_id(tenant_id, upstream_id)
            .await
            .map_err(|e| match e {
                RepositoryError::NotFound { .. } => upstream_missing(),
                other => DomainError::from(other),
            })?;
        // A registry route on an API upstream would be deleted with it, and
        // every later boot would fail to re-create the route.
        if managed_by == ManagedBy::Registry && upstream.managed_by != ManagedBy::Registry {
            return Err(DomainError::validation(format!(
                "upstream '{upstream_id}' was created through the Management API; \
                 a types-registry route must name a types-registry upstream"
            )));
        }

        let mut route = Route {
            id: req.id.unwrap_or_else(Uuid::new_v4),
            tenant_id,
            upstream_id: req.upstream_id,
            match_rules: req.match_rules,
            plugins: req.plugins,
            rate_limit: req.rate_limit,
            cors: req.cors,
            tags: canonical_tags(req.tags),
            priority: req.priority,
            enabled: req.enabled,
            managed_by,
        };
        canonicalize_match_rules(&mut route.match_rules);

        validate_match_rules(&route.match_rules)?;
        check_route_overlap(&*self.routes, &route, None, managed_by).await?;

        // On a database the foreign key rejects a route whose upstream was
        // deleted after the check above; answer as the check would have.
        self.routes.create(route).await.map_err(|e| match e {
            RepositoryError::NotFound {
                entity: "upstream", ..
            } => upstream_missing(),
            other => DomainError::from(other),
        })
    }

    async fn update_route_as(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
        req: UpdateRouteRequest,
        writer: ManagedBy,
    ) -> Result<Route, DomainError> {
        let tenant_id = ctx.subject_tenant_id();
        let mut existing = self
            .routes
            .get_by_id(tenant_id, id)
            .await
            .map_err(DomainError::from)?;
        ensure_owner("route", id, existing.managed_by, writer)?;

        validate_tags(&req.tags)?;
        validate_match_fields(&req.match_rules)?;
        validate_plugin_refs(req.plugins.as_ref())?;
        validate_json_text(None, None, req.cors.as_ref(), req.plugins.as_ref())?;

        // Full replacement: directly assign all fields (None = unset).
        existing.match_rules = req.match_rules;
        existing.plugins = req.plugins;
        existing.rate_limit = req.rate_limit;
        if let Some(ref cors) = req.cors {
            crate::domain::cors::validate_cors_config(cors)?;
        }
        existing.cors = req.cors;
        existing.tags = canonical_tags(req.tags);
        existing.priority = req.priority;
        existing.enabled = req.enabled;
        canonicalize_match_rules(&mut existing.match_rules);

        validate_match_rules(&existing.match_rules)?;
        check_route_overlap(&*self.routes, &existing, Some(existing.id), writer).await?;

        self.routes
            .update(existing)
            .await
            .map_err(DomainError::from)
    }

    async fn delete_route_as(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
        writer: ManagedBy,
    ) -> Result<(), DomainError> {
        let tenant_id = ctx.subject_tenant_id();
        let existing = self
            .routes
            .get_by_id(tenant_id, id)
            .await
            .map_err(DomainError::from)?;
        ensure_owner("route", id, existing.managed_by, writer)?;
        self.routes
            .delete(tenant_id, id)
            .await
            .map_err(DomainError::from)
    }
}

#[async_trait]
impl RegistryProvisioner for ControlPlaneServiceImpl {
    async fn create_registry_upstream(
        &self,
        ctx: &SecurityContext,
        req: CreateUpstreamRequest,
    ) -> Result<Upstream, DomainError> {
        self.create_upstream_as(ctx, req, ManagedBy::Registry).await
    }

    async fn update_registry_upstream(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
        req: UpdateUpstreamRequest,
    ) -> Result<Upstream, DomainError> {
        self.update_upstream_as(ctx, id, req, ManagedBy::Registry)
            .await
    }

    async fn delete_registry_upstream(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
    ) -> Result<(), DomainError> {
        self.delete_upstream_as(ctx, id, ManagedBy::Registry).await
    }

    async fn create_registry_route(
        &self,
        ctx: &SecurityContext,
        req: CreateRouteRequest,
    ) -> Result<Route, DomainError> {
        self.create_route_as(ctx, req, ManagedBy::Registry).await
    }

    async fn update_registry_route(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
        req: UpdateRouteRequest,
    ) -> Result<Route, DomainError> {
        self.update_route_as(ctx, id, req, ManagedBy::Registry)
            .await
    }

    async fn delete_registry_route(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
    ) -> Result<(), DomainError> {
        self.delete_route_as(ctx, id, ManagedBy::Registry).await
    }

    async fn registry_upstream_keys(&self) -> Result<Vec<RowKey>, DomainError> {
        self.upstreams
            .list_registry_keys()
            .await
            .map_err(DomainError::from)
    }

    async fn registry_route_keys(&self) -> Result<Vec<RowKey>, DomainError> {
        self.routes
            .list_registry_keys()
            .await
            .map_err(DomainError::from)
    }
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
