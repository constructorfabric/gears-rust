mod alias;
mod ancestor;
mod authz;
mod bind;
mod budget;
mod canonical;
mod merge;
mod registry;
mod resolution;
mod validation;
mod withheld;

use std::sync::Arc;

use super::{ControlPlaneService, ProxyGrant, UpstreamCheck};
use crate::domain::ssrf::SsrfGuard;

use crate::domain::error::{DeniedOn, DomainError};
use crate::domain::model::{
    CreateRouteRequest, CreateUpstreamRequest, ListQuery, ManagedBy, ProxyTarget, Route,
    UpdateRouteRequest, UpdateUpstreamRequest, Upstream,
};
use crate::domain::ports::OagwMetricsPort;
use crate::domain::rate_limit::RateLimiter;
use crate::domain::repo::{
    ProxyConfigReader, RepositoryError, RouteRepository, Tags, UpstreamRepository,
};

// The change log decoder bounds aliases with it (the `cluster` feature).
#[cfg(any(test, feature = "cluster"))]
pub(crate) use alias::MAX_ALIAS_LENGTH;
#[cfg(test)]
use alias::compute_derived_alias;
use alias::{
    enforce_alias_create_derived, enforce_alias_create_with, enforce_alias_update_derived,
    enforce_alias_update_with, normalize_alias,
};
use ancestor::ancestor_upstreams;
use authz::{Action, Managed};
use bind::{BindOverrides, validate_bind_constraints, validate_secret_ref_accessible};
use budget::validate_shared_pool_cost;
use canonical::canonical_tags;
pub(crate) use canonical::{route_drift, upstream_drift};
#[cfg(test)]
use merge::compute_effective_config;
#[cfg(test)]
use merge::merge_rate_limit;
pub(crate) use registry::RegistryProvisionerImpl;
use resolution::ResolvedAlias;
pub(in crate::domain::services) use validation::validate_route_request;
#[cfg(test)]
pub(crate) use validation::{MAX_GRPC_NAME_BYTES, MAX_PATH_BYTES, MAX_REF_BYTES, MAX_TAG_BYTES};
pub(crate) use validation::{forbid_http_endpoints, overlapping_method, registry_route_overlaps};
use validation::{
    route_overlap_conflict, validate_route_update_request, validate_upstream_request,
    validate_upstream_update_request,
};
#[cfg(test)]
use {
    budget::validate_budget_config,
    validation::{validate_endpoints, validate_endpoints_ssrf},
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
    /// Whether upstream endpoints may use plaintext HTTP
    /// (`allow_http_upstream`); off unless the gear enables it.
    allow_http_upstream: bool,
    proxy_reader: Option<Arc<dyn ProxyConfigReader>>,
    /// Counts withheld ancestor credentials on the proxy path; none in tests
    /// that do not set it.
    metrics: Option<Arc<dyn OagwMetricsPort>>,
    /// The proxy's rate-limit buckets; a write that changes or removes a
    /// limit drops the buckets it made stale. None in tests that do not set
    /// it.
    rate_limiter: Option<Arc<RateLimiter>>,
    withheld_log: withheld::WithheldLog,
}

impl ControlPlaneServiceImpl {
    /// Create the control-plane service from its repositories and collaborators.
    /// With `proxy_reader`, proxy resolution without tags reads through it
    /// (ADR-0020); without, it reads the repositories.
    #[must_use]
    pub(crate) fn new(
        upstreams: Arc<dyn UpstreamRepository>,
        routes: Arc<dyn RouteRepository>,
        tenant_resolver: Arc<dyn TenantResolverClient>,
        policy_enforcer: PolicyEnforcer,
        credstore: Arc<dyn CredStoreClientV1>,
        ssrf_guard: Arc<SsrfGuard>,
        proxy_reader: Option<Arc<dyn ProxyConfigReader>>,
    ) -> Self {
        Self {
            upstreams,
            routes,
            tenant_resolver,
            policy_enforcer,
            credstore,
            ssrf_guard,
            allow_http_upstream: false,
            proxy_reader,
            metrics: None,
            rate_limiter: None,
            withheld_log: withheld::WithheldLog::default(),
        }
    }

