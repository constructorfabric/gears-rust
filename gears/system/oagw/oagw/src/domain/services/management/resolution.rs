use super::alias::{normalize_alias, validate_alias};
use super::merge::{
    compute_effective_config, compute_effective_config_reporting, is_visible_to_descendant,
    shadowed_route_limits, shared_route_view, shared_upstream_view,
};
use super::validation::validate_endpoint_hostnames_ssrf;

use std::collections::HashSet;

use toolkit_macros::domain_model;

use crate::domain::error::DomainError;
use crate::domain::model::{HttpMethod, Route, ShadowedRouteLimit, Upstream};
use crate::domain::repo::{ProxyConfigReader, RepositoryError, RowKey, Tags};
use crate::domain::route_matching::{SeparatorBypass, parse_method, select_route};

use super::ControlPlaneServiceImpl;

use toolkit_security::SecurityContext;
use uuid::Uuid;

/// What [`ControlPlaneServiceImpl::resolve_alias_sourced`] resolves to: a
/// [`ProxyTarget`] whose route may be absent, with its sources.
///
/// [`ProxyTarget`]: crate::domain::model::ProxyTarget
#[domain_model]
#[derive(Debug)]
pub(super) struct ResolvedAlias {
    /// The effective upstream.
    pub(super) effective: Upstream,
    /// The matched route, when a method and path were given.
    pub(super) route: Option<Route>,
    /// The enforced rate limits of the routes `route` shadows.
    pub(super) shadowed_route_limits: Vec<ShadowedRouteLimit>,
    /// The rows the answer is built from.
    pub(super) sources: Contributors,
}

/// Where resolution reads the configuration from.
#[derive(Clone, Copy)]
enum Source<'a> {
    /// The proxy configuration cache's reader (ADR-0020), which holds no
    /// tags.
    Cache(&'a dyn ProxyConfigReader),
    /// The repositories, with or without tags.
    Repositories(Tags),
}

/// The rows a resolved answer carries settings of.
#[domain_model]
#[derive(Debug)]
pub(super) struct Contributors {
    /// The selected upstream, then the merge chain closest-first. Ancestor
    /// upstreams outside the merge chain share nothing and change nothing in
    /// the answer.
    pub(super) upstreams: Vec<RowKey>,
    /// The shadowed ancestor routes at least one of whose enforced plugins
    /// the answer carries. The matched route is not one of them, nor is a
    /// route that enforces only a rate limit: that limit is not in the answer.
    pub(super) routes: Vec<RowKey>,
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

    /// [`Self::resolve_alias_sourced`]'s effective config and route.
    #[cfg(test)]
    pub(crate) async fn resolve_alias(
        &self,
        ctx: &SecurityContext,
        tenant_chain: &[Uuid],
        alias: &str,
        method_path: Option<(&str, &str)>,
        tags: Tags,
    ) -> Result<(Upstream, Option<Route>), DomainError> {
        let resolved = self
            .resolve_alias_sourced(ctx, tenant_chain, alias, method_path, tags)
            .await?;
        Ok((resolved.effective, resolved.route))
    }

