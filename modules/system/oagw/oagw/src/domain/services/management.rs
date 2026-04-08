use std::sync::Arc;

use super::ControlPlaneService;
use std::net::IpAddr;

use crate::domain::error::DomainError;
use crate::domain::model::{
    CreateRouteRequest, CreateUpstreamRequest, Endpoint, ListQuery, Route, UpdateRouteRequest,
    UpdateUpstreamRequest, Upstream,
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

        // Apply partial update.
        if let Some(server) = req.server {
            validate_endpoints(&server.endpoints)?;
            existing.server = server;
        }
        if let Some(protocol) = req.protocol {
            existing.protocol = protocol;
        }

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
        let effective_auth = req.auth.as_ref().or(existing.auth.as_ref());
        let effective_rate_limit = req.rate_limit.as_ref().or(existing.rate_limit.as_ref());
        let effective_plugins = req.plugins.as_ref().or(existing.plugins.as_ref());
        let has_overrides = effective_auth.is_some()
            || effective_rate_limit.is_some()
            || effective_plugins.is_some();

        if has_overrides || endpoints_changed || req.alias.is_some() {
            let tenant_chain = self.build_tenant_chain(ctx).await?;
            self.validate_ancestor_bind(
                ctx,
                &tenant_chain,
                &existing.alias,
                &BindOverrides {
                    auth: effective_auth,
                    rate_limit: effective_rate_limit,
                    plugins: effective_plugins,
                },
            )
            .await?;
        }

        if let Some(auth) = req.auth {
            existing.auth = Some(auth);
        }
        if let Some(headers) = req.headers {
            existing.headers = Some(headers);
        }
        if let Some(plugins) = req.plugins {
            existing.plugins = Some(plugins);
        }
        if let Some(rate_limit) = req.rate_limit {
            existing.rate_limit = Some(rate_limit);
        }
        if let Some(tags) = req.tags {
            existing.tags = tags;
        }
        if let Some(enabled) = req.enabled {
            existing.enabled = enabled;
        }

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
            tags: req.tags,
            priority: req.priority,
            enabled: req.enabled,
        };

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
        upstream_id: Uuid,
        query: &ListQuery,
    ) -> Result<Vec<Route>, DomainError> {
        let tenant_id = ctx.subject_tenant_id();
        self.routes
            .list_by_upstream(tenant_id, upstream_id, query)
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

        if let Some(match_rules) = req.match_rules {
            existing.match_rules = match_rules;
        }
        if let Some(plugins) = req.plugins {
            existing.plugins = Some(plugins);
        }
        if let Some(rate_limit) = req.rate_limit {
            existing.rate_limit = Some(rate_limit);
        }
        if let Some(tags) = req.tags {
            existing.tags = tags;
        }
        if let Some(priority) = req.priority {
            existing.priority = priority;
        }
        if let Some(enabled) = req.enabled {
            existing.enabled = enabled;
        }

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
        if strip_brackets(&ep.host)
            .parse::<std::net::Ipv6Addr>()
            .is_ok()
        {
            return Err(DomainError::validation(format!(
                "endpoint[{i}] uses IPv6 address '{}'; IPv6 endpoints are not yet supported",
                ep.host
            )));
        }
    }

    // Check all-IP vs all-hostname consistency.
    let ip_count = endpoints
        .iter()
        .filter(|ep| strip_brackets(&ep.host).parse::<IpAddr>().is_ok())
        .count();
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

/// Strip surrounding `[` and `]` from a host string so that bracketed IPv6
/// literals (e.g. `[2001:db8::1]`) can be parsed by `Ipv6Addr` / `IpAddr`.
fn strip_brackets(host: &str) -> &str {
    host.strip_prefix('[')
        .and_then(|s| s.strip_suffix(']'))
        .unwrap_or(host)
}

/// Normalize an alias to lowercase. Hostname trailing dots are already
/// handled by `Endpoint::normalized_host()` during derivation; this covers
/// user-provided explicit aliases. All trailing dots are stripped.
fn normalize_alias(alias: &str) -> String {
    alias.to_ascii_lowercase().trim_end_matches('.').to_string()
}