    /// Accept plaintext HTTP upstream endpoints (`allow_http_upstream`).
    #[must_use]
    pub(crate) fn with_allow_http_upstream(mut self, allow: bool) -> Self {
        self.allow_http_upstream = allow;
        self
    }

    /// Drop the buckets of `rate_limiter` that a write makes stale.
    #[must_use]
    pub(crate) fn with_rate_limiter(mut self, rate_limiter: Arc<RateLimiter>) -> Self {
        self.rate_limiter = Some(rate_limiter);
        self
    }

    /// Drop the rate-limit buckets of a deleted upstream.
    fn drop_upstream_buckets(&self, id: Uuid) {
        if let Some(limiter) = &self.rate_limiter {
            limiter.remove_keys_for_upstream(id);
        }
    }

    /// Count withheld ancestor credentials (`credentials_withheld_total`).
    #[must_use]
    pub(crate) fn with_metrics(mut self, metrics: Arc<dyn OagwMetricsPort>) -> Self {
        self.metrics = Some(metrics);
        self
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
        self.authorize(ctx, Managed::Upstream, Action::Create)
            .await?;
        self.create_upstream_as(ctx, req, ManagedBy::Api)
            .await
            .map_err(|e| e.on_entity(DeniedOn::Upstream))
    }

    async fn get_upstream(&self, ctx: &SecurityContext, id: Uuid) -> Result<Upstream, DomainError> {
        self.authorize(ctx, Managed::Upstream, Action::Read(id))
            .await?;
        self.get_upstream_unchecked(ctx, id).await
    }

    async fn list_upstreams(
        &self,
        ctx: &SecurityContext,
        query: &ListQuery,
    ) -> Result<Vec<Upstream>, DomainError> {
        self.authorize(ctx, Managed::Upstream, Action::List).await?;
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
        self.authorize(ctx, Managed::Upstream, Action::Override(id))
            .await?;
        self.update_upstream_as(ctx, id, req, ManagedBy::Api)
            .await
            .map_err(|e| e.on_entity(DeniedOn::Upstream))
    }

    async fn delete_upstream(&self, ctx: &SecurityContext, id: Uuid) -> Result<(), DomainError> {
        self.authorize(ctx, Managed::Upstream, Action::Delete(id))
            .await?;
        self.delete_upstream_as(ctx, id, ManagedBy::Api)
            .await
            .map_err(|e| e.on_entity(DeniedOn::Upstream))
    }

    // -- Route CRUD --

    async fn create_route(
        &self,
        ctx: &SecurityContext,
        req: CreateRouteRequest,
    ) -> Result<Route, DomainError> {
        self.authorize(ctx, Managed::Route, Action::Create).await?;
        self.create_route_as(ctx, req, ManagedBy::Api, UpstreamCheck::Read)
            .await
            .map_err(|e| e.on_entity(DeniedOn::Route))
    }

    async fn get_route(&self, ctx: &SecurityContext, id: Uuid) -> Result<Route, DomainError> {
        self.authorize(ctx, Managed::Route, Action::Read(id))
            .await?;
        self.get_route_unchecked(ctx, id).await
    }

    async fn list_routes(
        &self,
        ctx: &SecurityContext,
        upstream_id: Option<Uuid>,
        query: &ListQuery,
    ) -> Result<Vec<Route>, DomainError> {
        self.authorize(ctx, Managed::Route, Action::List).await?;
        self.list_routes_unchecked(ctx, upstream_id, query).await
    }

    async fn update_route(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
        req: UpdateRouteRequest,
    ) -> Result<Route, DomainError> {
        self.authorize(ctx, Managed::Route, Action::Override(id))
            .await?;
        self.update_route_as(ctx, id, req, ManagedBy::Api, None)
            .await
            .map_err(|e| e.on_entity(DeniedOn::Route))
    }

