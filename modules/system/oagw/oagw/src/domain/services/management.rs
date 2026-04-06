use std::sync::Arc;

use super::ControlPlaneService;

use crate::domain::error::DomainError;
use crate::domain::model::{
    CreateRouteRequest, CreateUpstreamRequest, Endpoint, ListQuery, MatchRules, Route,
    UpdateRouteRequest, UpdateUpstreamRequest, Upstream,
};
use crate::domain::repo::{RouteRepository, UpstreamRepository};

use async_trait::async_trait;
use authz_resolver_sdk::PolicyEnforcer;
use authz_resolver_sdk::pep::{AccessRequest, ResourceType};
use credstore_sdk::CredStoreClientV1;
use modkit_macros::domain_model;
use modkit_security::SecurityContext;
use tenant_resolver_sdk::TenantResolverClient;
use uuid::Uuid;

/// Resource type for upstream binding permission checks.
const UPSTREAM_RESOURCE: ResourceType = ResourceType {
    name: "gts.x.core.oagw.upstream.v1~",
    supported_properties: &["owner_tenant_id"],
};

/// Permission action names for ancestor bind checks.
mod bind_actions {
    pub const BIND: &str = "bind";
    pub const OVERRIDE_AUTH: &str = "override_auth";
    pub const OVERRIDE_RATE: &str = "override_rate";
    pub const ADD_PLUGINS: &str = "add_plugins";
}

/// Control Plane service implementation backed by in-memory repositories.
#[domain_model]
pub(crate) struct ControlPlaneServiceImpl {
    upstreams: Arc<dyn UpstreamRepository>,
    routes: Arc<dyn RouteRepository>,
    tenant_resolver: Arc<dyn TenantResolverClient>,
    policy_enforcer: PolicyEnforcer,
    credstore: Arc<dyn CredStoreClientV1>,
}