/// Check whether the given endpoints are all IP addresses.
fn endpoints_are_ip(endpoints: &[Endpoint]) -> bool {
    !endpoints.is_empty()
        && endpoints
            .iter()
            .all(|ep| strip_brackets(&ep.host).parse::<IpAddr>().is_ok())
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
/// (`auth`, `rate_limit`, `plugins`) is not `private`.
///
/// Returns `false` when all shareable fields are `None` — this is intentional.
/// An upstream with no auth, rate_limit, or plugins has no configuration to
/// share with descendants, so it is treated as invisible. Fields without a
/// sharing mode (e.g. `headers`) do not contribute to visibility.
///
/// TODO: when CORS sharing mode lands on the domain model, add `cors` to
/// the visibility check per the spec (`cors_sharing` is listed in step 2b).
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

    auth_shared || rate_shared || plugins_shared
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
mod tests {
    use std::sync::Arc;

    use crate::domain::model::{
        Endpoint, HttpMatch, HttpMethod, MatchRules, PathSuffixMode, Scheme, Server,
    };

    use super::*;
    use crate::domain::test_support::{
        MockCredStoreClient, MockTenantResolverClient, allow_all_enforcer,
    };
    use crate::infra::storage::{InMemoryRouteRepo, InMemoryUpstreamRepo};
    use tenant_resolver_sdk::TenantId;

    fn make_service() -> ControlPlaneServiceImpl {
        ControlPlaneServiceImpl::new(
            Arc::new(InMemoryUpstreamRepo::new()),
            Arc::new(InMemoryRouteRepo::new()),
            Arc::new(MockTenantResolverClient::single_tenant()),
            allow_all_enforcer(),
            Arc::new(MockCredStoreClient::empty()),
        )
    }

    fn make_service_with_resolver(resolver: MockTenantResolverClient) -> ControlPlaneServiceImpl {
        ControlPlaneServiceImpl::new(
            Arc::new(InMemoryUpstreamRepo::new()),
            Arc::new(InMemoryRouteRepo::new()),
            Arc::new(resolver),
            allow_all_enforcer(),
            Arc::new(MockCredStoreClient::empty()),
        )
    }

    fn make_service_with_resolver_and_creds(
        resolver: MockTenantResolverClient,
        creds: Vec<(String, String)>,
    ) -> ControlPlaneServiceImpl {
        ControlPlaneServiceImpl::new(
            Arc::new(InMemoryUpstreamRepo::new()),
            Arc::new(InMemoryRouteRepo::new()),
            Arc::new(resolver),
            allow_all_enforcer(),
            Arc::new(MockCredStoreClient::with_secrets(creds)),
        )
    }

    fn test_ctx(tenant_id: Uuid) -> SecurityContext {
        SecurityContext::builder()
            .subject_tenant_id(tenant_id)
            .subject_id(Uuid::new_v4())
            .build()
            .expect("test security context")
    }

    /// Hostname-based upstream — alias is auto-derived as `api.openai.com`.
    fn make_create_upstream_hostname() -> CreateUpstreamRequest {
        CreateUpstreamRequest {
            server: Server {
                endpoints: vec![Endpoint {
                    scheme: Scheme::Https,
                    host: "api.openai.com".into(),
                    port: 443,
                }],
            },
            protocol: "gts.x.core.oagw.protocol.v1~x.core.oagw.http.v1".into(),
            alias: None,
            auth: None,
            headers: None,
            plugins: None,
            rate_limit: None,
            tags: vec![],
            enabled: true,
        }
    }

    /// IP-based upstream — requires an explicit alias.
    fn make_create_upstream_ip(alias: &str) -> CreateUpstreamRequest {
        CreateUpstreamRequest {
            server: Server {
                endpoints: vec![Endpoint {
                    scheme: Scheme::Https,
                    host: "10.0.0.1".into(),
                    port: 443,
                }],
            },
            protocol: "gts.x.core.oagw.protocol.v1~x.core.oagw.http.v1".into(),
            alias: Some(alias.into()),
            auth: None,
            headers: None,
            plugins: None,
            rate_limit: None,
            tags: vec![],
            enabled: true,
        }
    }

    fn make_create_route(upstream_id: Uuid) -> CreateRouteRequest {
        CreateRouteRequest {
            upstream_id,
            match_rules: MatchRules {
                http: Some(HttpMatch {
                    methods: vec![HttpMethod::Post],
                    path: "/v1/chat/completions".into(),
                    query_allowlist: vec![],
                    path_suffix_mode: PathSuffixMode::Append,
                }),
                grpc: None,
            },
            plugins: None,
            rate_limit: None,
            tags: vec![],
            priority: 0,
            enabled: true,
        }
    }

    #[tokio::test]
    async fn upstream_crud_lifecycle() {
        let svc = make_service();
        let tenant = Uuid::new_v4();
        let ctx = test_ctx(tenant);

        // Create (IP-based, explicit alias)
        let u = svc
            .create_upstream(&ctx, make_create_upstream_ip("openai"))
            .await
            .unwrap();
        assert_eq!(u.alias, "openai");

        // Get
        let fetched = svc.get_upstream(&ctx, u.id).await.unwrap();
        assert_eq!(fetched.id, u.id);

        // Update alias (allowed for IP-based endpoints)
        let updated = svc
            .update_upstream(
                &ctx,
                u.id,
                UpdateUpstreamRequest {
                    alias: Some("openai-v2".into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(updated.alias, "openai-v2");
        assert_eq!(updated.id, u.id);

        // List
        let list = svc
            .list_upstreams(&ctx, &ListQuery::default())
            .await
            .unwrap();
        assert_eq!(list.len(), 1);

        // Delete
        svc.delete_upstream(&ctx, u.id).await.unwrap();
        assert!(svc.get_upstream(&ctx, u.id).await.is_err());
    }

    #[tokio::test]
    async fn alias_auto_generation() {
        let svc = make_service();
        let tenant = Uuid::new_v4();
        let ctx = test_ctx(tenant);

        // Standard port (443) — port omitted in alias.
        let u1 = svc
            .create_upstream(&ctx, make_create_upstream_hostname())
            .await
            .unwrap();
        assert_eq!(u1.alias, "api.openai.com");

        // Non-standard port — port included.
        let req = CreateUpstreamRequest {
            server: Server {
                endpoints: vec![Endpoint {
                    scheme: Scheme::Https,
                    host: "api.openai.com".into(),
                    port: 8443,
                }],
            },
            protocol: "gts.x.core.oagw.protocol.v1~x.core.oagw.http.v1".into(),
            alias: None,
            auth: None,
            headers: None,
            plugins: None,
            rate_limit: None,
            tags: vec![],
            enabled: true,
        };
        let u2 = svc.create_upstream(&ctx, req).await.unwrap();
        assert_eq!(u2.alias, "api.openai.com:8443");
    }

    #[tokio::test]
    async fn alias_rejects_path_traversal() {
        let svc = make_service();
        let tenant = Uuid::new_v4();
        let ctx = test_ctx(tenant);

        let err = svc
            .create_upstream(&ctx, make_create_upstream_ip("../../admin"))
            .await
            .unwrap_err();
        assert!(matches!(err, DomainError::Validation { .. }));
    }

    #[tokio::test]
    async fn alias_rejects_empty() {
        let svc = make_service();
        let tenant = Uuid::new_v4();
        let ctx = test_ctx(tenant);

        let err = svc
            .create_upstream(&ctx, make_create_upstream_ip(""))
            .await
            .unwrap_err();
        assert!(matches!(err, DomainError::Validation { .. }));
    }

    #[tokio::test]
    async fn alias_rejects_slashes() {
        let svc = make_service();
        let tenant = Uuid::new_v4();
        let ctx = test_ctx(tenant);

        let err = svc
            .create_upstream(&ctx, make_create_upstream_ip("foo/bar"))
            .await
            .unwrap_err();
        assert!(matches!(err, DomainError::Validation { .. }));
    }

    #[tokio::test]
    async fn duplicate_alias_conflict() {
        let svc = make_service();
        let tenant = Uuid::new_v4();
        let ctx = test_ctx(tenant);

        svc.create_upstream(&ctx, make_create_upstream_ip("openai"))
            .await
            .unwrap();

        let err = svc
            .create_upstream(&ctx, make_create_upstream_ip("openai"))
            .await
            .unwrap_err();
        assert!(matches!(err, DomainError::Conflict { .. }));
    }

    #[tokio::test]
    async fn route_create_with_wrong_tenant_upstream() {
        let svc = make_service();
        let t1 = Uuid::new_v4();
        let t2 = Uuid::new_v4();
        let ctx1 = test_ctx(t1);
        let ctx2 = test_ctx(t2);

        let u = svc
            .create_upstream(&ctx1, make_create_upstream_ip("openai"))
            .await
            .unwrap();

        // Try to create route in different tenant referencing t1's upstream.
        let err = svc
            .create_route(&ctx2, make_create_route(u.id))
            .await
            .unwrap_err();
        assert!(matches!(err, DomainError::Validation { .. }));
    }

    #[tokio::test]
    async fn alias_resolution_enabled() {
        let svc = make_service();
        let tenant = Uuid::new_v4();
        let ctx = test_ctx(tenant);

        let u = svc
            .create_upstream(&ctx, make_create_upstream_ip("openai"))
            .await
            .unwrap();

        let chain = svc.build_tenant_chain(&ctx).await.unwrap();
        let (resolved, _) = svc
            .resolve_alias(&ctx, &chain, "openai", None)
            .await
            .unwrap();
        assert_eq!(resolved.id, u.id);
    }

    #[tokio::test]
    async fn alias_resolution_disabled_returns_503() {
        let svc = make_service();
        let tenant = Uuid::new_v4();
        let ctx = test_ctx(tenant);

        let u = svc
            .create_upstream(&ctx, make_create_upstream_ip("openai"))
            .await
            .unwrap();

        // Disable the upstream.
        svc.update_upstream(
            &ctx,
            u.id,
            UpdateUpstreamRequest {
                enabled: Some(false),
                ..Default::default()
            },
        )
        .await
        .unwrap();

        let chain = svc.build_tenant_chain(&ctx).await.unwrap();
        let err = svc
            .resolve_alias(&ctx, &chain, "openai", None)
            .await
            .unwrap_err();
        assert!(matches!(err, DomainError::UpstreamDisabled { .. }));
    }

    #[tokio::test]
    async fn alias_resolution_nonexistent_returns_404() {
        let svc = make_service();
        let tenant = Uuid::new_v4();
        let ctx = test_ctx(tenant);

        let chain = svc.build_tenant_chain(&ctx).await.unwrap();
        let err = svc
            .resolve_alias(&ctx, &chain, "nonexistent", None)
            .await
            .unwrap_err();
        assert!(matches!(err, DomainError::NotFound { .. }));
    }

    #[tokio::test]
    async fn route_matching_through_cp() {
        let svc = make_service();
        let tenant = Uuid::new_v4();
        let ctx = test_ctx(tenant);

        let u = svc
            .create_upstream(&ctx, make_create_upstream_ip("openai"))
            .await
            .unwrap();
        let r = svc
            .create_route(&ctx, make_create_route(u.id))
            .await
            .unwrap();

        let chain = svc.build_tenant_chain(&ctx).await.unwrap();
        let matched = ControlPlaneServiceImpl::find_route_in_chain(
            &*svc.routes,
            &chain,
            u.id,
            "POST",
            "/v1/chat/completions",
        )
        .await
        .unwrap();
        assert_eq!(matched.id, r.id);
    }

    #[tokio::test]
    async fn route_matching_no_match_returns_404() {
        let svc = make_service();
        let tenant = Uuid::new_v4();
        let ctx = test_ctx(tenant);

        let u = svc
            .create_upstream(&ctx, make_create_upstream_ip("openai"))
            .await
            .unwrap();

        let chain = svc.build_tenant_chain(&ctx).await.unwrap();
        let err = ControlPlaneServiceImpl::find_route_in_chain(
            &*svc.routes,
            &chain,
            u.id,
            "GET",
            "/v1/unknown",
        )
        .await
        .unwrap_err();
        assert!(matches!(err, DomainError::NotFound { .. }));
    }

    // -- validate_endpoints tests --

    #[test]
    fn validate_endpoints_rejects_empty() {
        let err = validate_endpoints(&[]).unwrap_err();
        assert!(matches!(err, DomainError::Validation { .. }));
    }

    #[test]
    fn validate_endpoints_rejects_mixed_ip_and_hostname() {
        let endpoints = vec![
            Endpoint {
                scheme: Scheme::Https,
                host: "10.0.0.1".into(),
                port: 443,
            },
            Endpoint {
                scheme: Scheme::Https,
                host: "api.example.com".into(),
                port: 443,
            },
        ];
        let err = validate_endpoints(&endpoints).unwrap_err();
        match err {
            DomainError::Validation { detail, .. } => {
                assert!(
                    detail.contains("mixed"),
                    "expected mixed error, got: {detail}"
                );
            }
            _ => panic!("expected Validation, got: {err:?}"),
        }
    }

    #[test]
    fn validate_endpoints_rejects_mixed_scheme() {
        let endpoints = vec![
            Endpoint {
                scheme: Scheme::Https,
                host: "a.example.com".into(),
                port: 443,
            },
            Endpoint {
                scheme: Scheme::Http,
                host: "b.example.com".into(),
                port: 443,
            },
        ];
        let err = validate_endpoints(&endpoints).unwrap_err();
        match err {
            DomainError::Validation { detail, .. } => {
                assert!(
                    detail.contains("scheme"),
                    "expected scheme error, got: {detail}"
                );
            }
            _ => panic!("expected Validation, got: {err:?}"),
        }
    }

    #[test]
    fn validate_endpoints_accepts_all_ip() {
        let endpoints = vec![
            Endpoint {
                scheme: Scheme::Https,
                host: "10.0.0.1".into(),
                port: 443,
            },
            Endpoint {
                scheme: Scheme::Https,
                host: "10.0.0.2".into(),
                port: 443,
            },
        ];
        assert!(validate_endpoints(&endpoints).is_ok());
    }

    #[test]
    fn validate_endpoints_accepts_all_hostname() {
        let endpoints = vec![
            Endpoint {
                scheme: Scheme::Https,
                host: "a.example.com".into(),
                port: 443,
            },
            Endpoint {
                scheme: Scheme::Https,
                host: "b.example.com".into(),
                port: 443,
            },
        ];
        assert!(validate_endpoints(&endpoints).is_ok());
    }

    #[test]
    fn validate_endpoints_rejects_mixed_ports() {
        let endpoints = vec![
            Endpoint {
                scheme: Scheme::Https,
                host: "a.example.com".into(),
                port: 443,
            },
            Endpoint {
                scheme: Scheme::Https,
                host: "b.example.com".into(),
                port: 8443,
            },
        ];
        let err = validate_endpoints(&endpoints).unwrap_err();
        assert!(
            err.to_string().contains("port"),
            "expected port error, got: {err}"
        );
    }

    #[test]
    fn validate_endpoints_accepts_single() {
        let endpoints = vec![Endpoint {
            scheme: Scheme::Https,
            host: "api.openai.com".into(),
            port: 443,
        }];
        assert!(validate_endpoints(&endpoints).is_ok());
    }

    #[test]
    fn validate_endpoints_rejects_ipv6() {
        let endpoints = vec![Endpoint {
            scheme: Scheme::Https,
            host: "::1".into(),
            port: 443,
        }];
        let err = validate_endpoints(&endpoints).unwrap_err();
        match err {
            DomainError::Validation { detail, .. } => {
                assert!(
                    detail.contains("IPv6"),
                    "expected IPv6 error, got: {detail}"
                );
                assert!(
                    detail.contains("not yet supported"),
                    "expected 'not yet supported', got: {detail}"
                );
            }
            _ => panic!("expected Validation, got: {err:?}"),
        }
    }

    #[test]
    fn validate_endpoints_rejects_ipv6_full_address() {
        let endpoints = vec![Endpoint {
            scheme: Scheme::Https,
            host: "2001:db8::1".into(),
            port: 8443,
        }];
        let err = validate_endpoints(&endpoints).unwrap_err();
        assert!(matches!(err, DomainError::Validation { .. }));
    }

    #[test]
    fn validate_endpoints_rejects_bracketed_ipv6() {
        let endpoints = vec![Endpoint {
            scheme: Scheme::Https,
            host: "[2001:db8::1]".into(),
            port: 8443,
        }];
        let err = validate_endpoints(&endpoints).unwrap_err();
        match err {
            DomainError::Validation { detail, .. } => {
                assert!(
                    detail.contains("IPv6"),
                    "expected IPv6 error, got: {detail}"
                );
                assert!(
                    detail.contains("not yet supported"),
                    "expected 'not yet supported', got: {detail}"
                );
            }
            _ => panic!("expected Validation, got: {err:?}"),
        }
    }

    // -- validate_hostname (RFC 1123) tests --

    #[test]
    fn validate_endpoints_rejects_empty_label() {
        let endpoints = vec![Endpoint {
            scheme: Scheme::Https,
            host: "api..openai.com".into(),
            port: 443,
        }];
        let err = validate_endpoints(&endpoints).unwrap_err();
        match err {
            DomainError::Validation { detail, .. } => {
                assert!(
                    detail.contains("empty label"),
                    "expected empty label error, got: {detail}"
                );
            }
            _ => panic!("expected Validation, got: {err:?}"),
        }
    }

    #[test]
    fn validate_endpoints_rejects_leading_hyphen() {
        let endpoints = vec![Endpoint {
            scheme: Scheme::Https,
            host: "-api.openai.com".into(),
            port: 443,
        }];
        let err = validate_endpoints(&endpoints).unwrap_err();
        match err {
            DomainError::Validation { detail, .. } => {
                assert!(
                    detail.contains("must not start or end with '-'"),
                    "expected hyphen error, got: {detail}"
                );
            }
            _ => panic!("expected Validation, got: {err:?}"),
        }
    }

    #[test]
    fn validate_endpoints_rejects_trailing_hyphen() {
        let endpoints = vec![Endpoint {
            scheme: Scheme::Https,
            host: "api-.openai.com".into(),
            port: 443,
        }];
        let err = validate_endpoints(&endpoints).unwrap_err();
        assert!(matches!(err, DomainError::Validation { .. }));
    }

    #[test]
    fn validate_endpoints_rejects_underscore_in_hostname() {
        let endpoints = vec![Endpoint {
            scheme: Scheme::Https,
            host: "api_v2.openai.com".into(),
            port: 443,
        }];
        let err = validate_endpoints(&endpoints).unwrap_err();
        match err {
            DomainError::Validation { detail, .. } => {
                assert!(
                    detail.contains("invalid characters"),
                    "expected invalid chars error, got: {detail}"
                );
            }
            _ => panic!("expected Validation, got: {err:?}"),
        }
    }

    #[test]
    fn validate_endpoints_rejects_overlength_label() {
        let long_label = "a".repeat(64);
        let host = format!("{long_label}.example.com");
        let endpoints = vec![Endpoint {
            scheme: Scheme::Https,
            host,
            port: 443,
        }];
        let err = validate_endpoints(&endpoints).unwrap_err();
        match err {
            DomainError::Validation { detail, .. } => {
                assert!(
                    detail.contains("exceeds 63"),
                    "expected label length error, got: {detail}"
                );
            }
            _ => panic!("expected Validation, got: {err:?}"),
        }
    }

    #[test]
    fn validate_endpoints_accepts_trailing_dot_fqdn() {
        let endpoints = vec![Endpoint {
            scheme: Scheme::Https,
            host: "api.openai.com.".into(),
            port: 443,
        }];
        assert!(validate_endpoints(&endpoints).is_ok());
    }

    #[test]
    fn validate_endpoints_accepts_max_length_label() {
        let label = "a".repeat(63);
        let host = format!("{label}.example.com");
        let endpoints = vec![Endpoint {
            scheme: Scheme::Https,
            host,
            port: 443,
        }];
        assert!(validate_endpoints(&endpoints).is_ok());
    }

    #[test]
    fn validate_endpoints_rejects_empty_hostname() {
        let endpoints = vec![Endpoint {
            scheme: Scheme::Https,
            host: "".into(),
            port: 443,
        }];
        let err = validate_endpoints(&endpoints).unwrap_err();
        assert!(matches!(err, DomainError::Validation { .. }));
    }

    #[tokio::test]
    async fn delete_upstream_cascades_routes() {
        let svc = make_service();
        let tenant = Uuid::new_v4();
        let ctx = test_ctx(tenant);

        let u = svc
            .create_upstream(&ctx, make_create_upstream_ip("openai"))
            .await
            .unwrap();
        let r = svc
            .create_route(&ctx, make_create_route(u.id))
            .await
            .unwrap();

        svc.delete_upstream(&ctx, u.id).await.unwrap();

        // Route should be gone.
        assert!(svc.get_route(&ctx, r.id).await.is_err());
    }

    // -- Alias resolution tests --

    #[tokio::test]
    async fn resolve_alias_walks_tenant_chain_to_ancestor() {
        use crate::domain::model::{AuthConfig, SharingMode};

        let root = Uuid::new_v4();
        let child = Uuid::new_v4();
        let resolver =
            MockTenantResolverClient::with_hierarchy(vec![TenantId(root), TenantId(child)]);
        let svc = make_service_with_resolver(resolver);

        // Create upstream in root tenant with inherit sharing (visible to descendants).
        let root_ctx = test_ctx(root);
        let mut req = make_create_upstream_hostname();
        req.auth = Some(AuthConfig {
            plugin_type: "noop".into(),
            sharing: SharingMode::Inherit,
            config: None,
        });
        let root_upstream = svc.create_upstream(&root_ctx, req).await.unwrap();

        // Child tenant should resolve the alias via tenant chain walk.
        let child_ctx = test_ctx(child);
        let chain = svc.build_tenant_chain(&child_ctx).await.unwrap();
        let (resolved, _) = svc
            .resolve_alias(&child_ctx, &chain, "api.openai.com", None)
            .await
            .unwrap();
        assert_eq!(resolved.id, root_upstream.id);
    }

    #[tokio::test]
    async fn resolve_alias_child_shadows_ancestor() {
        use crate::domain::model::{AuthConfig, SharingMode};

        let root = Uuid::new_v4();
        let child = Uuid::new_v4();
        let resolver =
            MockTenantResolverClient::with_hierarchy(vec![TenantId(root), TenantId(child)]);
        let svc = make_service_with_resolver(resolver);

        // Create upstream in root with inherit sharing.
        let root_ctx = test_ctx(root);
        let mut req = make_create_upstream_hostname();
        req.auth = Some(AuthConfig {
            plugin_type: "noop".into(),
            sharing: SharingMode::Inherit,
            config: None,
        });
        svc.create_upstream(&root_ctx, req).await.unwrap();

        // Create upstream with same host in child tenant (same derived alias shadows root).
        let child_ctx = test_ctx(child);
        let child_upstream = svc
            .create_upstream(&child_ctx, make_create_upstream_hostname())
            .await
            .unwrap();

        // Child resolves to its own upstream (shadow wins).
        let chain = svc.build_tenant_chain(&child_ctx).await.unwrap();
        let (resolved, _) = svc
            .resolve_alias(&child_ctx, &chain, "api.openai.com", None)
            .await
            .unwrap();
        assert_eq!(resolved.id, child_upstream.id);
    }

    #[tokio::test]
    async fn resolve_alias_private_ancestor_not_visible() {
        let root = Uuid::new_v4();
        let child = Uuid::new_v4();
        let resolver =
            MockTenantResolverClient::with_hierarchy(vec![TenantId(root), TenantId(child)]);
        let svc = make_service_with_resolver(resolver);

        // Create upstream in root with all-private sharing (default).
        let root_ctx = test_ctx(root);
        svc.create_upstream(&root_ctx, make_create_upstream_hostname())
            .await
            .unwrap();

        // Child should NOT see the private upstream → NotFound.
        let child_ctx = test_ctx(child);
        let chain = svc.build_tenant_chain(&child_ctx).await.unwrap();
        let err = svc
            .resolve_alias(&child_ctx, &chain, "api.openai.com", None)
            .await
            .unwrap_err();
        assert!(matches!(err, DomainError::NotFound { .. }));
    }

    #[tokio::test]
    async fn resolve_alias_disabled_ancestor_falls_through() {
        use crate::domain::model::{AuthConfig, SharingMode};

        let root = Uuid::new_v4();
        let parent = Uuid::new_v4();
        let child = Uuid::new_v4();
        let resolver = MockTenantResolverClient::with_hierarchy(vec![
            TenantId(root),
            TenantId(parent),
            TenantId(child),
        ]);
        let svc = make_service_with_resolver(resolver);

        // Create disabled upstream in parent with inherit sharing.
        let parent_ctx = test_ctx(parent);
        let mut req = make_create_upstream_hostname();
        req.enabled = false;
        req.auth = Some(AuthConfig {
            plugin_type: "noop".into(),
            sharing: SharingMode::Inherit,
            config: None,
        });
        svc.create_upstream(&parent_ctx, req).await.unwrap();

        // Create enabled upstream in root with inherit sharing.
        let root_ctx = test_ctx(root);
        let mut req2 = make_create_upstream_hostname();
        req2.auth = Some(AuthConfig {
            plugin_type: "noop".into(),
            sharing: SharingMode::Inherit,
            config: None,
        });
        let root_upstream = svc.create_upstream(&root_ctx, req2).await.unwrap();

        // Child resolves: parent disabled → falls through to root.
        let child_ctx = test_ctx(child);
        let chain = svc.build_tenant_chain(&child_ctx).await.unwrap();
        let (resolved, _) = svc
            .resolve_alias(&child_ctx, &chain, "api.openai.com", None)
            .await
            .unwrap();
        assert_eq!(resolved.id, root_upstream.id);
    }

    #[tokio::test]
    async fn resolve_alias_all_disabled_returns_upstream_disabled() {
        use crate::domain::model::{AuthConfig, SharingMode};

        let root = Uuid::new_v4();
        let child = Uuid::new_v4();
        let resolver =
            MockTenantResolverClient::with_hierarchy(vec![TenantId(root), TenantId(child)]);
        let svc = make_service_with_resolver(resolver);

        // Create disabled upstream in root with inherit sharing.
        let root_ctx = test_ctx(root);
        let mut req = make_create_upstream_hostname();
        req.enabled = false;
        req.auth = Some(AuthConfig {
            plugin_type: "noop".into(),
            sharing: SharingMode::Inherit,
            config: None,
        });
        svc.create_upstream(&root_ctx, req).await.unwrap();

        // Child resolves: only disabled match → UpstreamDisabled.
        let child_ctx = test_ctx(child);
        let chain = svc.build_tenant_chain(&child_ctx).await.unwrap();
        let err = svc
            .resolve_alias(&child_ctx, &chain, "api.openai.com", None)
            .await
            .unwrap_err();
        assert!(matches!(err, DomainError::UpstreamDisabled { .. }));
    }

    #[tokio::test]
    async fn resolve_alias_disabled_child_falls_through_to_ancestor() {
        use crate::domain::model::{AuthConfig, SharingMode};

        let root = Uuid::new_v4();
        let child = Uuid::new_v4();
        let resolver =
            MockTenantResolverClient::with_hierarchy(vec![TenantId(root), TenantId(child)]);
        let svc = make_service_with_resolver(resolver);

        // Create enabled upstream in root with inherit sharing.
        let root_ctx = test_ctx(root);
        let mut root_req = make_create_upstream_hostname();
        root_req.auth = Some(AuthConfig {
            plugin_type: "noop".into(),
            sharing: SharingMode::Inherit,
            config: None,
        });
        let root_upstream = svc.create_upstream(&root_ctx, root_req).await.unwrap();

        // Create disabled upstream in child with same host (same derived alias).
        let child_ctx = test_ctx(child);
        let mut child_req = make_create_upstream_hostname();
        child_req.enabled = false;
        child_req.auth = Some(AuthConfig {
            plugin_type: "noop".into(),
            sharing: SharingMode::Inherit,
            config: None,
        });
        svc.create_upstream(&child_ctx, child_req).await.unwrap();

        // Child resolves: own upstream disabled → falls through to root ancestor.
        let chain = svc.build_tenant_chain(&child_ctx).await.unwrap();
        let (resolved, _) = svc
            .resolve_alias(&child_ctx, &chain, "api.openai.com", None)
            .await
            .unwrap();
        assert_eq!(resolved.id, root_upstream.id);
    }

    #[tokio::test]
    async fn resolve_alias_no_match_in_tenant_chain_returns_not_found() {
        let root = Uuid::new_v4();
        let child = Uuid::new_v4();
        let resolver =
            MockTenantResolverClient::with_hierarchy(vec![TenantId(root), TenantId(child)]);
        let svc = make_service_with_resolver(resolver);

        // No upstreams created anywhere.
        let child_ctx = test_ctx(child);
        let chain = svc.build_tenant_chain(&child_ctx).await.unwrap();
        let err = svc
            .resolve_alias(&child_ctx, &chain, "nonexistent", None)
            .await
            .unwrap_err();
        assert!(matches!(err, DomainError::NotFound { .. }));
    }

    // -- Effective config merge tests --

    use std::collections::HashMap;

    use crate::domain::model::{
        AuthConfig, PluginBinding, PluginsConfig, RateLimitAlgorithm, RateLimitConfig,
        RateLimitScope, RateLimitStrategy, SharingMode, SustainedRate, Window,
    };

    fn make_upstream(
        tenant_id: Uuid,
        alias: &str,
        auth: Option<AuthConfig>,
        rate_limit: Option<RateLimitConfig>,
        plugins: Option<PluginsConfig>,
        tags: Vec<String>,
    ) -> Upstream {
        Upstream {
            id: Uuid::new_v4(),
            tenant_id,
            alias: alias.into(),
            server: Server {
                endpoints: vec![Endpoint {
                    scheme: Scheme::Https,
                    host: "api.example.com".into(),
                    port: 443,
                }],
            },
            protocol: "http".into(),
            enabled: true,
            auth,
            headers: None,
            plugins,
            rate_limit,
            tags,
        }
    }

    fn make_rate_limit(sharing: SharingMode, rate: u32, window: Window) -> RateLimitConfig {
        RateLimitConfig {
            sharing,
            algorithm: RateLimitAlgorithm::TokenBucket,
            sustained: SustainedRate { rate, window },
            burst: None,
            scope: RateLimitScope::Tenant,
            strategy: RateLimitStrategy::Reject,
            cost: 1,
        }
    }

    #[test]
    fn effective_config_single_upstream() {
        let t = Uuid::new_v4();
        let u = make_upstream(t, "openai", None, None, None, vec!["a".into()]);
        let effective = compute_effective_config(std::slice::from_ref(&u), None).unwrap();
        assert_eq!(effective.id, u.id);
        assert_eq!(effective.tags, vec!["a".to_string()]);
    }

    #[test]
    fn effective_config_auth_inherit_descendant_overrides() {
        let root_id = Uuid::new_v4();
        let child_id = Uuid::new_v4();

        let root_auth = AuthConfig {
            plugin_type: "apikey".into(),
            sharing: SharingMode::Inherit,
            config: None,
        };
        let child_auth = AuthConfig {
            plugin_type: "oauth2".into(),
            sharing: SharingMode::Inherit,
            config: None,
        };

        let root = make_upstream(root_id, "openai", Some(root_auth), None, None, vec![]);
        let child = make_upstream(
            child_id,
            "openai",
            Some(child_auth.clone()),
            None,
            None,
            vec![],
        );

        let effective = compute_effective_config(&[root, child], None).unwrap();
        assert_eq!(effective.auth.unwrap().plugin_type, "oauth2");
    }

    #[test]
    fn effective_config_auth_enforce_ancestor_wins() {
        let root_id = Uuid::new_v4();
        let child_id = Uuid::new_v4();

        let root_auth = AuthConfig {
            plugin_type: "apikey".into(),
            sharing: SharingMode::Enforce,
            config: None,
        };
        let child_auth = AuthConfig {
            plugin_type: "oauth2".into(),
            sharing: SharingMode::Inherit,
            config: None,
        };

        let root = make_upstream(root_id, "openai", Some(root_auth), None, None, vec![]);
        let child = make_upstream(child_id, "openai", Some(child_auth), None, None, vec![]);

        let effective = compute_effective_config(&[root, child], None).unwrap();
        // Ancestor enforce wins — apikey stays.
        assert_eq!(effective.auth.unwrap().plugin_type, "apikey");
    }

    #[test]
    fn effective_config_rate_limit_min_wins() {
        let root_id = Uuid::new_v4();
        let child_id = Uuid::new_v4();

        let root_rl = make_rate_limit(SharingMode::Enforce, 100, Window::Minute);
        let child_rl = make_rate_limit(SharingMode::Inherit, 200, Window::Minute);

        let root = make_upstream(root_id, "openai", None, Some(root_rl), None, vec![]);
        let child = make_upstream(child_id, "openai", None, Some(child_rl), None, vec![]);

        let effective = compute_effective_config(&[root, child], None).unwrap();
        // min(100/min, 200/min) = 100/min
        assert_eq!(effective.rate_limit.unwrap().sustained.rate, 100);
    }

    #[test]
    fn effective_config_rate_limit_descendant_stricter() {
        let root_id = Uuid::new_v4();
        let child_id = Uuid::new_v4();

        let root_rl = make_rate_limit(SharingMode::Inherit, 1000, Window::Minute);
        let child_rl = make_rate_limit(SharingMode::Inherit, 50, Window::Minute);

        let root = make_upstream(root_id, "openai", None, Some(root_rl), None, vec![]);
        let child = make_upstream(child_id, "openai", None, Some(child_rl), None, vec![]);

        let effective = compute_effective_config(&[root, child], None).unwrap();
        assert_eq!(effective.rate_limit.unwrap().sustained.rate, 50);
    }

    #[test]
    fn effective_config_plugins_concatenation() {
        let root_id = Uuid::new_v4();
        let child_id = Uuid::new_v4();

        let root_plugins = PluginsConfig {
            sharing: SharingMode::Inherit,
            items: vec![
                PluginBinding {
                    plugin_ref: "plugin-a".into(),
                    config: HashMap::new(),
                },
                PluginBinding {
                    plugin_ref: "plugin-b".into(),
                    config: HashMap::new(),
                },
            ],
        };
        let child_plugins = PluginsConfig {
            sharing: SharingMode::Inherit,
            items: vec![
                PluginBinding {
                    plugin_ref: "plugin-b".into(),
                    config: HashMap::new(),
                },
                PluginBinding {
                    plugin_ref: "plugin-c".into(),
                    config: HashMap::new(),
                },
            ],
        };

        let root = make_upstream(root_id, "openai", None, None, Some(root_plugins), vec![]);
        let child = make_upstream(child_id, "openai", None, None, Some(child_plugins), vec![]);

        let effective = compute_effective_config(&[root, child], None).unwrap();
        let items = effective.plugins.unwrap().items;
        // ancestor + descendant (dedup): [a, b, c]
        assert_eq!(
            items
                .iter()
                .map(|b| b.plugin_ref.as_str())
                .collect::<Vec<_>>(),
            vec!["plugin-a", "plugin-b", "plugin-c"]
        );
    }

    #[test]
    fn effective_config_enforced_plugins_cannot_be_removed() {
        let root_id = Uuid::new_v4();
        let child_id = Uuid::new_v4();

        let root_plugins = PluginsConfig {
            sharing: SharingMode::Enforce,
            items: vec![PluginBinding {
                plugin_ref: "required-plugin".into(),
                config: HashMap::new(),
            }],
        };
        let child_plugins = PluginsConfig {
            sharing: SharingMode::Enforce,
            items: vec![PluginBinding {
                plugin_ref: "extra-plugin".into(),
                config: HashMap::new(),
            }],
        };

        let root = make_upstream(root_id, "openai", None, None, Some(root_plugins), vec![]);
        let child = make_upstream(child_id, "openai", None, None, Some(child_plugins), vec![]);

        let effective = compute_effective_config(&[root, child], None).unwrap();
        let items = effective.plugins.unwrap().items;
        // Enforced plugins remain: required-plugin + extra-plugin.
        assert!(items.iter().any(|b| b.plugin_ref == "required-plugin"));
        assert!(items.iter().any(|b| b.plugin_ref == "extra-plugin"));
    }

    #[test]
    fn effective_config_tags_union() {
        let root_id = Uuid::new_v4();
        let child_id = Uuid::new_v4();

        let root = make_upstream(
            root_id,
            "openai",
            None,
            None,
            None,
            vec!["env:prod".into(), "team:platform".into()],
        );
        let child = make_upstream(
            child_id,
            "openai",
            None,
            None,
            None,
            vec!["team:platform".into(), "region:us".into()],
        );

        let effective = compute_effective_config(&[root, child], None).unwrap();
        assert!(effective.tags.contains(&"env:prod".to_string()));
        assert!(effective.tags.contains(&"team:platform".to_string()));
        assert!(effective.tags.contains(&"region:us".to_string()));
        assert_eq!(effective.tags.len(), 3);
    }

    #[test]
    fn effective_config_route_rate_limit_applies_min() {
        let t = Uuid::new_v4();
        let upstream_rl = make_rate_limit(SharingMode::Inherit, 100, Window::Minute);
        let u = make_upstream(t, "openai", None, Some(upstream_rl), None, vec![]);

        let route = Route {
            id: Uuid::new_v4(),
            tenant_id: t,
            upstream_id: u.id,
            match_rules: MatchRules {
                http: None,
                grpc: None,
            },
            plugins: None,
            rate_limit: Some(make_rate_limit(SharingMode::Inherit, 50, Window::Minute)),
            tags: vec![],
            priority: 0,
            enabled: true,
        };

        let effective = compute_effective_config(&[u], Some(&route)).unwrap();
        // min(100/min, 50/min) = 50/min
        assert_eq!(effective.rate_limit.unwrap().sustained.rate, 50);
    }

    #[test]
    fn effective_config_three_layer_merge() {
        let root_id = Uuid::new_v4();
        let parent_id = Uuid::new_v4();
        let child_id = Uuid::new_v4();

        let root = make_upstream(
            root_id,
            "openai",
            Some(AuthConfig {
                plugin_type: "apikey".into(),
                sharing: SharingMode::Enforce,
                config: None,
            }),
            Some(make_rate_limit(SharingMode::Enforce, 1000, Window::Minute)),
            Some(PluginsConfig {
                sharing: SharingMode::Enforce,
                items: vec![PluginBinding {
                    plugin_ref: "audit-log".into(),
                    config: HashMap::new(),
                }],
            }),
            vec!["env:prod".into()],
        );
        let parent = make_upstream(
            parent_id,
            "openai",
            Some(AuthConfig {
                plugin_type: "oauth2".into(),
                sharing: SharingMode::Inherit,
                config: None,
            }),
            Some(make_rate_limit(SharingMode::Inherit, 500, Window::Minute)),
            Some(PluginsConfig {
                sharing: SharingMode::Inherit,
                items: vec![PluginBinding {
                    plugin_ref: "rate-guard".into(),
                    config: HashMap::new(),
                }],
            }),
            vec!["team:partner".into()],
        );
        let child = make_upstream(
            child_id,
            "openai",
            None,
            Some(make_rate_limit(SharingMode::Inherit, 200, Window::Minute)),
            Some(PluginsConfig {
                sharing: SharingMode::Inherit,
                items: vec![PluginBinding {
                    plugin_ref: "transform-x".into(),
                    config: HashMap::new(),
                }],
            }),
            vec!["region:us".into()],
        );

        let child_id_val = child.id;
        let effective = compute_effective_config(&[root, parent, child], None).unwrap();

        // Auth: root enforced → apikey wins even though parent set oauth2.
        assert_eq!(effective.auth.unwrap().plugin_type, "apikey");

        // Rate limit: min(1000, 500, 200) = 200/min.
        assert_eq!(effective.rate_limit.unwrap().sustained.rate, 200);

        // Plugins: enforced audit-log + rate-guard + transform-x.
        let items = effective.plugins.unwrap().items;
        assert!(items.iter().any(|b| b.plugin_ref == "audit-log"));
        assert!(items.iter().any(|b| b.plugin_ref == "rate-guard"));
        assert!(items.iter().any(|b| b.plugin_ref == "transform-x"));

        // Tags: union of all three.
        assert!(effective.tags.contains(&"env:prod".to_string()));
        assert!(effective.tags.contains(&"team:partner".to_string()));
        assert!(effective.tags.contains(&"region:us".to_string()));

        // Identity: uses child's id/tenant.
        assert_eq!(effective.id, child_id_val);
        assert_eq!(effective.tenant_id, child_id);
    }

    // -- Ancestor bind validation tests --

    #[tokio::test]
    async fn bind_rejects_auth_override_on_enforce() {
        let root = Uuid::new_v4();
        let child = Uuid::new_v4();
        let resolver =
            MockTenantResolverClient::with_hierarchy(vec![TenantId(root), TenantId(child)]);
        let svc = make_service_with_resolver(resolver);

        // Create upstream in root with auth sharing = enforce.
        let root_ctx = test_ctx(root);
        let mut root_req = make_create_upstream_hostname();
        root_req.auth = Some(AuthConfig {
            plugin_type: "apikey".into(),
            sharing: SharingMode::Enforce,
            config: None,
        });
        svc.create_upstream(&root_ctx, root_req).await.unwrap();

        // Child tries to create upstream with same host (same derived alias) AND auth override.
        let child_ctx = test_ctx(child);
        let mut child_req = make_create_upstream_hostname();
        child_req.auth = Some(AuthConfig {
            plugin_type: "oauth2".into(),
            sharing: SharingMode::Inherit,
            config: None,
        });
        let err = svc
            .create_upstream(&child_ctx, child_req)
            .await
            .unwrap_err();
        match err {
            DomainError::Validation { detail, .. } => {
                assert!(
                    detail.contains("enforce"),
                    "expected enforce error, got: {detail}"
                );
            }
            _ => panic!("expected Validation error, got: {err:?}"),
        }
    }

    #[tokio::test]
    async fn bind_rejects_rate_limit_override_on_private() {
        let root = Uuid::new_v4();
        let child = Uuid::new_v4();
        let resolver =
            MockTenantResolverClient::with_hierarchy(vec![TenantId(root), TenantId(child)]);
        let svc = make_service_with_resolver(resolver);

        // Create upstream in root with rate_limit sharing = private.
        let root_ctx = test_ctx(root);
        let mut root_req = make_create_upstream_hostname();
        root_req.rate_limit = Some(make_rate_limit(SharingMode::Private, 100, Window::Minute));
        // Need at least one non-private field so root upstream is visible.
        root_req.auth = Some(AuthConfig {
            plugin_type: "noop".into(),
            sharing: SharingMode::Inherit,
            config: None,
        });
        svc.create_upstream(&root_ctx, root_req).await.unwrap();

        // Child tries to override rate_limit on private ancestor field.
        let child_ctx = test_ctx(child);
        let mut child_req = make_create_upstream_hostname();
        child_req.rate_limit = Some(make_rate_limit(SharingMode::Inherit, 50, Window::Minute));
        let err = svc
            .create_upstream(&child_ctx, child_req)
            .await
            .unwrap_err();
        match err {
            DomainError::Validation { detail, .. } => {
                assert!(
                    detail.contains("private"),
                    "expected private error, got: {detail}"
                );
            }
            _ => panic!("expected Validation error, got: {err:?}"),
        }
    }

    #[tokio::test]
    async fn bind_allows_inherit_override_with_permissions() {
        let root = Uuid::new_v4();
        let child = Uuid::new_v4();
        let resolver =
            MockTenantResolverClient::with_hierarchy(vec![TenantId(root), TenantId(child)]);
        let svc = make_service_with_resolver(resolver);

        // Create upstream in root with inherit sharing.
        let root_ctx = test_ctx(root);
        let mut root_req = make_create_upstream_hostname();
        root_req.auth = Some(AuthConfig {
            plugin_type: "apikey".into(),
            sharing: SharingMode::Inherit,
            config: None,
        });
        svc.create_upstream(&root_ctx, root_req).await.unwrap();

        // Child creates upstream with same host (same derived alias) and overrides auth.
        // With allow-all enforcer, bind + override_auth permissions pass.
        let child_ctx = test_ctx(child);
        let mut child_req = make_create_upstream_hostname();
        child_req.auth = Some(AuthConfig {
            plugin_type: "oauth2".into(),
            sharing: SharingMode::Inherit,
            config: None,
        });
        let child_upstream = svc.create_upstream(&child_ctx, child_req).await.unwrap();
        assert_eq!(child_upstream.alias, "api.openai.com");
        assert_eq!(child_upstream.auth.unwrap().plugin_type, "oauth2");
    }

    #[tokio::test]
    async fn bind_no_ancestor_match_creates_normally() {
        let root = Uuid::new_v4();
        let child = Uuid::new_v4();
        let resolver =
            MockTenantResolverClient::with_hierarchy(vec![TenantId(root), TenantId(child)]);
        let svc = make_service_with_resolver(resolver);

        // No upstream in root. Child creates fresh upstream — no permission checks needed.
        let child_ctx = test_ctx(child);
        let child_upstream = svc
            .create_upstream(&child_ctx, make_create_upstream_ip("fresh-alias"))
            .await
            .unwrap();
        assert_eq!(child_upstream.alias, "fresh-alias");
    }

    // -- Secret ref validation tests --

    fn auth_with_secret_ref(secret_ref: &str) -> AuthConfig {
        let mut config = std::collections::HashMap::new();
        config.insert("header".into(), "authorization".into());
        config.insert("secret_ref".into(), secret_ref.into());
        AuthConfig {
            plugin_type: "apikey".into(),
            sharing: SharingMode::Inherit,
            config: Some(config),
        }
    }

    #[tokio::test]
    async fn bind_rejects_inaccessible_secret_ref() {
        let root = Uuid::new_v4();
        let child = Uuid::new_v4();
        let resolver =
            MockTenantResolverClient::with_hierarchy(vec![TenantId(root), TenantId(child)]);
        // No secrets in credstore.
        let svc = make_service_with_resolver_and_creds(resolver, vec![]);

        // Root upstream with auth inherit.
        let root_ctx = test_ctx(root);
        let mut root_req = make_create_upstream_hostname();
        root_req.auth = Some(AuthConfig {
            plugin_type: "apikey".into(),
            sharing: SharingMode::Inherit,
            config: None,
        });
        svc.create_upstream(&root_ctx, root_req).await.unwrap();

        // Child tries to bind with a secret_ref the credstore doesn't have.
        let child_ctx = test_ctx(child);
        let mut child_req = make_create_upstream_hostname();
        child_req.auth = Some(auth_with_secret_ref("cred://missing-key"));
        let err = svc
            .create_upstream(&child_ctx, child_req)
            .await
            .unwrap_err();
        match err {
            DomainError::Validation { detail, .. } => {
                assert!(
                    detail.contains("not accessible"),
                    "expected 'not accessible' error, got: {detail}"
                );
            }
            _ => panic!("expected Validation error, got: {err:?}"),
        }
    }

    #[tokio::test]
    async fn bind_allows_accessible_secret_ref() {
        let root = Uuid::new_v4();
        let child = Uuid::new_v4();
        let resolver =
            MockTenantResolverClient::with_hierarchy(vec![TenantId(root), TenantId(child)]);
        let svc = make_service_with_resolver_and_creds(
            resolver,
            vec![("my-key".into(), "secret-value".into())],
        );

        // Root upstream with auth inherit.
        let root_ctx = test_ctx(root);
        let mut root_req = make_create_upstream_hostname();
        root_req.auth = Some(AuthConfig {
            plugin_type: "apikey".into(),
            sharing: SharingMode::Inherit,
            config: None,
        });
        svc.create_upstream(&root_ctx, root_req).await.unwrap();

        // Child binds with accessible secret_ref.
        let child_ctx = test_ctx(child);
        let mut child_req = make_create_upstream_hostname();
        child_req.auth = Some(auth_with_secret_ref("cred://my-key"));
        let child_upstream = svc.create_upstream(&child_ctx, child_req).await.unwrap();
        assert_eq!(child_upstream.alias, "api.openai.com");
    }

    // -- Update upstream bind validation tests --

    #[tokio::test]
    async fn update_rejects_auth_override_on_enforce() {
        let root = Uuid::new_v4();
        let child = Uuid::new_v4();
        let resolver =
            MockTenantResolverClient::with_hierarchy(vec![TenantId(root), TenantId(child)]);
        let svc = make_service_with_resolver(resolver);

        // Root upstream with auth enforce.
        let root_ctx = test_ctx(root);
        let mut root_req = make_create_upstream_hostname();
        root_req.auth = Some(AuthConfig {
            plugin_type: "apikey".into(),
            sharing: SharingMode::Enforce,
            config: None,
        });
        svc.create_upstream(&root_ctx, root_req).await.unwrap();

        // Child creates upstream with same host (same derived alias, no auth override on create).
        let child_ctx = test_ctx(child);
        let child_upstream = svc
            .create_upstream(&child_ctx, make_create_upstream_hostname())
            .await
            .unwrap();

        // Child tries to update auth — should fail because ancestor is enforce.
        let err = svc
            .update_upstream(
                &child_ctx,
                child_upstream.id,
                UpdateUpstreamRequest {
                    auth: Some(AuthConfig {
                        plugin_type: "oauth2".into(),
                        sharing: SharingMode::Inherit,
                        config: None,
                    }),
                    ..Default::default()
                },
            )
            .await
            .unwrap_err();
        match err {
            DomainError::Validation { detail, .. } => {
                assert!(
                    detail.contains("enforce"),
                    "expected enforce error, got: {detail}"
                );
            }
            _ => panic!("expected Validation error, got: {err:?}"),
        }
    }

    #[tokio::test]
    async fn update_alias_to_ancestor_requires_bind() {
        let root = Uuid::new_v4();
        let child = Uuid::new_v4();
        let resolver =
            MockTenantResolverClient::with_hierarchy(vec![TenantId(root), TenantId(child)]);
        let svc = make_service_with_resolver(resolver);

        // Root upstream with inherit sharing (IP-based for explicit alias control).
        let root_ctx = test_ctx(root);
        let mut root_req = make_create_upstream_ip("openai");
        root_req.auth = Some(AuthConfig {
            plugin_type: "apikey".into(),
            sharing: SharingMode::Inherit,
            config: None,
        });
        svc.create_upstream(&root_ctx, root_req).await.unwrap();

        // Child creates upstream with different alias (IP-based).
        let child_ctx = test_ctx(child);
        let child_upstream = svc
            .create_upstream(&child_ctx, make_create_upstream_ip("other"))
            .await
            .unwrap();

        // Child updates alias to match ancestor — with allow-all enforcer this passes.
        let updated = svc
            .update_upstream(
                &child_ctx,
                child_upstream.id,
                UpdateUpstreamRequest {
                    alias: Some("openai".into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(updated.alias, "openai");
    }

    #[tokio::test]
    async fn update_alias_only_validates_existing_overrides_against_ancestor_enforce() {
        let root = Uuid::new_v4();
        let child = Uuid::new_v4();
        let resolver =
            MockTenantResolverClient::with_hierarchy(vec![TenantId(root), TenantId(child)]);
        let svc = make_service_with_resolver(resolver);

        // Root upstream with auth enforce (IP-based for explicit alias control).
        let root_ctx = test_ctx(root);
        let mut root_req = make_create_upstream_ip("openai");
        root_req.auth = Some(AuthConfig {
            plugin_type: "apikey".into(),
            sharing: SharingMode::Enforce,
            config: None,
        });
        svc.create_upstream(&root_ctx, root_req).await.unwrap();

        // Child creates upstream with a different alias but with auth already set (IP-based).
        let child_ctx = test_ctx(child);
        let mut child_req = make_create_upstream_ip("other");
        child_req.auth = Some(AuthConfig {
            plugin_type: "oauth2".into(),
            sharing: SharingMode::Inherit,
            config: None,
        });
        let child_upstream = svc.create_upstream(&child_ctx, child_req).await.unwrap();

        // Alias-only update to match ancestor — must fail because the child's
        // existing auth override conflicts with the ancestor's enforce mode.
        let err = svc
            .update_upstream(
                &child_ctx,
                child_upstream.id,
                UpdateUpstreamRequest {
                    alias: Some("openai".into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap_err();
        match err {
            DomainError::Validation { detail, .. } => {
                assert!(
                    detail.contains("enforce"),
                    "expected enforce error, got: {detail}"
                );
            }
            _ => panic!("expected Validation error, got: {err:?}"),
        }
    }

    #[tokio::test]
    async fn update_no_ancestor_match_succeeds() {
        let root = Uuid::new_v4();
        let child = Uuid::new_v4();
        let resolver =
            MockTenantResolverClient::with_hierarchy(vec![TenantId(root), TenantId(child)]);
        let svc = make_service_with_resolver(resolver);

        // Child creates upstream (IP-based for explicit alias).
        let child_ctx = test_ctx(child);
        let child_upstream = svc
            .create_upstream(&child_ctx, make_create_upstream_ip("my-svc"))
            .await
            .unwrap();

        // Update auth — no ancestor match, should succeed without permission checks.
        let updated = svc
            .update_upstream(
                &child_ctx,
                child_upstream.id,
                UpdateUpstreamRequest {
                    auth: Some(AuthConfig {
                        plugin_type: "oauth2".into(),
                        sharing: SharingMode::Inherit,
                        config: None,
                    }),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(updated.auth.unwrap().plugin_type, "oauth2");
    }

    // -- resolve_proxy_target tests --

    #[tokio::test]
    async fn proxy_target_resolves_route_from_ancestor_upstream() {
        use crate::domain::model::{AuthConfig, SharingMode};

        let root = Uuid::new_v4();
        let child = Uuid::new_v4();
        let resolver =
            MockTenantResolverClient::with_hierarchy(vec![TenantId(root), TenantId(child)]);
        let svc = make_service_with_resolver(resolver);

        // Root creates upstream with auth inherit (hostname-based, derived alias).
        let root_ctx = test_ctx(root);
        let mut root_req = make_create_upstream_hostname();
        root_req.auth = Some(AuthConfig {
            plugin_type: "apikey".into(),
            sharing: SharingMode::Inherit,
            config: None,
        });
        let root_upstream = svc.create_upstream(&root_ctx, root_req).await.unwrap();

        // Root creates a route on that upstream.
        let route_req = CreateRouteRequest {
            upstream_id: root_upstream.id,
            match_rules: MatchRules {
                http: Some(HttpMatch {
                    path: "/v1/chat".into(),
                    methods: vec![HttpMethod::Post],
                    query_allowlist: vec![],
                    path_suffix_mode: PathSuffixMode::default(),
                }),
                grpc: None,
            },
            plugins: None,
            rate_limit: None,
            tags: vec![],
            priority: 0,
            enabled: true,
        };
        let root_route = svc.create_route(&root_ctx, route_req).await.unwrap();

        // Child creates upstream with same host (same derived alias, bind to ancestor).
        let child_ctx = test_ctx(child);
        let _child_upstream = svc
            .create_upstream(&child_ctx, make_create_upstream_hostname())
            .await
            .unwrap();

        // Child resolves proxy target — should find the route defined on
        // the root's upstream ID, not the child's.
        let (effective, route) = svc
            .resolve_proxy_target(&child_ctx, "api.openai.com", "POST", "/v1/chat")
            .await
            .unwrap();

        assert_eq!(route.id, root_route.id);
        assert_eq!(effective.alias, "api.openai.com");
    }

    #[tokio::test]
    async fn proxy_target_prefers_child_route_over_ancestor() {
        use crate::domain::model::{AuthConfig, SharingMode};

        let root = Uuid::new_v4();
        let child = Uuid::new_v4();
        let resolver =
            MockTenantResolverClient::with_hierarchy(vec![TenantId(root), TenantId(child)]);
        let svc = make_service_with_resolver(resolver);

        // Root creates upstream with auth inherit (hostname-based, derived alias).
        let root_ctx = test_ctx(root);
        let mut root_req = make_create_upstream_hostname();
        root_req.auth = Some(AuthConfig {
            plugin_type: "apikey".into(),
            sharing: SharingMode::Inherit,
            config: None,
        });
        let root_upstream = svc.create_upstream(&root_ctx, root_req).await.unwrap();

        // Root creates a route.
        let root_route_req = CreateRouteRequest {
            upstream_id: root_upstream.id,
            match_rules: MatchRules {
                http: Some(HttpMatch {
                    path: "/v1/chat".into(),
                    methods: vec![HttpMethod::Post],
                    query_allowlist: vec![],
                    path_suffix_mode: PathSuffixMode::default(),
                }),
                grpc: None,
            },
            plugins: None,
            rate_limit: None,
            tags: vec![],
            priority: 0,
            enabled: true,
        };
        svc.create_route(&root_ctx, root_route_req).await.unwrap();

        // Child creates upstream with same host (same derived alias).
        let child_ctx = test_ctx(child);
        let child_upstream = svc
            .create_upstream(&child_ctx, make_create_upstream_hostname())
            .await
            .unwrap();

        // Child creates its own route on its own upstream.
        let child_route_req = CreateRouteRequest {
            upstream_id: child_upstream.id,
            match_rules: MatchRules {
                http: Some(HttpMatch {
                    path: "/v1/chat".into(),
                    methods: vec![HttpMethod::Post],
                    query_allowlist: vec![],
                    path_suffix_mode: PathSuffixMode::default(),
                }),
                grpc: None,
            },
            plugins: None,
            rate_limit: None,
            tags: vec![],
            priority: 0,
            enabled: true,
        };
        let child_route = svc.create_route(&child_ctx, child_route_req).await.unwrap();

        // Child resolves — should prefer its own route (child upstream ID checked first).
        let (_effective, route) = svc
            .resolve_proxy_target(&child_ctx, "api.openai.com", "POST", "/v1/chat")
            .await
            .unwrap();

        assert_eq!(route.id, child_route.id);
    }

    // -- Private sharing (no enforce ancestor) tests --

    #[test]
    fn merge_auth_private_replaces_when_ancestor_not_enforced() {
        let root_id = Uuid::new_v4();
        let child_id = Uuid::new_v4();

        let root_auth = AuthConfig {
            plugin_type: "apikey".into(),
            sharing: SharingMode::Inherit,
            config: None,
        };
        let child_auth = AuthConfig {
            plugin_type: "oauth2".into(),
            sharing: SharingMode::Private,
            config: None,
        };

        let root = make_upstream(root_id, "openai", Some(root_auth), None, None, vec![]);
        let child = make_upstream(child_id, "openai", Some(child_auth), None, None, vec![]);

        let effective = compute_effective_config(&[root, child], None).unwrap();
        // Ancestor is Inherit (not Enforce) — Private descendant replaces.
        let auth = effective.auth.unwrap();
        assert_eq!(auth.plugin_type, "oauth2");
        assert_eq!(auth.sharing, SharingMode::Private);
    }

    #[test]
    fn route_private_plugins_are_skipped() {
        let t = Uuid::new_v4();
        let upstream_plugins = PluginsConfig {
            sharing: SharingMode::Inherit,
            items: vec![PluginBinding {
                plugin_ref: "upstream-plugin".into(),
                config: HashMap::new(),
            }],
        };
        let u = make_upstream(t, "openai", None, None, Some(upstream_plugins), vec![]);

        let route = Route {
            id: Uuid::new_v4(),
            tenant_id: t,
            upstream_id: u.id,
            match_rules: MatchRules {
                http: None,
                grpc: None,
            },
            plugins: Some(PluginsConfig {
                sharing: SharingMode::Private,
                items: vec![PluginBinding {
                    plugin_ref: "route-plugin".into(),
                    config: HashMap::new(),
                }],
            }),
            rate_limit: None,
            tags: vec![],
            priority: 0,
            enabled: true,
        };

        let effective = compute_effective_config(&[u], Some(&route)).unwrap();
        let items = effective.plugins.unwrap().items;
        // Route plugins with Private sharing are skipped — only upstream plugins remain.
        assert_eq!(
            items
                .iter()
                .map(|b| b.plugin_ref.as_str())
                .collect::<Vec<_>>(),
            vec!["upstream-plugin"]
        );
    }

    #[test]
    fn route_private_rate_limit_is_skipped() {
        let t = Uuid::new_v4();
        let upstream_rl = make_rate_limit(SharingMode::Inherit, 100, Window::Minute);
        let u = make_upstream(t, "openai", None, Some(upstream_rl), None, vec![]);

        let route = Route {
            id: Uuid::new_v4(),
            tenant_id: t,
            upstream_id: u.id,
            match_rules: MatchRules {
                http: None,
                grpc: None,
            },
            plugins: None,
            rate_limit: Some(make_rate_limit(SharingMode::Private, 10, Window::Minute)),
            tags: vec![],
            priority: 0,
            enabled: true,
        };

        let effective = compute_effective_config(&[u], Some(&route)).unwrap();
        // Route rate_limit with Private sharing is skipped — upstream rate stays.
        assert_eq!(effective.rate_limit.unwrap().sustained.rate, 100);
    }

    // -- Defense-in-depth: enforce vs private merge tests --

    #[test]
    fn merge_rate_limit_private_cannot_bypass_enforced_ancestor() {
        let root_id = Uuid::new_v4();
        let child_id = Uuid::new_v4();

        let root_rl = make_rate_limit(SharingMode::Enforce, 100, Window::Minute);
        let child_rl = make_rate_limit(SharingMode::Private, 9999, Window::Minute);

        let root = make_upstream(root_id, "openai", None, Some(root_rl), None, vec![]);
        let child = make_upstream(child_id, "openai", None, Some(child_rl), None, vec![]);

        let effective = compute_effective_config(&[root, child], None).unwrap();
        // Enforced ancestor rate (100/min) must still constrain even though
        // descendant declared Private with a much higher rate.
        assert_eq!(effective.rate_limit.unwrap().sustained.rate, 100);
    }

    #[test]
    fn merge_auth_private_cannot_bypass_enforced_ancestor() {
        let root_id = Uuid::new_v4();
        let child_id = Uuid::new_v4();

        let root_auth = AuthConfig {
            plugin_type: "apikey".into(),
            sharing: SharingMode::Enforce,
            config: None,
        };
        let child_auth = AuthConfig {
            plugin_type: "oauth2".into(),
            sharing: SharingMode::Private,
            config: None,
        };

        let root = make_upstream(root_id, "openai", Some(root_auth), None, None, vec![]);
        let child = make_upstream(child_id, "openai", Some(child_auth), None, None, vec![]);

        let effective = compute_effective_config(&[root, child], None).unwrap();
        // Enforced ancestor auth (apikey) must survive even though
        // descendant declared Private with oauth2.
        assert_eq!(effective.auth.unwrap().plugin_type, "apikey");
    }

    #[test]
    fn merge_plugins_private_cannot_drop_enforced_ancestor_plugins() {
        let root_id = Uuid::new_v4();
        let child_id = Uuid::new_v4();

        let root_plugins = PluginsConfig {
            sharing: SharingMode::Enforce,
            items: vec![PluginBinding {
                plugin_ref: "audit-log".into(),
                config: HashMap::new(),
            }],
        };
        let child_plugins = PluginsConfig {
            sharing: SharingMode::Private,
            items: vec![PluginBinding {
                plugin_ref: "my-plugin".into(),
                config: HashMap::new(),
            }],
        };

        let root = make_upstream(root_id, "openai", None, None, Some(root_plugins), vec![]);
        let child = make_upstream(child_id, "openai", None, None, Some(child_plugins), vec![]);

        let effective = compute_effective_config(&[root, child], None).unwrap();
        let items = effective.plugins.unwrap().items;
        // Enforced "audit-log" must survive even though descendant set Private.
        assert!(items.iter().any(|b| b.plugin_ref == "audit-log"));
        assert!(items.iter().any(|b| b.plugin_ref == "my-plugin"));
    }

    // -- Alias enforcement tests --

    #[test]
    fn compute_derived_alias_single_hostname_standard_port() {
        let eps = vec![Endpoint {
            scheme: Scheme::Https,
            host: "api.openai.com".into(),
            port: 443,
        }];
        assert_eq!(compute_derived_alias(&eps), Some("api.openai.com".into()));
    }

    #[test]
    fn compute_derived_alias_single_hostname_non_standard_port() {
        let eps = vec![Endpoint {
            scheme: Scheme::Https,
            host: "api.openai.com".into(),
            port: 8443,
        }];
        assert_eq!(
            compute_derived_alias(&eps),
            Some("api.openai.com:8443".into())
        );
    }

    #[test]
    fn compute_derived_alias_http_standard_port() {
        let eps = vec![Endpoint {
            scheme: Scheme::Http,
            host: "api.example.com".into(),
            port: 80,
        }];
        assert_eq!(compute_derived_alias(&eps), Some("api.example.com".into()));
    }

    #[test]
    fn compute_derived_alias_grpc_standard_port() {
        let eps = vec![Endpoint {
            scheme: Scheme::Grpc,
            host: "grpc.example.com".into(),
            port: 443,
        }];
        assert_eq!(compute_derived_alias(&eps), Some("grpc.example.com".into()));
    }

    #[test]
    fn compute_derived_alias_multi_host_common_suffix() {
        let eps = vec![
            Endpoint {
                scheme: Scheme::Https,
                host: "us.vendor.com".into(),
                port: 443,
            },
            Endpoint {
                scheme: Scheme::Https,
                host: "eu.vendor.com".into(),
                port: 443,
            },
        ];
        assert_eq!(compute_derived_alias(&eps), Some("vendor.com".into()));
    }

    #[test]
    fn compute_derived_alias_multi_host_common_suffix_non_standard_port() {
        let eps = vec![
            Endpoint {
                scheme: Scheme::Https,
                host: "us.vendor.com".into(),
                port: 8443,
            },
            Endpoint {
                scheme: Scheme::Https,
                host: "eu.vendor.com".into(),
                port: 8443,
            },
        ];
        assert_eq!(compute_derived_alias(&eps), Some("vendor.com:8443".into()));
    }

    #[test]
    fn compute_derived_alias_multi_host_deeper_common_suffix() {
        let eps = vec![
            Endpoint {
                scheme: Scheme::Https,
                host: "a.b.vendor.com".into(),
                port: 443,
            },
            Endpoint {
                scheme: Scheme::Https,
                host: "c.b.vendor.com".into(),
                port: 443,
            },
        ];
        assert_eq!(compute_derived_alias(&eps), Some("b.vendor.com".into()));
    }

    #[test]
    fn compute_derived_alias_multi_host_no_common_suffix() {
        let eps = vec![
            Endpoint {
                scheme: Scheme::Https,
                host: "us.foo.com".into(),
                port: 443,
            },
            Endpoint {
                scheme: Scheme::Https,
                host: "eu.bar.com".into(),
                port: 443,
            },
        ];
        // Only 1 common label ("com") — minimum is 2.
        assert_eq!(compute_derived_alias(&eps), None);
    }

    #[test]
    fn compute_derived_alias_multi_host_identical_treated_as_single() {
        let eps = vec![
            Endpoint {
                scheme: Scheme::Https,
                host: "api.vendor.com".into(),
                port: 443,
            },
            Endpoint {
                scheme: Scheme::Https,
                host: "api.vendor.com".into(),
                port: 443,
            },
        ];
        assert_eq!(compute_derived_alias(&eps), Some("api.vendor.com".into()));
    }

    #[test]
    fn compute_derived_alias_ip_returns_none() {
        let eps = vec![Endpoint {
            scheme: Scheme::Https,
            host: "10.0.1.1".into(),
            port: 443,
        }];
        assert_eq!(compute_derived_alias(&eps), None);
    }

    #[test]
    fn compute_derived_alias_normalizes_case() {
        let eps = vec![Endpoint {
            scheme: Scheme::Https,
            host: "Api.OpenAI.COM".into(),
            port: 443,
        }];
        assert_eq!(compute_derived_alias(&eps), Some("api.openai.com".into()));
    }

    #[test]
    fn compute_derived_alias_strips_trailing_dot() {
        let eps = vec![Endpoint {
            scheme: Scheme::Https,
            host: "api.example.com.".into(),
            port: 443,
        }];
        assert_eq!(compute_derived_alias(&eps), Some("api.example.com".into()));
    }

    #[test]
    fn normalize_alias_lowercases() {
        assert_eq!(normalize_alias("My-Service"), "my-service");
    }

    #[test]
    fn normalize_alias_strips_trailing_dot() {
        assert_eq!(normalize_alias("api.example.com."), "api.example.com");
    }

    #[test]
    fn normalize_alias_strips_multiple_trailing_dots() {
        assert_eq!(normalize_alias("svc.."), "svc");
        assert_eq!(normalize_alias("svc..."), "svc");
    }

    #[test]
    fn normalized_host_strips_multiple_trailing_dots() {
        let ep = Endpoint {
            scheme: Scheme::Https,
            host: "Api.Example.COM..".into(),
            port: 443,
        };
        assert_eq!(ep.normalized_host(), "api.example.com");
    }

    #[test]
    fn enforce_alias_create_hostname_rejects_user_alias() {
        let eps = vec![Endpoint {
            scheme: Scheme::Https,
            host: "api.openai.com".into(),
            port: 443,
        }];
        let err = enforce_alias_create(Some("custom-alias"), &eps).unwrap_err();
        assert!(matches!(err, DomainError::Validation { .. }));
    }

    #[test]
    fn enforce_alias_create_hostname_tolerates_exact_match() {
        let eps = vec![Endpoint {
            scheme: Scheme::Https,
            host: "api.openai.com".into(),
            port: 443,
        }];
        let alias = enforce_alias_create(Some("api.openai.com"), &eps).unwrap();
        assert_eq!(alias, "api.openai.com");
    }

    #[test]
    fn enforce_alias_create_hostname_auto_derives() {
        let eps = vec![Endpoint {
            scheme: Scheme::Https,
            host: "api.openai.com".into(),
            port: 443,
        }];
        let alias = enforce_alias_create(None, &eps).unwrap();
        assert_eq!(alias, "api.openai.com");
    }

    #[test]
    fn enforce_alias_create_ip_requires_explicit() {
        let eps = vec![Endpoint {
            scheme: Scheme::Https,
            host: "10.0.1.1".into(),
            port: 443,
        }];
        let err = enforce_alias_create(None, &eps).unwrap_err();
        assert!(matches!(err, DomainError::Validation { .. }));
    }

    #[test]
    fn enforce_alias_create_ip_accepts_explicit() {
        let eps = vec![Endpoint {
            scheme: Scheme::Https,
            host: "10.0.1.1".into(),
            port: 443,
        }];
        let alias = enforce_alias_create(Some("my-backend"), &eps).unwrap();
        assert_eq!(alias, "my-backend");
    }

    #[test]
    fn enforce_alias_update_hostname_to_hostname_recomputes() {
        let old_eps = vec![Endpoint {
            scheme: Scheme::Https,
            host: "old.vendor.com".into(),
            port: 443,
        }];
        let new_eps = vec![Endpoint {
            scheme: Scheme::Https,
            host: "new.vendor.com".into(),
            port: 443,
        }];
        let alias = enforce_alias_update(None, &new_eps, "old.vendor.com", &old_eps).unwrap();
        assert_eq!(alias, "new.vendor.com");
    }

    #[test]
    fn enforce_alias_update_hostname_to_ip_requires_alias() {
        let old_eps = vec![Endpoint {
            scheme: Scheme::Https,
            host: "api.openai.com".into(),
            port: 443,
        }];
        let new_eps = vec![Endpoint {
            scheme: Scheme::Https,
            host: "10.0.1.1".into(),
            port: 443,
        }];
        let err = enforce_alias_update(None, &new_eps, "api.openai.com", &old_eps).unwrap_err();
        assert!(matches!(err, DomainError::Validation { .. }));
    }

    #[test]
    fn enforce_alias_update_hostname_to_ip_with_alias_succeeds() {
        let old_eps = vec![Endpoint {
            scheme: Scheme::Https,
            host: "api.openai.com".into(),
            port: 443,
        }];
        let new_eps = vec![Endpoint {
            scheme: Scheme::Https,
            host: "10.0.1.1".into(),
            port: 443,
        }];
        let alias =
            enforce_alias_update(Some("my-backend"), &new_eps, "api.openai.com", &old_eps).unwrap();
        assert_eq!(alias, "my-backend");
    }

    #[test]
    fn enforce_alias_update_ip_to_ip_retains_existing() {
        let old_eps = vec![Endpoint {
            scheme: Scheme::Https,
            host: "10.0.1.1".into(),
            port: 443,
        }];
        let new_eps = vec![Endpoint {
            scheme: Scheme::Https,
            host: "10.0.1.2".into(),
            port: 443,
        }];
        let alias = enforce_alias_update(None, &new_eps, "my-backend", &old_eps).unwrap();
        assert_eq!(alias, "my-backend");
    }

    #[test]
    fn enforce_alias_update_ip_to_hostname_recomputes() {
        let old_eps = vec![Endpoint {
            scheme: Scheme::Https,
            host: "10.0.1.1".into(),
            port: 443,
        }];
        let new_eps = vec![Endpoint {
            scheme: Scheme::Https,
            host: "api.openai.com".into(),
            port: 443,
        }];
        let alias = enforce_alias_update(None, &new_eps, "my-backend", &old_eps).unwrap();
        assert_eq!(alias, "api.openai.com");
    }

    #[tokio::test]
    async fn create_hostname_rejects_explicit_alias() {
        let svc = make_service();
        let tenant = Uuid::new_v4();
        let ctx = test_ctx(tenant);

        let mut req = make_create_upstream_hostname();
        req.alias = Some("custom".into());
        let err = svc.create_upstream(&ctx, req).await.unwrap_err();
        assert!(matches!(err, DomainError::Validation { .. }));
    }

    #[tokio::test]
    async fn create_ip_requires_explicit_alias() {
        let svc = make_service();
        let tenant = Uuid::new_v4();
        let ctx = test_ctx(tenant);

        let req = CreateUpstreamRequest {
            server: Server {
                endpoints: vec![Endpoint {
                    scheme: Scheme::Https,
                    host: "10.0.0.1".into(),
                    port: 443,
                }],
            },
            protocol: "gts.x.core.oagw.protocol.v1~x.core.oagw.http.v1".into(),
            alias: None,
            auth: None,
            headers: None,
            plugins: None,
            rate_limit: None,
            tags: vec![],
            enabled: true,
        };
        let err = svc.create_upstream(&ctx, req).await.unwrap_err();
        assert!(matches!(err, DomainError::Validation { .. }));
    }

    #[tokio::test]
    async fn update_hostname_rejects_alias_override() {
        let svc = make_service();
        let tenant = Uuid::new_v4();
        let ctx = test_ctx(tenant);

        let u = svc
            .create_upstream(&ctx, make_create_upstream_hostname())
            .await
            .unwrap();
        assert_eq!(u.alias, "api.openai.com");

        // Try to override alias on hostname-based upstream — should fail.
        let err = svc
            .update_upstream(
                &ctx,
                u.id,
                UpdateUpstreamRequest {
                    alias: Some("custom".into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap_err();
        assert!(matches!(err, DomainError::Validation { .. }));
    }

    #[tokio::test]
    async fn update_endpoints_recomputes_alias() {
        let svc = make_service();
        let tenant = Uuid::new_v4();
        let ctx = test_ctx(tenant);

        let u = svc
            .create_upstream(&ctx, make_create_upstream_hostname())
            .await
            .unwrap();
        assert_eq!(u.alias, "api.openai.com");

        // Update endpoints to a different host — alias should recompute.
        let updated = svc
            .update_upstream(
                &ctx,
                u.id,
                UpdateUpstreamRequest {
                    server: Some(Server {
                        endpoints: vec![Endpoint {
                            scheme: Scheme::Https,
                            host: "api.anthropic.com".into(),
                            port: 443,
                        }],
                    }),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(updated.alias, "api.anthropic.com");
    }

    #[tokio::test]
    async fn update_hostname_to_ip_without_alias_fails() {
        let svc = make_service();
        let tenant = Uuid::new_v4();
        let ctx = test_ctx(tenant);

        let u = svc
            .create_upstream(&ctx, make_create_upstream_hostname())
            .await
            .unwrap();

        // Switch to IP endpoints without providing alias.
        let err = svc
            .update_upstream(
                &ctx,
                u.id,
                UpdateUpstreamRequest {
                    server: Some(Server {
                        endpoints: vec![Endpoint {
                            scheme: Scheme::Https,
                            host: "10.0.0.1".into(),
                            port: 443,
                        }],
                    }),
                    ..Default::default()
                },
            )
            .await
            .unwrap_err();
        assert!(matches!(err, DomainError::Validation { .. }));
    }

    #[tokio::test]
    async fn update_hostname_to_ip_with_alias_succeeds() {
        let svc = make_service();
        let tenant = Uuid::new_v4();
        let ctx = test_ctx(tenant);

        let u = svc
            .create_upstream(&ctx, make_create_upstream_hostname())
            .await
            .unwrap();

        // Switch to IP endpoints with explicit alias.
        let updated = svc
            .update_upstream(
                &ctx,
                u.id,
                UpdateUpstreamRequest {
                    server: Some(Server {
                        endpoints: vec![Endpoint {
                            scheme: Scheme::Https,
                            host: "10.0.0.1".into(),
                            port: 443,
                        }],
                    }),
                    alias: Some("my-backend".into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(updated.alias, "my-backend");
    }

    #[tokio::test]
    async fn resolve_alias_case_insensitive() {
        let svc = make_service();
        let tenant = Uuid::new_v4();
        let ctx = test_ctx(tenant);

        let u = svc
            .create_upstream(&ctx, make_create_upstream_hostname())
            .await
            .unwrap();
        assert_eq!(u.alias, "api.openai.com");

        // Resolve with different casing — should still find the upstream.
        let chain = svc.build_tenant_chain(&ctx).await.unwrap();
        let (resolved, _) = svc
            .resolve_alias(&ctx, &chain, "Api.OpenAI.COM", None)
            .await
            .unwrap();
        assert_eq!(resolved.id, u.id);
    }

    #[tokio::test]
    async fn multi_endpoint_common_suffix_alias() {
        let svc = make_service();
        let tenant = Uuid::new_v4();
        let ctx = test_ctx(tenant);

        let req = CreateUpstreamRequest {
            server: Server {
                endpoints: vec![
                    Endpoint {
                        scheme: Scheme::Https,
                        host: "us.vendor.com".into(),
                        port: 443,
                    },
                    Endpoint {
                        scheme: Scheme::Https,
                        host: "eu.vendor.com".into(),
                        port: 443,
                    },
                ],
            },
            protocol: "gts.x.core.oagw.protocol.v1~x.core.oagw.http.v1".into(),
            alias: None,
            auth: None,
            headers: None,
            plugins: None,
            rate_limit: None,
            tags: vec![],
            enabled: true,
        };
        let u = svc.create_upstream(&ctx, req).await.unwrap();
        assert_eq!(u.alias, "vendor.com");
    }

    #[tokio::test]
    async fn multi_endpoint_same_suffix_different_ports_get_distinct_aliases() {
        let svc = make_service();
        let tenant = Uuid::new_v4();
        let ctx = test_ctx(tenant);

        // Pool A: non-standard port 8443.
        let req_a = CreateUpstreamRequest {
            server: Server {
                endpoints: vec![
                    Endpoint {
                        scheme: Scheme::Https,
                        host: "us.vendor.com".into(),
                        port: 8443,
                    },
                    Endpoint {
                        scheme: Scheme::Https,
                        host: "eu.vendor.com".into(),
                        port: 8443,
                    },
                ],
            },
            protocol: "gts.x.core.oagw.protocol.v1~x.core.oagw.http.v1".into(),
            alias: None,
            auth: None,
            headers: None,
            plugins: None,
            rate_limit: None,
            tags: vec![],
            enabled: true,
        };
        let u_a = svc.create_upstream(&ctx, req_a).await.unwrap();
        assert_eq!(u_a.alias, "vendor.com:8443");

        // Pool B: non-standard port 9443 — same hosts, different port.
        let req_b = CreateUpstreamRequest {
            server: Server {
                endpoints: vec![
                    Endpoint {
                        scheme: Scheme::Https,
                        host: "us.vendor.com".into(),
                        port: 9443,
                    },
                    Endpoint {
                        scheme: Scheme::Https,
                        host: "eu.vendor.com".into(),
                        port: 9443,
                    },
                ],
            },
            protocol: "gts.x.core.oagw.protocol.v1~x.core.oagw.http.v1".into(),
            alias: None,
            auth: None,
            headers: None,
            plugins: None,
            rate_limit: None,
            tags: vec![],
            enabled: true,
        };
        let u_b = svc.create_upstream(&ctx, req_b).await.unwrap();
        assert_eq!(u_b.alias, "vendor.com:9443");

        // Both stored separately — no 409 conflict.
        assert_ne!(u_a.id, u_b.id);
    }

    #[tokio::test]
    async fn multi_endpoint_public_suffix_requires_explicit_alias() {
        let svc = make_service();
        let tenant = Uuid::new_v4();
        let ctx = test_ctx(tenant);

        // Endpoints share only a public suffix ("co.uk"), not a registrable domain.
        // The system must NOT derive an alias from a bare public suffix.
        let req = CreateUpstreamRequest {
            server: Server {
                endpoints: vec![
                    Endpoint {
                        scheme: Scheme::Https,
                        host: "foo.co.uk".into(),
                        port: 443,
                    },
                    Endpoint {
                        scheme: Scheme::Https,
                        host: "bar.co.uk".into(),
                        port: 443,
                    },
                ],
            },
            protocol: "gts.x.core.oagw.protocol.v1~x.core.oagw.http.v1".into(),
            alias: None,
            auth: None,
            headers: None,
            plugins: None,
            rate_limit: None,
            tags: vec![],
            enabled: true,
        };

        // Should fail because alias cannot be derived and none was provided.
        let err = svc.create_upstream(&ctx, req).await.unwrap_err();
        assert!(
            matches!(err, DomainError::Validation { .. }),
            "expected Validation error, got: {err:?}",
        );
    }

    #[tokio::test]
    async fn multi_endpoint_public_suffix_with_explicit_alias_succeeds() {
        let svc = make_service();
        let tenant = Uuid::new_v4();
        let ctx = test_ctx(tenant);

        // Same public-suffix-only endpoints, but an explicit alias is provided.
        let req = CreateUpstreamRequest {
            server: Server {
                endpoints: vec![
                    Endpoint {
                        scheme: Scheme::Https,
                        host: "foo.co.uk".into(),
                        port: 443,
                    },
                    Endpoint {
                        scheme: Scheme::Https,
                        host: "bar.co.uk".into(),
                        port: 443,
                    },
                ],
            },
            protocol: "gts.x.core.oagw.protocol.v1~x.core.oagw.http.v1".into(),
            alias: Some("my-uk-backends".into()),
            auth: None,
            headers: None,
            plugins: None,
            rate_limit: None,
            tags: vec![],
            enabled: true,
        };

        let u = svc.create_upstream(&ctx, req).await.unwrap();
        assert_eq!(u.alias, "my-uk-backends");
    }
}