    async fn delete_route(&self, ctx: &SecurityContext, id: Uuid) -> Result<(), DomainError> {
        self.authorize(ctx, Managed::Route, Action::Delete(id))
            .await?;
        self.delete_route_as(ctx, id, ManagedBy::Api)
            .await
            .map_err(|e| e.on_entity(DeniedOn::Route))
    }

    // -- Resolution --

    async fn resolve_proxy_target_for_client(
        &self,
        ctx: &SecurityContext,
        alias: &str,
        method: &str,
        path: &str,
    ) -> Result<(Upstream, Route), DomainError> {
        // Asked first, so a caller without `read` learns nothing about which
        // aliases and routes exist.
        self.authorize(ctx, Managed::Upstream, Action::List).await?;
        // Match on the path the proxy would match on.
        let path = crate::domain::proxy_path::canonical_path(path, path)?;
        // One chain for the merge and for the checks below, so the tenants
        // authorized are the ones whose settings were merged.
        let tenant_chain = self.build_tenant_chain(ctx).await?;
        let enforcer = &self.policy_enforcer;
        let resolved = self
            .resolve_alias_sourced(ctx, &tenant_chain, alias, Some((method, &path)), Tags::Load)
            .await;
        let (upstream, route, sources) = match resolved {
            Ok(ResolvedAlias {
                effective,
                route: Some(route),
                sources,
                ..
            }) => (effective, route, sources),
            Ok(ResolvedAlias { route: None, .. }) => {
                return Err(DomainError::Internal {
                    message: "resolve_alias returned None route for method+path request".into(),
                });
            }
            Err(e) => {
                // Resolution looks at every tenant of the chain, so its
                // errors (unknown alias, no matching route) tell about the
                // ancestors' upstreams too: report them only to a caller
                // with `read` on all of them.
                for &owner in &tenant_chain {
                    if owner != ctx.subject_tenant_id() {
                        authz::authorize_owned(
                            enforcer,
                            ctx,
                            Managed::Upstream,
                            Action::List,
                            owner,
                        )
                        .await?;
                    }
                }
                return Err(e);
            }
        };
        // The pair carries settings merged from the upstreams it is built
        // from (the selected one and the ancestors that share with it) and
        // the enforced plugins of the ancestor routes the matched one
        // shadows: the client needs `read` on each of them, and on the route
        // it gets back (the proxy hot path holds a grant and calls
        // `resolve_proxy_target`). An ancestor's upstream or route that adds
        // nothing to the answer is not asked about.
        for source in &sources.upstreams {
            authz::authorize_owned(
                enforcer,
                ctx,
                Managed::Upstream,
                Action::Read(source.id),
                source.tenant_id,
            )
            .await?;
        }
        authz::authorize_owned(
            enforcer,
            ctx,
            Managed::Route,
            Action::Read(route.id),
            route.tenant_id,
        )
        .await?;
        for shadowed in &sources.routes {
            authz::authorize_owned(
                enforcer,
                ctx,
                Managed::Route,
                Action::Read(shadowed.id),
                shadowed.tenant_id,
            )
            .await?;
        }
        Ok((upstream, route))
    }

    async fn resolve_proxy_target(
        &self,
        grant: &ProxyGrant,
        alias: &str,
        method: &str,
        path: &str,
        tags: Tags,
    ) -> Result<ProxyTarget, DomainError> {
        self.resolve_target(grant.ctx(), alias, method, path, tags)
            .await
    }
}

impl ControlPlaneServiceImpl {
    /// Resolve the proxy target for `ctx`, without a permission check: the
    /// callers ([`ControlPlaneService::resolve_proxy_target`] and
    /// [`ControlPlaneService::resolve_proxy_target_for_client`]) authorize.
    async fn resolve_target(
        &self,
        ctx: &SecurityContext,
        alias: &str,
        method: &str,
        path: &str,
        tags: Tags,
    ) -> Result<ProxyTarget, DomainError> {
        let tenant_chain = self.build_tenant_chain(ctx).await?;
        self.resolve_in_chain(ctx, &tenant_chain, alias, method, path, tags)
            .await
    }