    /// Alias resolution (`cpt-cf-oagw-algo-alias-resolution`): the alias's
    /// upstreams across the tenant chain are loaded in one read; the closest
    /// enabled upstream wins, and the visible enabled ancestors above it form
    /// the merge chain. With `method_path`, the route is selected over the
    /// selected and ancestor upstreams, with the enforced rate limits of the
    /// routes it shadows. Returns the effective config and the rows it is
    /// built from; with [`Tags::Skip`] it carries no tags.
    ///
    /// With a [`ProxyConfigReader`] and [`Tags::Skip`] both reads go through
    /// the reader (ADR-0020); otherwise they go to the repositories.
    ///
    /// An alias that breaks the alias rules (`validate_alias`) is not found
    /// without a read: no stored upstream can have it, and a request cannot
    /// fill the cache with it.
    ///
    /// Of an upstream or route another tenant owns, only the shared fields
    /// are used and returned: its `private` ones stay with their owner, also
    /// when the requesting tenant has no upstream of its own.
    pub(super) async fn resolve_alias_sourced(
        &self,
        ctx: &SecurityContext,
        tenant_chain: &[Uuid],
        alias: &str,
        method_path: Option<(&str, &str)>,
        tags: Tags,
    ) -> Result<ResolvedAlias, DomainError> {
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
        let subject = ctx.subject_tenant_id();
        let shared = |u: Upstream| {
            if u.tenant_id == subject {
                u
            } else {
                shared_upstream_view(u)
            }
        };
        let (selected_upstream, merge_chain) = select_upstream(candidates, subject, tenant_chain)?;
        let selected_upstream = shared(selected_upstream);
        let merge_chain: Vec<Upstream> = merge_chain.into_iter().map(shared).collect();
        let upstream_sources: Vec<RowKey> = std::iter::once(&selected_upstream)
            .chain(&merge_chain)
            .map(|u| RowKey {
                tenant_id: u.tenant_id,
                id: u.id,
            })
            .collect();

        // The deny-list may have grown since the upstream was stored.
        validate_endpoint_hostnames_ssrf(&self.ssrf_guard, &selected_upstream.server.endpoints)?;

        // Selected upstream first, then ancestor upstreams closest-first.
        let (route, shadowed) = if let Some((method, path)) = method_path {
            let upstreams: Vec<&Upstream> = std::iter::once(&selected_upstream)
                .chain(&merge_chain)
                .collect();
            let (route, shadowed) = self
                .match_route(source, tenant_chain, &upstreams, method, path)
                .await?;
            let shared = |route: Route| {
                if route.tenant_id == subject {
                    route
                } else {
                    shared_route_view(route)
                }
            };
            // Root-first, as the merge walks the chain.
            let shadowed: Vec<Route> = shadowed.into_iter().rev().map(shared).collect();
            (Some(shared(route)), shadowed)
        } else {
            (None, Vec::new())
        };
        let mut sources = Contributors {
            upstreams: upstream_sources,
            routes: Vec::new(),
        };

        // Build effective config. Nothing is shadowed without upstreams above
        // the selected one.
        if merge_chain.is_empty() {
            // Single upstream → apply route overrides directly if present.
            let effective = match route {
                Some(ref route) => {
                    compute_effective_config(std::slice::from_ref(&selected_upstream), Some(route))?
                }
                None => selected_upstream,
            };
            return Ok(ResolvedAlias {
                effective,
                route,
                shadowed_route_limits: Vec::new(),
                sources,
            });
        }

        // Root-first order for merge: reverse ancestors, append selected.
        let mut merge_vec: Vec<Upstream> = merge_chain.into_iter().rev().collect();
        merge_vec.push(selected_upstream);

        // Reported on the proxy path only: the SDK's resolve (`Tags::Load`)
        // sends nothing.
        let report = matches!(tags, Tags::Skip);
        let effective = compute_effective_config_reporting(
            &merge_vec,
            route.as_ref(),
            &shadowed,
            &mut |withheld| {
                if report {
                    self.withheld_log.report(self.metrics.as_deref(), &withheld);
                }
            },
            &mut |r| {
                sources.routes.push(RowKey {
                    tenant_id: r.tenant_id,
                    id: r.id,
                });
            },
        )?;
        Ok(ResolvedAlias {
            effective,
            route,
            shadowed_route_limits: shadowed_route_limits(&shadowed),
            sources,
        })
    }

