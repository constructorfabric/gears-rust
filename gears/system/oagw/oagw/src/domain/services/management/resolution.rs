use super::alias::normalize_alias;
use super::merge::{compute_effective_config, is_visible_to_descendant};
use super::validation::validate_endpoint_hostnames_ssrf;

use crate::domain::error::DomainError;
use crate::domain::model::{Route, Upstream};
use crate::domain::repo::{RepositoryError, Tags};

use super::ControlPlaneServiceImpl;

use toolkit_security::SecurityContext;
use uuid::Uuid;

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

    /// Alias resolution (`cpt-cf-oagw-algo-alias-resolution`): one
    /// `list_by_alias_for_tenants` call loads the alias across the tenant
    /// chain; the closest enabled upstream wins, and the visible enabled
    /// ancestors above it form the merge chain. With `method_path`, one
    /// `find_matching_in_tenants` call resolves the route over the selected
    /// and ancestor upstreams. Returns the effective config; with
    /// [`Tags::Skip`] it carries no tags.
    pub(crate) async fn resolve_alias(
        &self,
        ctx: &SecurityContext,
        tenant_chain: &[Uuid],
        alias: &str,
        method_path: Option<(&str, &str)>,
        tags: Tags,
    ) -> Result<(Upstream, Option<Route>), DomainError> {
        let tenant_id = ctx.subject_tenant_id();
        // Normalize the incoming alias for case-insensitive matching.
        let alias = normalize_alias(alias);

        let chain_ids: std::collections::HashSet<Uuid> = tenant_chain.iter().copied().collect();
        let mut candidates = self
            .upstreams
            .list_by_alias_for_tenants(&alias, &chain_ids, tags)
            .await?;
        candidates.sort_by_key(|u| tenant_chain.iter().position(|t| *t == u.tenant_id));

        // Collect all visible enabled upstreams in chain order.
        let mut found: Vec<Upstream> = Vec::new();
        let mut disabled_alias: Option<String> = None;
        for upstream in candidates {
            if upstream.tenant_id != tenant_id && !is_visible_to_descendant(&upstream) {
                continue;
            }
            if !upstream.enabled {
                if disabled_alias.is_none() {
                    disabled_alias = Some(upstream.alias.clone());
                }
                continue;
            }
            found.push(upstream);
        }

        // The winning upstream is the closest enabled match.
        let mut found = found.into_iter();
        let Some(selected_upstream) = found.next() else {
            if let Some(alias) = disabled_alias {
                return Err(DomainError::upstream_disabled(alias));
            }
            return Err(DomainError::not_found("upstream", Uuid::nil()));
        };

        // The deny-list may have grown since the upstream was stored.
        validate_endpoint_hostnames_ssrf(&self.ssrf_guard, &selected_upstream.server.endpoints)?;

        // Ancestors above the selected one form the merge chain (closest first).
        let merge_chain: Vec<Upstream> = found.collect();

        // Selected upstream first, then ancestor upstreams closest-first.
        let route = if let Some((method, path)) = method_path {
            let upstream_ids: Vec<Uuid> = std::iter::once(selected_upstream.id)
                .chain(merge_chain.iter().map(|u| u.id))
                .collect();
            match self
                .routes
                .find_matching_in_tenants(tenant_chain, &upstream_ids, method, path, tags)
                .await
            {
                Ok(route) => Some(route),
                Err(RepositoryError::NotFound { .. }) => {
                    return Err(DomainError::not_found("route", Uuid::nil()));
                }
                Err(e) => return Err(DomainError::from(e)),
            }
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
}