    /// [`Self::resolve_target`] over a tenant chain the caller built.
    async fn resolve_in_chain(
        &self,
        ctx: &SecurityContext,
        tenant_chain: &[Uuid],
        alias: &str,
        method: &str,
        path: &str,
        tags: Tags,
    ) -> Result<ProxyTarget, DomainError> {
        let resolved = self
            .resolve_alias_sourced(ctx, tenant_chain, alias, Some((method, path)), tags)
            .await?;
        Ok(ProxyTarget {
            upstream: resolved.effective,
            route: resolved.route.ok_or_else(|| DomainError::Internal {
                message: "resolve_alias returned None route for method+path request".into(),
            })?,
            shadowed_route_limits: resolved.shadowed_route_limits,
        })
    }
}

// ===========================================================================
// Writes on behalf of an owner: the Management API or the registry reconcile
// ===========================================================================

/// The answer to a route naming an upstream the tenant does not have.
fn upstream_missing(upstream_id: Uuid) -> DomainError {
    DomainError::validation(format!(
        "upstream '{upstream_id}' not found for this tenant"
    ))
}

/// A route write the database rejected for its upstream (the composite
/// foreign key) answers as the upstream check does.
fn upstream_fk(e: RepositoryError, upstream_id: Uuid) -> DomainError {
    match e {
        RepositoryError::NotFound {
            entity: "upstream", ..
        } => upstream_missing(upstream_id),
        other => DomainError::from(other),
    }
}