    /// The best enabled HTTP route for `method` and `path` over `upstreams`
    /// (preference order) owned by a tenant in `tenant_chain`, and the routes
    /// it shadows: closest-first, for each upstream after the winner's, the
    /// route its owner would select on it (`shadowed_routes`). Through the
    /// cache, [`select_route`] runs here on each upstream's routes; a route's
    /// tenant is its upstream's.
    ///
    /// The repositories answer the winner in one call; the routes above it
    /// take one more read, only when there are upstreams above it.
    async fn match_route(
        &self,
        source: Source<'_>,
        tenant_chain: &[Uuid],
        upstreams: &[&Upstream],
        method: &str,
        path: &str,
    ) -> Result<(Route, Vec<Route>), DomainError> {
        let upstream_ids: Vec<Uuid> = upstreams.iter().map(|u| u.id).collect();
        let not_found = || DomainError::not_found("route", Uuid::nil());
        // Unknown methods never match; nothing is read for them.
        let http_method = parse_method(method).ok_or_else(not_found)?;
        let keys: Vec<RowKey> = upstreams
            .iter()
            .map(|u| RowKey {
                tenant_id: u.tenant_id,
                id: u.id,
            })
            .collect();
        // The upstreams after the winner's.
        let above = |winner: &Route| {
            upstream_ids
                .iter()
                .position(|id| *id == winner.upstream_id)
                .map_or(upstream_ids.len(), |i| i + 1)
        };
        match source {
            Source::Cache(reader) => {
                let routes = reader.routes_of(&keys).await?;
                let winner = select_route(
                    routes.iter().flat_map(|r| r.iter()),
                    tenant_chain,
                    &upstream_ids,
                    http_method,
                    path,
                )
                .map_err(|bypass| bypass.into_error(path))?
                .cloned()
                .ok_or_else(not_found)?;
                let from = above(&winner);
                let shadowed = shadowed_routes(
                    upstream_ids
                        .iter()
                        .copied()
                        .zip(routes.iter().map(|r| &r[..]))
                        .skip(from),
                    tenant_chain,
                    http_method,
                    path,
                )?;
                Ok((winner, shadowed))
            }
            Source::Repositories(tags) => {
                let winner = match self
                    .routes
                    .find_matching_in_tenants(tenant_chain, &upstream_ids, method, path, tags)
                    .await
                {
                    Ok(route) => route,
                    Err(RepositoryError::NotFound { .. }) => return Err(not_found()),
                    Err(RepositoryError::SeparatorBypass) => {
                        return Err(SeparatorBypass.into_error(path));
                    }
                    Err(e) => return Err(DomainError::from(e)),
                };
                let Some(above_keys) = keys.get(above(&winner)..).filter(|k| !k.is_empty()) else {
                    return Ok((winner, Vec::new()));
                };
                let loaded = self.routes.list_proxy_routes(above_keys).await?;
                let shadowed = shadowed_routes(
                    loaded
                        .iter()
                        .map(|(key, routes)| (key.id, routes.as_slice())),
                    tenant_chain,
                    http_method,
                    path,
                )?;
                Ok((winner, shadowed))
            }
        }
    }
}

/// For each `(upstream, its routes)`, the route its owner would select for
/// the request on that upstream, when it enforces its plugins or rate limit:
/// what a route the request matched closer to the requester must not bypass.
/// The others apply nothing and are left out. The merge applies only the
/// enforced plugins; the enforced limit is returned apart
/// ([`shadowed_route_limits`]), for the proxy to check on the route's own
/// bucket.
///
/// An encoded separator that selects another route on one of these upstreams
/// is refused as on the winner's ([`SeparatorBypass`]) when a route there
/// enforces something: read decoded, the request may fall under it.
fn shadowed_routes<'r>(
    per_upstream: impl Iterator<Item = (Uuid, &'r [Route])>,
    tenant_chain: &[Uuid],
    method: HttpMethod,
    path: &str,
) -> Result<Vec<Route>, DomainError> {
    let mut shadowed = Vec::new();
    for (upstream_id, routes) in per_upstream {
        match select_route(routes, tenant_chain, &[upstream_id], method, path) {
            Ok(selected) => shadowed.extend(selected.filter(|r| enforces_something(r)).cloned()),
            Err(bypass) if routes.iter().any(enforces_something) => {
                return Err(bypass.into_error(path));
            }
            Err(_) => {}
        }
    }
    Ok(shadowed)
}

/// Whether `route` enforces its plugins or rate limit.
fn enforces_something(route: &Route) -> bool {
    use crate::domain::model::SharingMode::Enforce;

    route.plugins.as_ref().is_some_and(|p| p.sharing == Enforce)
        || route
            .rate_limit
            .as_ref()
            .is_some_and(|r| r.sharing == Enforce)
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
