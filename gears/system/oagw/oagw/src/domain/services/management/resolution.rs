use super::alias::{normalize_alias, validate_alias};
use super::merge::{compute_effective_config, is_visible_to_descendant};
use super::validation::validate_endpoint_hostnames_ssrf;

use std::collections::HashSet;

use crate::domain::error::DomainError;
use crate::domain::model::{Route, Upstream};
use crate::domain::repo::{ProxyConfigReader, RepositoryError, RowKey, Tags};
use crate::domain::route_matching::{parse_method, select_route};

use super::ControlPlaneServiceImpl;

use toolkit_security::SecurityContext;
use uuid::Uuid;

/// Where resolution reads the configuration from.
#[derive(Clone, Copy)]
enum Source<'a> {
    /// The proxy configuration cache's reader (ADR-0020), which holds no
    /// tags.
    Cache(&'a dyn ProxyConfigReader),
    /// The repositories, with or without tags.
    Repositories(Tags),
}

impl ControlPlaneServiceImpl {
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

    /// Alias resolution (`cpt-cf-oagw-algo-alias-resolution`): the alias's
    /// upstreams across the tenant chain are loaded in one read; the closest
    /// enabled upstream wins, and the visible enabled ancestors above it form
    /// the merge chain. With `method_path`, the route is selected over the
    /// selected and ancestor upstreams. Returns the effective config; with
    /// [`Tags::Skip`] it carries no tags.
    ///
    /// With a [`ProxyConfigReader`] and [`Tags::Skip`] both reads go through
    /// the reader (ADR-0020); otherwise they go to the repositories.
    ///
    /// An alias that breaks the alias rules (`validate_alias`) is not found
    /// without a read: no stored upstream can have it, and a request cannot
    /// fill the cache with it.
    pub(crate) async fn resolve_alias(
        &self,
        ctx: &SecurityContext,
        tenant_chain: &[Uuid],
        alias: &str,
        method_path: Option<(&str, &str)>,
        tags: Tags,
    ) -> Result<(Upstream, Option<Route>), DomainError> {
        // Normalize the incoming alias for case-insensitive matching.
        let alias = normalize_alias(alias);
        if validate_alias(&alias).is_err() {
            return Err(DomainError::not_found("upstream", Uuid::nil()));
        }
        let source = match (tags, self.proxy_reader.as_deref()) {
            (Tags::Skip, Some(reader)) => Source::Cache(reader),
            (Tags::Skip, None) | (Tags::Load, _) => Source::Repositories(tags),
        };

        // The cache's candidates are shared; only the selected ones are
        // cloned.
        let cached;
        let loaded;
        let candidates: &[Upstream] = match source {
            Source::Cache(reader) => {
                cached = reader.upstreams_by_alias(&alias, tenant_chain).await?;
                &cached
            }
            Source::Repositories(tags) => {
                let chain_ids: HashSet<Uuid> = tenant_chain.iter().copied().collect();
                loaded = self
                    .upstreams
                    .list_by_alias_for_tenants(&alias, &chain_ids, tags)
                    .await?;
                &loaded
            }
        };
        let (selected_upstream, merge_chain) =
            select_upstream(candidates, ctx.subject_tenant_id(), tenant_chain)?;

        // The deny-list may have grown since the upstream was stored.
        validate_endpoint_hostnames_ssrf(&self.ssrf_guard, &selected_upstream.server.endpoints)?;

        // Selected upstream first, then ancestor upstreams closest-first.
        let route = if let Some((method, path)) = method_path {
            let upstreams: Vec<&Upstream> = std::iter::once(&selected_upstream)
                .chain(&merge_chain)
                .collect();
            Some(
                self.match_route(source, tenant_chain, &upstreams, method, path)
                    .await?,
            )
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
        let mut merge_vec: Vec<Upstream> = merge_chain.into_iter().rev().collect();
        merge_vec.push(selected_upstream);

        let effective = compute_effective_config(&merge_vec, route.as_ref())?;
        Ok((effective, route))
    }

    /// The best enabled HTTP route for `method` and `path` over `upstreams`
    /// (preference order) owned by a tenant in `tenant_chain`. Through the
    /// cache, [`select_route`] runs here on each upstream's routes; a route's
    /// tenant is its upstream's.
    async fn match_route(
        &self,
        source: Source<'_>,
        tenant_chain: &[Uuid],
        upstreams: &[&Upstream],
        method: &str,
        path: &str,
    ) -> Result<Route, DomainError> {
        let upstream_ids: Vec<Uuid> = upstreams.iter().map(|u| u.id).collect();
        let not_found = || DomainError::not_found("route", Uuid::nil());
        let reader = match source {
            Source::Cache(reader) => reader,
            Source::Repositories(tags) => {
                return match self
                    .routes
                    .find_matching_in_tenants(tenant_chain, &upstream_ids, method, path, tags)
                    .await
                {
                    Ok(route) => Ok(route),
                    Err(RepositoryError::NotFound { .. }) => Err(not_found()),
                    Err(e) => Err(DomainError::from(e)),
                };
            }
        };
        // Unknown methods never match; nothing is read for them.
        let method = parse_method(method).ok_or_else(not_found)?;
        let keys: Vec<RowKey> = upstreams
            .iter()
            .map(|u| RowKey {
                tenant_id: u.tenant_id,
                id: u.id,
            })
            .collect();
        let routes = reader.routes_of(&keys).await?;
        select_route(
            routes.iter().flat_map(|r| r.iter()),
            tenant_chain,
            &upstream_ids,
            method,
            path,
        )
        .cloned()
        .ok_or_else(not_found)
    }
}

/// Pick the upstream an alias resolves to among its `candidates` (every
/// upstream with the alias in `tenant_chain`, any order) for `tenant_id`:
/// the closest visible enabled one, and the visible enabled ones above it
/// closest-first (the merge chain). An ancestor's upstream is visible only
/// when it shares something with descendants. No enabled match is
/// `UpstreamDisabled` when a visible match is disabled, else not found.
pub(super) fn select_upstream(
    candidates: &[Upstream],
    tenant_id: Uuid,
    tenant_chain: &[Uuid],
) -> Result<(Upstream, Vec<Upstream>), DomainError> {
    let mut candidates: Vec<&Upstream> = candidates.iter().collect();
    // Candidates are read within the chain; one outside it sorts last.
    candidates.sort_by_key(|u| {
        tenant_chain
            .iter()
            .position(|t| *t == u.tenant_id)
            .unwrap_or(usize::MAX)
    });

    // Collect all visible enabled upstreams in chain order.
    let mut found: Vec<&Upstream> = Vec::new();
    let mut disabled_alias: Option<&str> = None;
    for upstream in candidates {
        if upstream.tenant_id != tenant_id && !is_visible_to_descendant(upstream) {
            continue;
        }
        if !upstream.enabled {
            if disabled_alias.is_none() {
                disabled_alias = Some(&upstream.alias);
            }
            continue;
        }
        found.push(upstream);
    }

    // The winning upstream is the closest enabled match.
    let mut found = found.into_iter().cloned();
    let Some(selected_upstream) = found.next() else {
        if let Some(alias) = disabled_alias {
            return Err(DomainError::upstream_disabled(alias));
        }
        return Err(DomainError::not_found("upstream", Uuid::nil()));
    };

    // Ancestors above the selected one form the merge chain (closest first).
    Ok((selected_upstream, found.collect()))
}