/// A route write the repository rejected: an overlap answers with the
/// Management API's 409, a missing upstream as the upstream check does.
fn route_write_error(e: RepositoryError, route: &Route) -> DomainError {
    match e {
        RepositoryError::RouteOverlap(overlap) => route_overlap_conflict(route, overlap.method),
        other => upstream_fk(other, route.upstream_id),
    }
}

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
    /// Check a management permission for an API/SDK call.
    async fn authorize(
        &self,
        ctx: &SecurityContext,
        managed: Managed,
        action: Action,
    ) -> Result<(), DomainError> {
        authz::authorize(&self.policy_enforcer, ctx, managed, action).await
    }

    /// Read an upstream of the caller's tenant without a permission check
    /// (the registry reconcile and the checked [`ControlPlaneService`] reads).
    async fn get_upstream_unchecked(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
    ) -> Result<Upstream, DomainError> {
        self.upstreams
            .get_by_id(ctx.subject_tenant_id(), id)
            .await
            .map_err(DomainError::from)
    }

    /// Read a route of the caller's tenant without a permission check.
    async fn get_route_unchecked(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
    ) -> Result<Route, DomainError> {
        self.routes
            .get_by_id(ctx.subject_tenant_id(), id)
            .await
            .map_err(DomainError::from)
    }

    /// List routes of the caller's tenant without a permission check.
    async fn list_routes_unchecked(
        &self,
        ctx: &SecurityContext,
        upstream_id: Option<Uuid>,
        query: &ListQuery,
    ) -> Result<Vec<Route>, DomainError> {
        self.routes
            .list(ctx.subject_tenant_id(), upstream_id, query)
            .await
            .map_err(DomainError::from)
    }

    async fn create_upstream_as(
        &self,
        ctx: &SecurityContext,
        req: CreateUpstreamRequest,
        managed_by: ManagedBy,
    ) -> Result<Upstream, DomainError> {
        let alias = validate_upstream_request(&self.ssrf_guard, self.allow_http_upstream, &req)?;

        let tenant_id = ctx.subject_tenant_id();
        let id = req.id.unwrap_or_else(Uuid::new_v4);
        let tenant_chain = self.build_tenant_chain(ctx).await?;

        // Check if an ancestor tenant has an upstream with this alias.
        // If so, this is a "bind" operation requiring ancestor bind validation.
        let ancestors = ancestor_upstreams(&*self.upstreams, &tenant_chain, &alias).await?;
        validate_bind_constraints(
            ctx,
            &self.policy_enforcer,
            &ancestors,
            &BindOverrides {
                server: &req.server,
                auth: req.auth.as_ref(),
                rate_limit: req.rate_limit.as_ref(),
                plugins: req.plugins.as_ref(),
                cors: req.cors.as_ref(),
            },
        )
        .await?;

        // Verify the descendant's credential references are reachable
        // (fail-closed).
        if let Some(auth) = req.auth.as_ref() {
            for credential in auth.credential_refs() {
                validate_secret_ref_accessible(ctx, self.credstore.as_ref(), credential).await?;
            }
        }

        self.validate_budget_allocation(
            ctx,
            tenant_id,
            ancestors.first(),
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
        validate_shared_pool_cost(&ancestors, &upstream)?;

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

        validate_upstream_update_request(&self.ssrf_guard, self.allow_http_upstream, &req)?;

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

        // Full-replacement: always validate ancestor bind constraints and budget
        // allocation against the final state. Even None fields are meaningful —
        // setting rate_limit to None removes the allocation.
        let tenant_chain = self.build_tenant_chain(ctx).await?;
        let ancestors =
            ancestor_upstreams(&*self.upstreams, &tenant_chain, &existing.alias).await?;
        validate_bind_constraints(
            ctx,
            &self.policy_enforcer,
            &ancestors,
            &BindOverrides {
                server: &existing.server,
                auth: req.auth.as_ref(),
                rate_limit: req.rate_limit.as_ref(),
                plugins: req.plugins.as_ref(),
                cors: req.cors.as_ref(),
            },
        )
        .await?;

        // Verify the descendant's credential references are reachable
        // (fail-closed).
        if let Some(auth) = req.auth.as_ref() {
            for credential in auth.credential_refs() {
                validate_secret_ref_accessible(ctx, self.credstore.as_ref(), credential).await?;
            }
        }

        self.validate_budget_allocation(
            ctx,
            tenant_id,
            ancestors.first(),
            &existing.alias,
            req.rate_limit.as_ref(),
        )
        .await?;

        // If this upstream's budget may be tightened (lower total, lower
        // overcommit_ratio, mode changed to allocated, or a longer window the
        // total is counted in), verify that existing descendant allocations
        // still fit within the proposed budget.
        if let Some(ref new_rl) = req.rate_limit {
            let old_budget = existing
                .rate_limit
                .as_ref()
                .map(|rl| (rl.budget.as_ref(), rl.sustained.window));
            let new_budget = Some((new_rl.budget.as_ref(), new_rl.sustained.window));
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
        let previous_rate_limit = std::mem::replace(&mut existing.rate_limit, req.rate_limit);
        existing.cors = req.cors;
        existing.tags = canonical_tags(req.tags);
        existing.enabled = req.enabled;
        validate_shared_pool_cost(&ancestors, &existing)?;

        let current = self
            .upstreams
            .update(existing)
            .await
            .map_err(DomainError::from)?;
        // A changed limit leaves stale buckets: drop them on every write
        // path (API, SDK, registry).
        if previous_rate_limit != current.rate_limit
            && let Some(limiter) = &self.rate_limiter
        {
            limiter.remove_keys_for_upstream(id);
        }
        Ok(current)
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
        // The deleted routes' buckets go with the idle-bucket sweep.
        if let Some(limiter) = &self.rate_limiter {
            limiter.remove_keys_for_upstream(id);
        }
        self.routes
            .delete_by_upstream(tenant_id, id)
            .await
            .map_err(DomainError::from)
    }

    /// Check that `upstream_id` names an upstream of the caller's tenant
    /// that `writer` may attach routes to, unless the registry reconcile
    /// vouches for it ([`UpstreamCheck::KnownRegistry`]).
    async fn check_route_upstream(
        &self,
        tenant_id: Uuid,
        upstream_id: Uuid,
        writer: ManagedBy,
        check: UpstreamCheck,
    ) -> Result<(), DomainError> {
        if check == UpstreamCheck::KnownRegistry {
            return Ok(());
        }
        let upstream = self
            .upstreams
            .get_by_id(tenant_id, upstream_id)
            .await
            .map_err(|e| match e {
                RepositoryError::NotFound { .. } => upstream_missing(upstream_id),
                other => DomainError::from(other),
            })?;
        // A registry route on an API upstream would be deleted with it, and
        // every later reconcile would fail to re-create the route.
        if writer == ManagedBy::Registry && upstream.managed_by != ManagedBy::Registry {
            return Err(DomainError::validation(format!(
                "upstream '{upstream_id}' was created through the Management API; \
                 a types-registry route must name a types-registry upstream"
            )));
        }
        Ok(())
    }

    async fn create_route_as(
        &self,
        ctx: &SecurityContext,
        req: CreateRouteRequest,
        managed_by: ManagedBy,
        check: UpstreamCheck,
    ) -> Result<Route, DomainError> {
        let match_rules = validate_route_request(&req)?;

        let tenant_id = ctx.subject_tenant_id();
        let upstream_id = req.upstream_id;
        self.check_route_upstream(tenant_id, upstream_id, managed_by, check)
            .await?;

        let route = Route {
            id: req.id.unwrap_or_else(Uuid::new_v4),
            tenant_id,
            upstream_id,
            match_rules,
            plugins: req.plugins,
            rate_limit: req.rate_limit,
            cors: req.cors,
            tags: canonical_tags(req.tags),
            priority: req.priority,
            enabled: req.enabled,
            managed_by,
        };
        // On a database a route whose upstream was deleted after the check
        // above (or never read, for a known registry upstream) is rejected;
        // answer as the check would have.
        let written = route.clone();
        self.routes
            .create(route)
            .await
            .map_err(|e| route_write_error(e, &written))
    }

    /// Full replacement of a route. `move_to` is the registry reconcile's
    /// move to another upstream of the tenant (the Management API passes
    /// `None`: a route stays on its upstream); the target is checked like a
    /// create's, and the overlap check runs against its routes.
    async fn update_route_as(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
        req: UpdateRouteRequest,
        writer: ManagedBy,
        move_to: Option<(Uuid, UpstreamCheck)>,
    ) -> Result<Route, DomainError> {
        let tenant_id = ctx.subject_tenant_id();
        let mut existing = self
            .routes
            .get_by_id(tenant_id, id)
            .await
            .map_err(DomainError::from)?;
        ensure_owner("route", id, existing.managed_by, writer)?;

        let match_rules = validate_route_update_request(&req)?;

        if let Some((target, check)) = move_to
            && target != existing.upstream_id
        {
            self.check_route_upstream(tenant_id, target, writer, check)
                .await?;
            existing.upstream_id = target;
        }

        // Full replacement: directly assign all fields (None = unset).
        existing.match_rules = match_rules;
        existing.plugins = req.plugins;
        let previous_rate_limit = std::mem::replace(&mut existing.rate_limit, req.rate_limit);
        existing.cors = req.cors;
        existing.tags = canonical_tags(req.tags);
        existing.priority = req.priority;
        existing.enabled = req.enabled;

        let written = existing.clone();
        let current = self
            .routes
            .update(existing)
            .await
            .map_err(|e| route_write_error(e, &written))?;
        if previous_rate_limit != current.rate_limit
            && let Some(limiter) = &self.rate_limiter
        {
            limiter.remove_keys_for_route(id);
        }
        Ok(current)
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
            .map_err(DomainError::from)?;
        if let Some(limiter) = &self.rate_limiter {
            limiter.remove_keys_for_route(id);
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