impl ControlPlaneServiceImpl {
    #[must_use]
    pub(crate) fn new(
        upstreams: Arc<dyn UpstreamRepository>,
        routes: Arc<dyn RouteRepository>,
        tenant_resolver: Arc<dyn TenantResolverClient>,
        policy_enforcer: PolicyEnforcer,
        credstore: Arc<dyn CredStoreClientV1>,
    ) -> Self {
        Self {
            upstreams,
            routes,
            tenant_resolver,
            policy_enforcer,
            credstore,
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
        if let Some(ref cors) = req.cors {
            crate::domain::cors::validate_cors_config(cors)?;
        }

        // Enforce alias derivation / explicit rules.
        let alias = enforce_alias_create(req.alias.as_deref(), &req.server.endpoints)?;

        let tenant_id = ctx.subject_tenant_id();
        let id = Uuid::new_v4();
        let tenant_chain = self.build_tenant_chain(ctx).await?;

        // Check if an ancestor tenant has an upstream with this alias.
        // If so, this is a "bind" operation requiring ancestor bind validation.
        self.validate_ancestor_bind(
            ctx,
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
        existing.server = req.server;
        existing.protocol = req.protocol;

        // Enforce alias re-evaluation when endpoints change.
        let endpoints_changed = existing.server.endpoints != old_endpoints;
        if endpoints_changed {
            let alias = enforce_alias_update(
                req.alias.as_deref(),
                &existing.server.endpoints,
                &existing.alias,
                &old_endpoints,
            )?;
            existing.alias = alias;
        } else if let Some(ref user_alias) = req.alias {
            let normalized = normalize_alias(user_alias);
            // No endpoint change — allow alias update only for IP-based endpoints,
            // or when the provided alias exactly matches the derived value (no-op).
            if let Some(derived) = compute_derived_alias(&existing.server.endpoints)
                && normalized != derived
            {
                return Err(DomainError::validation(
                    "alias cannot be overridden for hostname-based endpoints",
                ));
            }
            validate_alias(&normalized)?;
            existing.alias = normalized;
        }

        // Validate ancestor bind constraints if the resulting alias matches
        // an ancestor upstream.
        let has_overrides = req.auth.is_some()
            || req.rate_limit.is_some()
            || req.plugins.is_some()
            || req.cors.is_some();

        if has_overrides || endpoints_changed || req.alias.is_some() {
            let tenant_chain = self.build_tenant_chain(ctx).await?;
            self.validate_ancestor_bind(
                ctx,
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
        }

        // Full replacement: directly assign all fields (None = unset).
        existing.auth = req.auth;
        existing.headers = req.headers;
        existing.plugins = req.plugins;
        existing.rate_limit = req.rate_limit;
        if let Some(ref cors) = req.cors {
            crate::domain::cors::validate_cors_config(cors)?;
        }
        existing.cors = req.cors;
        existing.tags = req.tags;
        existing.enabled = req.enabled;

        self.upstreams
            .update(existing)
            .await
            .map_err(DomainError::from)
    }

    async fn delete_upstream(&self, ctx: &SecurityContext, id: Uuid) -> Result<(), DomainError> {
        let tenant_id = ctx.subject_tenant_id();
        // Cascade delete routes before removing the upstream.
        self.routes
            .delete_by_upstream(tenant_id, id)
            .await
            .map_err(DomainError::from)?;
        self.upstreams
            .delete(tenant_id, id)
            .await
            .map_err(|_| DomainError::not_found("upstream", id))
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
            id: Uuid::new_v4(),
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
        self.check_route_overlap(&route, None).await?;

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
        self.check_route_overlap(&existing, Some(existing.id))
            .await?;

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

// ===========================================================================
// Private helpers on ControlPlaneServiceImpl
// ===========================================================================

impl ControlPlaneServiceImpl {
    /// Check that no existing **enabled** route under the same upstream shares
    /// `(path_prefix, priority, method)` with the candidate route.
    ///
    /// `exclude_id` is `Some(route.id)` on update to skip the route being
    /// modified (it will be compared against its new state, not itself).
    ///
    /// Returns `DomainError::Conflict` on violation (maps to 409).
    async fn check_route_overlap(
        &self,
        candidate: &Route,
        exclude_id: Option<Uuid>,
    ) -> Result<(), DomainError> {
        // Disabled routes cannot cause match-time ambiguity.
        if !candidate.enabled {
            return Ok(());
        }

        let candidate_http = match &candidate.match_rules.http {
            Some(h) => h,
            None => return Ok(()), // No HTTP match rules → no overlap to check.
        };

        // Fetch all routes for this (tenant, upstream).
        let all = self
            .routes
            .list(
                candidate.tenant_id,
                Some(candidate.upstream_id),
                &ListQuery {
                    top: u32::MAX,
                    skip: 0,
                },
            )
            .await
            .map_err(DomainError::from)?;

        for existing in &all {
            // Skip self on update.
            if Some(existing.id) == exclude_id {
                continue;
            }
            // Only enabled routes can conflict.
            if !existing.enabled {
                continue;
            }
            // Must have HTTP match rules.
            let Some(existing_http) = &existing.match_rules.http else {
                continue;
            };
            // Must share path and priority.
            if existing_http.path != candidate_http.path || existing.priority != candidate.priority
            {
                continue;
            }
            // Check for any overlapping method.
            for m in &candidate_http.methods {
                if existing_http.methods.contains(m) {
                    return Err(DomainError::conflict(format!(
                        "route overlap: an enabled route already exists on upstream '{}' \
                         with path '{}', priority {}, method {:?}",
                        candidate.upstream_id, candidate_http.path, candidate.priority, m
                    )));
                }
            }
        }

        Ok(())
    }

    /// Validate bind constraints against the **closest** ancestor with a matching
    /// alias. Delegates to [`validate_bind_constraints`] for policy permissions,
    /// sharing mode enforcement, and `secret_ref` accessibility.
    ///
    /// Only the closest ancestor is checked — not the entire chain. This is
    /// intentional: permission to bind is granted by the immediate owner of
    /// the alias. Grandparent `enforce` constraints still propagate at runtime
    /// through [`compute_effective_config`], which merges the full chain
    /// root → descendant.
    ///
    /// No-op if no ancestor has the alias (fresh upstream, no bind needed).
    async fn validate_ancestor_bind(
        &self,
        ctx: &SecurityContext,
        tenant_chain: &[Uuid],
        alias: &str,
        overrides: &BindOverrides<'_>,
    ) -> Result<(), DomainError> {
        for &ancestor_tid in &tenant_chain[1..] {
            if let Ok(ancestor_upstream) = self.upstreams.get_by_alias(ancestor_tid, alias).await {
                validate_bind_constraints(
                    &self.policy_enforcer,
                    self.credstore.as_ref(),
                    ctx,
                    &ancestor_upstream,
                    overrides,
                )
                .await?;
                break; // Only check closest ancestor with matching alias.
            }
        }
        Ok(())
    }

    /// Build the ordered tenant chain `[self, parent, ..., root]`.
    ///
    /// Index 0 is always the requesting tenant. Callers that only need
    /// ancestors (e.g. permission checks) can skip `&chain[1..]`.
    pub(crate) async fn build_tenant_chain(
        &self,
        ctx: &SecurityContext,
    ) -> Result<Vec<Uuid>, DomainError> {
        let tenant_id = ctx.subject_tenant_id();
        let ancestors_resp = self
            .tenant_resolver
            .get_ancestors(
                ctx,
                tenant_resolver_sdk::TenantId(tenant_id),
                &tenant_resolver_sdk::GetAncestorsOptions::default(),
            )
            .await?;

        let mut chain = Vec::with_capacity(1 + ancestors_resp.ancestors.len());
        chain.push(tenant_id);
        for ancestor in &ancestors_resp.ancestors {
            chain.push(ancestor.id.0);
        }
        Ok(chain)
    }

    /// Alias resolution: find the winning upstream by alias across the tenant
    /// chain, collect the merge chain, optionally resolve a route, and return
    /// the effective config.
    ///
    /// Performs a **single walk** over the tenant chain, collecting all visible
    /// upstreams in one pass. The winning (closest enabled) upstream is selected
    /// and ancestors above it form the merge chain — no second pass needed.
    ///
    /// When `method_path` is `Some((method, path))`, a route is also resolved
    /// across the tenant chain (searching by each ancestor upstream ID) and
    /// folded into the effective config via `compute_effective_config`.
    pub(crate) async fn resolve_alias(
        &self,
        ctx: &SecurityContext,
        tenant_chain: &[Uuid],
        alias: &str,
        method_path: Option<(&str, &str)>,
    ) -> Result<(Upstream, Option<Route>), DomainError> {
        let tenant_id = ctx.subject_tenant_id();
        // Normalize the incoming alias for case-insensitive matching.
        let alias = normalize_alias(alias);

        // Single walk: collect all visible upstreams keyed by chain index.
        let mut found: Vec<(usize, Upstream)> = Vec::new();
        let mut disabled_alias: Option<String> = None;

        for (i, &tid) in tenant_chain.iter().enumerate() {
            match self.upstreams.get_by_alias(tid, &alias).await {
                Ok(upstream) => {
                    if tid != tenant_id && !is_visible_to_descendant(&upstream) {
                        continue;
                    }
                    if !upstream.enabled {
                        if disabled_alias.is_none() {
                            disabled_alias = Some(upstream.alias.clone());
                        }
                        continue;
                    }
                    found.push((i, upstream));
                }
                Err(_) => continue,
            }
        }

        // The winning upstream is the closest (lowest index) enabled match.
        let (_, selected_upstream) = match found.first() {
            Some(pair) => pair.clone(),
            None => {
                if let Some(alias) = disabled_alias {
                    return Err(DomainError::upstream_disabled(alias));
                }
                return Err(DomainError::not_found("upstream", Uuid::nil()));
            }
        };

        // Ancestors above the selected one form the merge chain (already collected).
        let merge_chain: Vec<&Upstream> = found[1..].iter().map(|(_, u)| u).collect();

        // Resolve route if method+path provided.
        // Search by each upstream ID in the chain — routes may be attached to
        // the selected upstream or any ancestor upstream.
        let route = if let Some((method, path)) = method_path {
            let mut route_found: Option<Route> = None;

            // Try selected upstream's ID first (most specific).
            if let Ok(r) = Self::find_route_in_chain(
                &*self.routes,
                tenant_chain,
                selected_upstream.id,
                method,
                path,
            )
            .await
            {
                route_found = Some(r);
            }

            // Fall back to ancestor upstream IDs (closest ancestor first).
            if route_found.is_none() {
                for ancestor in &merge_chain {
                    if let Ok(r) = Self::find_route_in_chain(
                        &*self.routes,
                        tenant_chain,
                        ancestor.id,
                        method,
                        path,
                    )
                    .await
                    {
                        route_found = Some(r);
                        break;
                    }
                }
            }

            Some(route_found.ok_or_else(|| DomainError::not_found("route", Uuid::nil()))?)
        } else {
            None
        };

        // Build effective config.
        if merge_chain.is_empty() {
            // Single upstream → apply route overrides directly if present.
            if let Some(ref route) = route {
                let effective = compute_effective_config(
                    std::slice::from_ref(&selected_upstream),
                    Some(route),
                )?;
                return Ok((effective, Some(route.clone())));
            }
            return Ok((selected_upstream, None));
        }

        // Root-first order for merge: reverse ancestors, append selected.
        let mut merge_vec: Vec<Upstream> = merge_chain.into_iter().rev().cloned().collect();
        merge_vec.push(selected_upstream);

        let effective = compute_effective_config(&merge_vec, route.as_ref())?;
        Ok((effective, route))
    }

    /// Find a matching route for `upstream_id` by searching across tenant scopes.
    pub(crate) async fn find_route_in_chain(
        routes: &dyn RouteRepository,
        tenant_chain: &[Uuid],
        upstream_id: Uuid,
        method: &str,
        path: &str,
    ) -> Result<Route, DomainError> {
        for &tid in tenant_chain {
            if let Ok(route) = routes.find_matching(tid, upstream_id, method, path).await {
                return Ok(route);
            }
        }
        Err(DomainError::not_found("route", Uuid::nil()))
    }
}

// ===========================================================================
// Free functions — validation, permissions, visibility, config merge, alias
// ===========================================================================

/// Ensure exactly one of `http` or `grpc` is present in the match rules.
///
/// Rejects routes where both fields are `None` (matches nothing) or both
/// are `Some` (ambiguous protocol).
fn validate_match_rules(rules: &MatchRules) -> Result<(), DomainError> {
    match (&rules.http, &rules.grpc) {
        (None, None) => Err(DomainError::validation(
            "match rules must specify exactly one of 'http' or 'grpc'",
        )),
        (Some(_), Some(_)) => Err(DomainError::validation(
            "match rules must specify exactly one of 'http' or 'grpc', not both",
        )),
        _ => Ok(()),
    }
}

/// Validate the endpoint list for a server configuration.
///
/// Rules:
/// - At least one endpoint is required.
/// - All endpoints must use either IP addresses or hostnames — no mixing.
/// - All endpoints must share the same scheme (upstream-level invariant).
fn validate_endpoints(endpoints: &[Endpoint]) -> Result<(), DomainError> {
    if endpoints.is_empty() {
        return Err(DomainError::validation(
            "server must have at least one endpoint",
        ));
    }

    // TODO(hardening): add configurable SSRF deny-list for private IPv4 ranges
    // (loopback, RFC 1918, link-local, 169.254.169.254 metadata). Should be
    // opt-in (many deployments legitimately proxy to internal services) and also
    // enforced at DNS resolution time in DnsDiscovery::resolve() to cover
    // hostnames that resolve to private IPs.

    // IPv6 endpoints are not yet supported — reject early with a clear message.
    // Enabling IPv6 requires SSRF protections (deny-lists for link-local, private
    // ranges, IPv4-mapped addresses).
    for (i, ep) in endpoints.iter().enumerate() {
        if ep.normalized_host().parse::<std::net::Ipv6Addr>().is_ok() {
            return Err(DomainError::validation(format!(
                "endpoint[{i}] uses IPv6 address '{}'; IPv6 endpoints are not yet supported",
                ep.host
            )));
        }
    }

    // Check all-IP vs all-hostname consistency.
    let ip_count = endpoints.iter().filter(|ep| ep.is_ip()).count();
    if ip_count != 0 && ip_count != endpoints.len() {
        return Err(DomainError::validation(
            "all endpoints must use either IP addresses or hostnames; mixed configurations are not allowed",
        ));
    }

    // Validate hostname format (RFC 1123) for non-IP endpoints.
    if ip_count == 0 {
        for (i, ep) in endpoints.iter().enumerate() {
            validate_hostname(i, &ep.host)?;
        }
    }

    // Enforce identical scheme and port across the pool.
    if endpoints.len() > 1 {
        let first_scheme = &endpoints[0].scheme;
        let first_port = endpoints[0].port;
        for (i, ep) in endpoints.iter().enumerate().skip(1) {
            if ep.scheme != *first_scheme {
                return Err(DomainError::validation(format!(
                    "endpoint[{i}] scheme {:?} differs from endpoint[0] scheme {:?}; all endpoints must share the same scheme",
                    ep.scheme, first_scheme
                )));
            }
            if ep.port != first_port {
                return Err(DomainError::validation(format!(
                    "endpoint[{i}] port {} differs from endpoint[0] port {}; all endpoints must share the same port",
                    ep.port, first_port
                )));
            }
        }
    }

    Ok(())
}

/// Validate a hostname per RFC 1123: max 253 chars total, each label 1–63 chars,
/// labels contain only ASCII alphanumeric + hyphen, labels don't start/end with
/// hyphen. A trailing dot (FQDN) is tolerated and stripped before validation.
fn validate_hostname(index: usize, host: &str) -> Result<(), DomainError> {
    let h = host.strip_suffix('.').unwrap_or(host);
    if h.is_empty() {
        return Err(DomainError::validation(format!(
            "endpoint[{index}] host is empty"
        )));
    }
    if h.len() > 253 {
        return Err(DomainError::validation(format!(
            "endpoint[{index}] host '{}' exceeds 253 characters",
            host
        )));
    }
    for label in h.split('.') {
        if label.is_empty() {
            return Err(DomainError::validation(format!(
                "endpoint[{index}] host '{host}' contains an empty label"
            )));
        }
        if label.len() > 63 {
            return Err(DomainError::validation(format!(
                "endpoint[{index}] host '{host}' label '{label}' exceeds 63 characters"
            )));
        }
        if !label
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        {
            return Err(DomainError::validation(format!(
                "endpoint[{index}] host '{host}' label '{label}' contains invalid characters; \
                 only ASCII alphanumeric and '-' are allowed"
            )));
        }
        if label.starts_with('-') || label.ends_with('-') {
            return Err(DomainError::validation(format!(
                "endpoint[{index}] host '{host}' label '{label}' must not start or end with '-'"
            )));
        }
    }
    Ok(())
}

/// Maximum length for an upstream alias.
const MAX_ALIAS_LENGTH: usize = 253;

/// Validate an alias: non-empty, max length, safe charset (alphanumeric + `.:-_`),
/// must contain at least one alphanumeric character, and must not be a dot-segment.
fn validate_alias(alias: &str) -> Result<(), DomainError> {
    if alias.is_empty() {
        return Err(DomainError::validation("alias must not be empty"));
    }
    if alias.len() > MAX_ALIAS_LENGTH {
        return Err(DomainError::validation(format!(
            "alias must not exceed {MAX_ALIAS_LENGTH} characters"
        )));
    }
    if !alias
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | ':' | '-' | '_'))
    {
        return Err(DomainError::validation(
            "alias contains invalid characters; only alphanumeric, '.', ':', '-', '_' are allowed",
        ));
    }
    // Reject dot-segments and punctuation-only aliases to prevent path traversal
    // and ambiguous URL segments in /proxy/{alias}/{path}.
    if alias == "." || alias == ".." {
        return Err(DomainError::validation(
            "alias must not be a dot-segment ('.' or '..')",
        ));
    }
    if !alias.chars().any(|c| c.is_ascii_alphanumeric()) {
        return Err(DomainError::validation(
            "alias must contain at least one alphanumeric character",
        ));
    }
    Ok(())
}

/// Normalize an alias to lowercase. Hostname trailing dots are already
/// handled by `Endpoint::normalized_host()` during derivation; this covers
/// user-provided explicit aliases. All trailing dots are stripped.
fn normalize_alias(alias: &str) -> String {
    alias.to_ascii_lowercase().trim_end_matches('.').to_string()
}

/// Check whether the given endpoints are all IP addresses.
fn endpoints_are_ip(endpoints: &[Endpoint]) -> bool {
    !endpoints.is_empty() && endpoints.iter().all(Endpoint::is_ip)
}

/// Attempt to derive an alias from the endpoint list.
///
/// Returns `Some(alias)` when derivation succeeds (hostname-based), or
/// `None` when an explicit alias is required (IP-based or no common suffix).
///
/// Derivation rules:
/// - Single host, standard port → hostname
/// - Single host, non-standard port → hostname:port
/// - Multiple hosts, all identical → treated as single-host
/// - Multiple hosts, common domain suffix (≥2 labels) → common suffix;
///   non-standard port is appended (e.g., `vendor.com:8443`) to avoid
///   collisions between pools on different ports
/// - Multiple hosts, no common suffix → `None`
/// - IP addresses → `None`
fn compute_derived_alias(endpoints: &[Endpoint]) -> Option<String> {
    if endpoints.is_empty() || endpoints_are_ip(endpoints) {
        return None;
    }

    // Collect unique normalized host contributions.
    let contributions: Vec<String> = endpoints.iter().map(|e| e.alias_contribution()).collect();

    // De-duplicate: if all identical, treat as single-endpoint.
    let unique: Vec<&str> = {
        let mut v: Vec<&str> = contributions.iter().map(String::as_str).collect();
        v.sort_unstable();
        v.dedup();
        v
    };

    if unique.len() == 1 {
        return Some(unique[0].to_string());
    }

    // Multi-host: extract pure hostnames for common suffix computation.
    let hosts: Vec<String> = endpoints.iter().map(|e| e.normalized_host()).collect();
    let suffix = common_domain_suffix(&hosts)?;

    // Append :port when the pool uses a non-standard port so that
    // pools with the same domain suffix but different ports get
    // distinct aliases (e.g., `vendor.com` vs `vendor.com:8443`).
    // validate_endpoints guarantees all endpoints share the same port.
    if endpoints[0].is_standard_port() {
        Some(suffix)
    } else {
        Some(format!("{suffix}:{}", endpoints[0].port))
    }
}

/// Extract the longest common domain suffix from a set of hostnames.
///
/// Returns `Some(suffix)` if the common suffix has ≥2 labels, `None` otherwise.
/// Example: `["us.vendor.com", "eu.vendor.com"]` → `Some("vendor.com")`.
fn common_domain_suffix(hosts: &[String]) -> Option<String> {
    if hosts.is_empty() {
        return None;
    }

    // Split each host into labels, reversed (rightmost first).
    let reversed: Vec<Vec<&str>> = hosts
        .iter()
        .map(|h| h.split('.').rev().collect::<Vec<_>>())
        .collect();

    // Find the longest common prefix of the reversed labels.
    let min_len = reversed.iter().map(|r| r.len()).min().unwrap_or(0);
    let mut common_count = 0;
    for i in 0..min_len {
        let label = reversed[0][i];
        if reversed.iter().all(|r| r[i] == label) {
            common_count += 1;
        } else {
            break;
        }
    }

    // Minimum 2 common labels (e.g. `vendor.com`, not just `com`).
    if common_count < 2 {
        return None;
    }

    // Reconstruct the suffix in correct order.
    let suffix: Vec<&str> = reversed[0][..common_count].iter().rev().copied().collect();
    let candidate = suffix.join(".");

    // Reject public suffixes (e.g. "co.uk", "com.au") that are not registrable
    // domains. A registrable domain has at least one label beyond the public
    // suffix (e.g. "vendor.co.uk" is registrable, "co.uk" is not).
    if psl::domain(candidate.as_bytes()).is_none() {
        tracing::debug!(suffix = %candidate, "common suffix is a public suffix (not a registrable domain), alias must be explicit");
        return None;
    }

    Some(candidate)
}

/// Enforce alias rules on upstream **creation**.
///
/// - Hostname-derivable endpoints: alias is auto-derived; user-provided alias
///   is rejected with `400 Validation`.
/// - IP or non-derivable endpoints: explicit alias is required.
fn enforce_alias_create(
    user_alias: Option<&str>,
    endpoints: &[Endpoint],
) -> Result<String, DomainError> {
    match compute_derived_alias(endpoints) {
        Some(derived) => {
            if let Some(user) = user_alias {
                // Reject user-provided alias when derivation is possible.
                let normalized_user = normalize_alias(user);
                if normalized_user != derived {
                    return Err(DomainError::validation(format!(
                        "alias is auto-derived for hostname-based endpoints; \
                         remove the 'alias' field (derived: '{derived}')"
                    )));
                }
                // User provided the exact derived value — tolerate silently.
            }
            validate_alias(&derived)?;
            Ok(derived)
        }
        None => {
            // Explicit alias required.
            let alias = user_alias.ok_or_else(|| {
                DomainError::validation(
                    "explicit alias is required for IP-based or heterogeneous-host endpoints",
                )
            })?;
            let normalized = normalize_alias(alias);
            validate_alias(&normalized)?;
            Ok(normalized)
        }
    }
}

/// Enforce alias rules on upstream **update** when endpoints change.
///
/// Re-evaluates alias enforcement against the (possibly new) endpoints:
/// - hostname→hostname: alias recomputed from new hosts.
/// - IP→IP: existing alias retained unless user provides a new one.
/// - hostname→IP: **rejected** unless user provides a new explicit alias.
/// - IP→hostname: alias recomputed (old explicit alias replaced).
fn enforce_alias_update(
    user_alias: Option<&str>,
    new_endpoints: &[Endpoint],
    existing_alias: &str,
    old_endpoints: &[Endpoint],
) -> Result<String, DomainError> {
    let old_derivable = compute_derived_alias(old_endpoints).is_some();
    let new_derived = compute_derived_alias(new_endpoints);

    match (old_derivable, &new_derived) {
        // New endpoints are hostname-derivable: recompute alias.
        // Covers hostname→hostname (recompute) and IP→hostname (old explicit alias replaced).
        (_, Some(derived)) => {
            if let Some(user) = user_alias {
                let normalized_user = normalize_alias(user);
                if normalized_user != *derived {
                    return Err(DomainError::validation(format!(
                        "alias is auto-derived for hostname-based endpoints; \
                         remove the 'alias' field (derived: '{derived}')"
                    )));
                }
            }
            validate_alias(derived)?;
            Ok(derived.clone())
        }
        // derivable → non-derivable: must provide explicit alias.
        (true, None) => {
            let alias = user_alias.ok_or_else(|| {
                DomainError::validation(
                    "explicit alias is required for IP-based or heterogeneous-host endpoints",
                )
            })?;
            let normalized = normalize_alias(alias);
            validate_alias(&normalized)?;
            Ok(normalized)
        }
        // IP → IP: keep existing unless user provides a new one.
        (false, None) => {
            if let Some(user) = user_alias {
                let normalized = normalize_alias(user);
                validate_alias(&normalized)?;
                Ok(normalized)
            } else {
                Ok(existing_alias.to_string())
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Ancestor bind validation
// ---------------------------------------------------------------------------

/// Describes the override fields a descendant is attempting to set.
/// Used by `validate_bind_constraints` so both create and update can share
/// the same validation logic.
#[allow(unknown_lints, de0309_must_have_domain_model)] // short-lived param container, not a domain entity
struct BindOverrides<'a> {
    auth: Option<&'a crate::domain::model::AuthConfig>,
    rate_limit: Option<&'a crate::domain::model::RateLimitConfig>,
    plugins: Option<&'a crate::domain::model::PluginsConfig>,
    cors: Option<&'a crate::domain::model::CorsConfig>,
}

/// Validate bind constraints when a descendant creates or updates an
/// upstream whose alias matches an ancestor's upstream.
///
/// Per `cpt-cf-oagw-algo-tenant-permission-check`:
/// - `oagw:upstream:bind` — required for any bind to ancestor upstream
/// - `oagw:upstream:override_auth` — required if descendant provides auth config
/// - `oagw:upstream:override_rate` — required if descendant provides rate_limit config
/// - `oagw:upstream:add_plugins` — required if descendant provides plugins config
///
/// Also validates sharing modes:
/// - `enforce` fields cannot be overridden (400 Validation)
/// - `private` fields are not visible (400 Validation)
async fn validate_bind_constraints(
    enforcer: &PolicyEnforcer,
    credstore: &dyn CredStoreClientV1,
    ctx: &SecurityContext,
    ancestor: &Upstream,
    overrides: &BindOverrides<'_>,
) -> Result<(), DomainError> {
    use crate::domain::model::SharingMode;

    // 1. Check bind permission.
    let access_req = AccessRequest::new()
        .resource_property("owner_tenant_id", ancestor.tenant_id)
        .require_constraints(false);
    enforcer
        .access_scope_with(
            ctx,
            &UPSTREAM_RESOURCE,
            bind_actions::BIND,
            Some(ancestor.id),
            &access_req,
        )
        .await?;

    // 2. Check per-field override permissions and sharing mode constraints.

    // Auth override
    if let Some(auth_override) = overrides.auth {
        match ancestor.auth.as_ref().map(|a| a.sharing) {
            Some(SharingMode::Enforce) => {
                return Err(DomainError::validation(
                    "cannot override auth: ancestor upstream has sharing mode 'enforce'",
                ));
            }
            Some(SharingMode::Private) => {
                return Err(DomainError::validation(
                    "cannot override auth: ancestor upstream field is private",
                ));
            }
            _ => {
                enforcer
                    .access_scope_with(
                        ctx,
                        &UPSTREAM_RESOURCE,
                        bind_actions::OVERRIDE_AUTH,
                        Some(ancestor.id),
                        &access_req,
                    )
                    .await?;

                // Validate secret_ref accessibility for the descendant tenant.
                if let Some(ref config) = auth_override.config
                    && let Some(raw_ref) = config.get("secret_ref")
                {
                    validate_secret_ref_accessible(credstore, ctx, raw_ref).await?;
                }
            }
        }
    }

    // Rate limit override
    if overrides.rate_limit.is_some() {
        match ancestor.rate_limit.as_ref().map(|r| r.sharing) {
            Some(SharingMode::Enforce) => {
                return Err(DomainError::validation(
                    "cannot override rate_limit: ancestor upstream has sharing mode 'enforce'",
                ));
            }
            Some(SharingMode::Private) => {
                return Err(DomainError::validation(
                    "cannot override rate_limit: ancestor upstream field is private",
                ));
            }
            _ => {
                enforcer
                    .access_scope_with(
                        ctx,
                        &UPSTREAM_RESOURCE,
                        bind_actions::OVERRIDE_RATE,
                        Some(ancestor.id),
                        &access_req,
                    )
                    .await?;
            }
        }
    }

    // Plugins override
    if overrides.plugins.is_some() {
        match ancestor.plugins.as_ref().map(|p| p.sharing) {
            Some(SharingMode::Enforce) => {
                return Err(DomainError::validation(
                    "cannot override plugins: ancestor upstream has sharing mode 'enforce'",
                ));
            }
            Some(SharingMode::Private) => {
                return Err(DomainError::validation(
                    "cannot override plugins: ancestor upstream field is private",
                ));
            }
            _ => {
                enforcer
                    .access_scope_with(
                        ctx,
                        &UPSTREAM_RESOURCE,
                        bind_actions::ADD_PLUGINS,
                        Some(ancestor.id),
                        &access_req,
                    )
                    .await?;
            }
        }
    }

    // CORS sharing mode constraints (no override permission required).
    if overrides.cors.is_some() {
        match ancestor.cors.as_ref().map(|c| c.sharing) {
            Some(SharingMode::Enforce) => {
                return Err(DomainError::validation(
                    "cannot override cors: ancestor upstream has sharing mode 'enforce'",
                ));
            }
            Some(SharingMode::Private) => {
                return Err(DomainError::validation(
                    "cannot override cors: ancestor upstream field is private",
                ));
            }
            _ => {}
        }
    }

    Ok(())
}

/// Validate that a `secret_ref` is accessible to the requesting tenant via
/// `cred_store`. Per `cpt-cf-oagw-principle-cred-isolation`, if the secret
/// is not accessible, the request is rejected (fail-closed).
async fn validate_secret_ref_accessible(
    credstore: &dyn CredStoreClientV1,
    ctx: &SecurityContext,
    raw_ref: &str,
) -> Result<(), DomainError> {
    let bare = raw_ref.strip_prefix("cred://").unwrap_or(raw_ref);
    let key = credstore_sdk::SecretRef::new(bare)
        .map_err(|e| DomainError::validation(format!("invalid secret_ref '{raw_ref}': {e}")))?;

    match credstore.get(ctx, &key).await {
        Ok(Some(_)) => Ok(()),
        Ok(None) => Err(DomainError::validation(format!(
            "secret_ref '{raw_ref}' is not accessible to this tenant"
        ))),
        Err(credstore_sdk::CredStoreError::Internal(msg)) => {
            // Fail-closed: cred_store unavailability → reject.
            tracing::warn!(secret_ref = raw_ref, error = %msg, "cred_store unavailable during secret_ref validation");
            Err(DomainError::Internal {
                message: format!("credential validation unavailable: {msg}"),
            })
        }
        Err(e) => Err(DomainError::validation(format!(
            "secret_ref '{raw_ref}' validation failed: {e}"
        ))),
    }
}

// ---------------------------------------------------------------------------
// Visibility and effective configuration merge
// ---------------------------------------------------------------------------

/// Check whether an upstream is visible to descendant tenants.
///
/// Per `cpt-cf-oagw-algo-tenant-alias-shadow` step 2b, an ancestor upstream is
/// visible if its own tenant matches the requester OR any per-field sharing flag
/// (`auth`, `rate_limit`, `plugins`, `cors`) is not `private`.
///
/// Returns `false` when all shareable fields are `None` — this is intentional.
/// An upstream with no auth, rate_limit, plugins, or cors has no configuration
/// to share with descendants, so it is treated as invisible. Fields without a
/// sharing mode (e.g. `headers`) do not contribute to visibility.
fn is_visible_to_descendant(upstream: &Upstream) -> bool {
    use crate::domain::model::SharingMode;

    let auth_shared = upstream
        .auth
        .as_ref()
        .is_some_and(|a| a.sharing != SharingMode::Private);
    let rate_shared = upstream
        .rate_limit
        .as_ref()
        .is_some_and(|r| r.sharing != SharingMode::Private);
    let plugins_shared = upstream
        .plugins
        .as_ref()
        .is_some_and(|p| p.sharing != SharingMode::Private);
    let cors_shared = upstream
        .cors
        .as_ref()
        .is_some_and(|c| c.sharing != SharingMode::Private);

    auth_shared || rate_shared || plugins_shared || cors_shared
}

/// Compute the effective upstream configuration by merging ancestor upstreams
/// in the tenant chain (root → descendant).
///
/// Per `cpt-cf-oagw-algo-tenant-config-merge`:
/// - Auth:       `private` → local-only (blocked by ancestor `enforce`); `inherit` → override; `enforce` → sticky
/// - Rate limit: `private` → local-only (constrained by ancestor `enforce` via `min()`); else `min(ancestor, descendant)`
/// - Plugins:    `private` → local-only (ancestor `enforce` items preserved); else concatenate `ancestor + descendant`
/// - Tags:       union (add-only)
///
/// `ancestor_chain` is ordered root-first: `[root, parent, ..., selected]`.
/// The last element is the selected (resolved) upstream.
pub(crate) fn compute_effective_config(
    ancestor_chain: &[Upstream],
    route: Option<&Route>,
) -> Result<Upstream, DomainError> {
    use crate::domain::model::SharingMode;

    if ancestor_chain.is_empty() {
        return Err(DomainError::Internal {
            message: "compute_effective_config called with empty ancestor_chain".into(),
        });
    }

    // Start with the root upstream as the base.
    let mut effective = ancestor_chain[0].clone();

    // Walk root → descendant, merging each layer.
    for layer in &ancestor_chain[1..] {
        // Auth merge
        merge_auth(&mut effective, layer);

        // Rate limit merge
        merge_rate_limit(&mut effective, layer);

        // Plugins merge
        merge_plugins(&mut effective, layer);

        // CORS merge
        merge_cors(&mut effective, layer)?;

        // Tags: union (add-only)
        for tag in &layer.tags {
            if !effective.tags.contains(tag) {
                effective.tags.push(tag.clone());
            }
        }

        // Server, protocol, enabled, alias: always use the selected upstream's values.
        effective.id = layer.id;
        effective.tenant_id = layer.tenant_id;
        effective.alias = layer.alias.clone();
        effective.server = layer.server.clone();
        effective.protocol = layer.protocol.clone();
        effective.enabled = layer.enabled;
        effective.headers = layer.headers.clone().or(effective.headers);
    }

    // Route-level overrides (route > upstream base per config layering).
    if let Some(route) = route {
        // Route plugins: concatenate upstream + route plugins.
        if let Some(ref route_plugins) = route.plugins {
            match route_plugins.sharing {
                SharingMode::Private => {}
                SharingMode::Inherit | SharingMode::Enforce => {
                    let mut merged_items = effective
                        .plugins
                        .as_ref()
                        .map(|p| p.items.clone())
                        .unwrap_or_default();
                    for item in &route_plugins.items {
                        if !merged_items.iter().any(|m| m == item) {
                            merged_items.push(item.clone());
                        }
                    }
                    effective.plugins = Some(crate::domain::model::PluginsConfig {
                        sharing: route_plugins.sharing,
                        items: merged_items,
                    });
                }
            }
        }

        // Route rate limit: min(effective, route).
        if let Some(ref route_rl) = route.rate_limit {
            match route_rl.sharing {
                SharingMode::Private => {}
                _ => {
                    effective.rate_limit =
                        Some(min_rate_limit(effective.rate_limit.as_ref(), route_rl));
                }
            }
        }

        // Route CORS: follows same sharing semantics as upstream-level merge.
        // Per inst-merge-3a5: private → skip; inherit → union origins;
        // enforce → use upstream CORS (keep effective unchanged).
        if let Some(ref route_cors) = route.cors {
            let effective_is_enforced = effective
                .cors
                .as_ref()
                .is_some_and(|c| c.sharing == SharingMode::Enforce);
            if !effective_is_enforced {
                match route_cors.sharing {
                    SharingMode::Private | SharingMode::Enforce => {
                        // Private → skip; Enforce → use upstream CORS.
                    }
                    SharingMode::Inherit => {
                        let mut merged = route_cors.clone();
                        if let Some(ref upstream_cors) = effective.cors {
                            for origin in &upstream_cors.allowed_origins {
                                if !merged.allowed_origins.contains(origin) {
                                    merged.allowed_origins.push(origin.clone());
                                }
                            }
                        }
                        crate::domain::cors::validate_cors_config(&merged)?;
                        effective.cors = Some(merged);
                    }
                }
            }
        }

        // Route tags: union.
        for tag in &route.tags {
            if !effective.tags.contains(tag) {
                effective.tags.push(tag.clone());
            }
        }
    }

    Ok(effective)
}

/// Merge auth config from a descendant layer onto the effective config.
///
/// Key invariant: once an ancestor sets `enforce`, no descendant can override
/// regardless of the descendant's own sharing mode.  This is defense-in-depth;
/// `validate_bind_constraints` also guards this at create/update time.
///
/// Sharing semantics:
/// - `Private` + ancestor enforced → keep ancestor (enforce is sticky)
/// - `Private` + ancestor not enforced → descendant replaces (local-only)
/// - `Inherit` → descendant overrides ancestor
/// - `Enforce` → descendant's enforce becomes sticky for further descendants
fn merge_auth(effective: &mut Upstream, layer: &Upstream) {
    use crate::domain::model::SharingMode;

    let effective_is_enforced = effective
        .auth
        .as_ref()
        .is_some_and(|a| a.sharing == SharingMode::Enforce);

    match &layer.auth {
        None => {} // Absent → inherit from previous level (no-op).
        Some(_) if effective_is_enforced => {
            // Ancestor enforced — no descendant can change it regardless of sharing mode.
        }
        Some(descendant_auth) => {
            // Private → local-only replace; Inherit → override; Enforce → becomes sticky.
            effective.auth = Some(descendant_auth.clone());
        }
    }
}

/// Merge rate limit config: `min(ancestor_enforced, descendant)`.
///
/// Key invariant: if the effective rate limit is already `Enforce`, a
/// descendant `Private` cannot drop it — `min()` is applied instead.
/// This is defense-in-depth; `validate_bind_constraints` also guards
/// this at create/update time.
fn merge_rate_limit(effective: &mut Upstream, layer: &Upstream) {
    use crate::domain::model::SharingMode;

    let effective_is_enforced = effective
        .rate_limit
        .as_ref()
        .is_some_and(|r| r.sharing == SharingMode::Enforce);

    match &layer.rate_limit {
        None => {} // Absent = inherit from previous level (no-op).
        Some(descendant_rl) => match descendant_rl.sharing {
            SharingMode::Private if effective_is_enforced => {
                // Ancestor enforced — descendant cannot escape; apply min.
                effective.rate_limit =
                    Some(min_rate_limit(effective.rate_limit.as_ref(), descendant_rl));
            }
            SharingMode::Private => {
                effective.rate_limit = Some(descendant_rl.clone());
            }
            SharingMode::Inherit | SharingMode::Enforce => {
                effective.rate_limit =
                    Some(min_rate_limit(effective.rate_limit.as_ref(), descendant_rl));
            }
        },
    }
}

/// Merge CORS config from a descendant layer onto the effective config.
///
/// Per `inst-merge-3a5` (feature 0005 — tenant hierarchy):
/// - `Private`  → skip (do not modify effective).
/// - Absent     → inherit from previous level; Private must not propagate.
/// - `Inherit`  → descendant config wins, `allowed_origins` is the union
///   of ancestor + descendant origins (deduped); Private ancestor origins
///   are excluded from the union.
/// - `Enforce`  → use ancestor CORS (keep effective unchanged).
///
/// Ancestor enforce is sticky: once effective is `Enforce`, no descendant
/// can change it regardless of sharing mode.
fn merge_cors(effective: &mut Upstream, layer: &Upstream) -> Result<(), DomainError> {
    use crate::domain::model::SharingMode;

    let effective_is_enforced = effective
        .cors
        .as_ref()
        .is_some_and(|c| c.sharing == SharingMode::Enforce);

    match &layer.cors {
        None => {
            // Absent → inherit from previous level, but Private must not propagate.
            if effective
                .cors
                .as_ref()
                .is_some_and(|c| c.sharing == SharingMode::Private)
            {
                effective.cors = None;
            }
        }
        Some(_) if effective_is_enforced => {
            // Ancestor enforced — no descendant can change it.
        }
        Some(descendant_cors) => match descendant_cors.sharing {
            SharingMode::Private => {
                // Per inst-merge-3a5: private → skip (do not modify effective).
            }
            SharingMode::Enforce => {
                // Per inst-merge-3a5: enforce → use ancestor CORS (keep effective unchanged).
            }
            SharingMode::Inherit => {
                // Union allowed_origins from ancestor + descendant, skipping Private ancestor.
                let mut merged = descendant_cors.clone();
                if let Some(ref ancestor) = effective.cors
                    && ancestor.sharing != SharingMode::Private
                {
                    for origin in &ancestor.allowed_origins {
                        if !merged.allowed_origins.contains(origin) {
                            merged.allowed_origins.push(origin.clone());
                        }
                    }
                }
                crate::domain::cors::validate_cors_config(&merged)?;
                effective.cors = Some(merged);
            }
        },
    }

    Ok(())
}

/// Return the stricter of two rate limit configs (lower rate wins).
fn min_rate_limit(
    a: Option<&crate::domain::model::RateLimitConfig>,
    b: &crate::domain::model::RateLimitConfig,
) -> crate::domain::model::RateLimitConfig {
    match a {
        None => b.clone(),
        Some(a) => {
            let a_rate = rate_per_second(a);
            let b_rate = rate_per_second(b);
            if b_rate < a_rate {
                b.clone()
            } else {
                a.clone()
            }
        }
    }
}

/// Normalize a rate limit to requests-per-second for comparison.
fn rate_per_second(rl: &crate::domain::model::RateLimitConfig) -> f64 {
    use crate::domain::model::Window;
    let divisor = match rl.sustained.window {
        Window::Second => 1.0,
        Window::Minute => 60.0,
        Window::Hour => 3600.0,
        Window::Day => 86400.0,
    };
    f64::from(rl.sustained.rate) / divisor
}

/// Merge plugins config: concatenate ancestor + descendant; enforced can't be removed.
///
/// Key invariant: if the effective plugins are already `Enforce`, a
/// descendant `Private` cannot drop enforced items — they are preserved
/// and the descendant's items are appended.
fn merge_plugins(effective: &mut Upstream, layer: &Upstream) {
    use crate::domain::model::SharingMode;

    let effective_is_enforced = effective
        .plugins
        .as_ref()
        .is_some_and(|p| p.sharing == SharingMode::Enforce);

    match &layer.plugins {
        None => {} // Inherit from previous level.
        Some(descendant_plugins) => match descendant_plugins.sharing {
            SharingMode::Private if effective_is_enforced => {
                // Ancestor enforced — preserve enforced items, append descendant.
                let mut merged = effective
                    .plugins
                    .as_ref()
                    .map(|p| p.items.clone())
                    .unwrap_or_default();
                for item in &descendant_plugins.items {
                    if !merged.iter().any(|m| m == item) {
                        merged.push(item.clone());
                    }
                }
                effective.plugins = Some(crate::domain::model::PluginsConfig {
                    sharing: SharingMode::Enforce,
                    items: merged,
                });
            }
            SharingMode::Private => {
                effective.plugins = Some(descendant_plugins.clone());
            }
            SharingMode::Inherit | SharingMode::Enforce => {
                // Concatenate: ancestor + descendant (dedup).
                let mut merged = effective
                    .plugins
                    .as_ref()
                    .map(|p| p.items.clone())
                    .unwrap_or_default();
                for item in &descendant_plugins.items {
                    if !merged.iter().any(|m| m == item) {
                        merged.push(item.clone());
                    }
                }
                effective.plugins = Some(crate::domain::model::PluginsConfig {
                    sharing: descendant_plugins.sharing,
                    items: merged,
                });
            }
        },
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
#[path = "management_tests.rs"]
mod management_tests;
