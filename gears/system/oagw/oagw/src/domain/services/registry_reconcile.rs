//! The startup reconcile of registry-managed upstreams and routes with the
//! instances in the types registry (`cpt-cf-oagw-flow-domain-registry-provisioning`,
//! ADR-0018 *Startup Provisioning from the Types Registry*). The gear runs it
//! once in `post_init`, through the [`RegistryProvisioner`] that only it holds.

use std::collections::{HashMap, HashSet};

use toolkit_macros::domain_model;
use toolkit_security::SecurityContext;
use tracing::{debug, info, warn};
use uuid::Uuid;

use super::RegistryProvisioner;
use super::management::{check_registry_route_overlaps, route_drift, upstream_drift};
use crate::domain::error::DomainError;
use crate::domain::model::{
    CreateRouteRequest, CreateUpstreamRequest, ListQuery, ManagedBy, UpdateRouteRequest,
    UpdateUpstreamRequest,
};
use crate::domain::repo::RowKey;
use crate::domain::type_provisioning::{ProvisionedRoute, ProvisionedUpstream};

/// What [`reconcile_registry`] did to one kind of row.
#[domain_model]
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Tally {
    /// Rows created.
    pub(crate) created: usize,
    /// Rows updated.
    pub(crate) updated: usize,
    /// Rows already matching the registry.
    pub(crate) unchanged: usize,
    /// Rows removed.
    pub(crate) removed: usize,
    /// No longer in the registry but kept: routes still use the upstream.
    /// Always 0 for routes.
    pub(crate) kept: usize,
}

/// Counts reported by [`reconcile_registry`].
#[domain_model]
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct ProvisioningCounts {
    /// What happened to upstreams.
    pub(crate) upstreams: Tally,
    /// What happened to routes.
    pub(crate) routes: Tally,
}

/// Make the registry-managed upstreams and routes match the instances other
/// gears registered in types-registry (`cpt-cf-oagw-flow-domain-registry-provisioning`).
///
/// The registry wins: missing instances are created, changed ones updated,
/// and rows the registry no longer has removed, except an upstream that routes
/// still use, which is kept with a warning. Row IDs are the GTS instance
/// UUIDs. Registry routes that overlap each other fail startup before any
/// write.
///
/// Removals run first, routes before upstreams. A route the registry moved to
/// another upstream is deleted with them and re-created under the same ID, so
/// its old upstream and that upstream's alias can go in the same boot. Stored
/// upstream instances are applied before new ones, so a new instance can take
/// an alias a changed one gives up.
///
/// Replicas booting together race on the same rows: a create that conflicts
/// falls back to a lookup, and a delete that finds nothing counts as done.
/// Any other failure fails startup.
pub(crate) async fn reconcile_registry(
    cp: &dyn RegistryProvisioner,
    upstreams: &[ProvisionedUpstream],
    routes: &[ProvisionedRoute],
    root_tenant_id: Uuid,
) -> anyhow::Result<ProvisioningCounts> {
    let upstreams = upstreams
        .iter()
        .map(|u| {
            Ok((
                instance_key("upstream", u.request.id, u.tenant_id, root_tenant_id)?,
                u,
            ))
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    let routes = routes
        .iter()
        .map(|r| {
            Ok((
                instance_key("route", r.request.id, r.tenant_id, root_tenant_id)?,
                r,
            ))
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    let requests: Vec<_> = routes.iter().map(|(key, r)| (*key, &r.request)).collect();
    check_registry_route_overlaps(&requests).map_err(|e| anyhow::anyhow!("{e}"))?;
    let mut counts = ProvisioningCounts::default();

    let wanted: HashMap<RowKey, Uuid> = routes
        .iter()
        .map(|(key, r)| (*key, r.request.upstream_id))
        .collect();
    let stored_routes = cp
        .registry_route_keys()
        .await
        .map_err(|e| anyhow::anyhow!("Failed to list registry-managed routes: {e}"))?;
    let mut replaced = HashSet::new();
    for key in stored_routes {
        let ctx = provisioning_ctx(key.tenant_id)?;
        let Some(&upstream_id) = wanted.get(&key) else {
            if deleted("route", key, cp.delete_registry_route(&ctx, key.id).await)? {
                info!(id = %key.id, tenant_id = %key.tenant_id, "Removed route no longer in types-registry");
                counts.routes.removed += 1;
            }
            continue;
        };
        let Some(stored) = lookup("route", key, cp.get_route(&ctx, key.id).await)? else {
            continue;
        };
        if stored.upstream_id != upstream_id
            && replace_route(cp, &ctx, key, stored.upstream_id, upstream_id).await?
        {
            replaced.insert(key);
        }
    }
    let wanted: HashSet<RowKey> = upstreams.iter().map(|(key, _)| *key).collect();
    let stored_upstreams = cp
        .registry_upstream_keys()
        .await
        .map_err(|e| anyhow::anyhow!("Failed to list registry-managed upstreams: {e}"))?;
    let mut kept = Vec::new();
    for &key in &stored_upstreams {
        if wanted.contains(&key) {
            continue;
        }
        let ctx = provisioning_ctx(key.tenant_id)?;
        let first_route = ListQuery { top: 1, skip: 0 };
        let in_use = !cp
            .list_routes(&ctx, Some(key.id), &first_route)
            .await
            .map_err(|e| provisioning_failed("upstream", key, &e))?
            .is_empty();
        if in_use {
            warn!(
                id = %key.id,
                tenant_id = %key.tenant_id,
                "upstream is no longer in types-registry but routes still use it; \
                 keeping it until they are deleted"
            );
            counts.upstreams.kept += 1;
            kept.push(key);
        } else if deleted(
            "upstream",
            key,
            cp.delete_registry_upstream(&ctx, key.id).await,
        )? {
            info!(id = %key.id, tenant_id = %key.tenant_id, "Removed upstream no longer in types-registry");
            counts.upstreams.removed += 1;
        }
    }

    // Stored instances first: one whose alias changed frees the old alias
    // for a new instance that takes it over in the same boot.
    let stored_upstreams: HashSet<RowKey> = stored_upstreams.into_iter().collect();
    let (stored, new): (Vec<_>, Vec<_>) = upstreams
        .iter()
        .partition(|(key, _)| stored_upstreams.contains(key));
    for (key, u) in stored.into_iter().chain(new) {
        let outcome = apply_upstream(cp, key, u, &kept).await?;
        outcome.count(&mut counts.upstreams);
    }
    for (key, r) in &routes {
        let outcome = match apply_route(cp, *key, r).await? {
            // Re-created after the removal phase deleted it.
            Applied::Created if replaced.contains(key) => Applied::Updated,
            outcome => outcome,
        };
        outcome.count(&mut counts.routes);
    }
    Ok(counts)
}

/// What applying one registry instance did.
enum Applied {
    Created,
    Updated,
    Unchanged,
}

impl Applied {
    fn count(self, tally: &mut Tally) {
        match self {
            Self::Created => tally.created += 1,
            Self::Updated => tally.updated += 1,
            Self::Unchanged => tally.unchanged += 1,
        }
    }
}

/// Create or update one upstream instance. `kept` are the upstreams the
/// removal phase kept for their routes, which still hold their aliases.
async fn apply_upstream(
    cp: &dyn RegistryProvisioner,
    key: RowKey,
    u: &ProvisionedUpstream,
    kept: &[RowKey],
) -> anyhow::Result<Applied> {
    let ctx = provisioning_ctx(key.tenant_id)?;
    let failed = |e: &DomainError| provisioning_failed("upstream", key, e);
    let stored = match lookup("upstream", key, cp.get_upstream(&ctx, key.id).await)? {
        Some(stored) => stored,
        None => match cp.create_registry_upstream(&ctx, u.request.clone()).await {
            Ok(created) => {
                info!(
                    id = %key.id,
                    tenant_id = %key.tenant_id,
                    alias = %created.alias,
                    "Provisioned upstream from types-registry"
                );
                return Ok(Applied::Created);
            }
            // Another replica may have created it since the lookup.
            Err(e @ DomainError::Conflict { .. }) => {
                match lookup("upstream", key, cp.get_upstream(&ctx, key.id).await)? {
                    Some(stored) => stored,
                    None => {
                        let held = alias_held_by_kept(cp, &ctx, key, kept, &e).await?;
                        return Err(held.unwrap_or_else(|| failed(&e)));
                    }
                }
            }
            Err(e) => return Err(failed(&e)),
        },
    };
    ensure_registry_managed("upstream", key, stored.managed_by)?;
    let drift = upstream_drift(&stored, &u.request).map_err(|e| failed(&e))?;
    if drift.is_empty() {
        debug!(
            id = %key.id,
            tenant_id = %key.tenant_id,
            "upstream from types-registry is up to date"
        );
        return Ok(Applied::Unchanged);
    }
    match cp
        .update_registry_upstream(&ctx, key.id, upstream_update(&u.request))
        .await
    {
        Ok(_) => {}
        // A changed alias another upstream holds.
        Err(e @ DomainError::Conflict { .. }) => {
            let held = alias_held_by_kept(cp, &ctx, key, kept, &e).await?;
            return Err(held.unwrap_or_else(|| failed(&e)));
        }
        Err(e) => return Err(failed(&e)),
    }
    info!(id = %key.id, tenant_id = %key.tenant_id, ?drift, "Updated upstream from types-registry");
    Ok(Applied::Updated)
}

/// Explain a write that conflicts on the alias of an upstream the removal
/// phase kept: restarting cannot clear it, only removing the routes that
/// keep that upstream can.
async fn alias_held_by_kept(
    cp: &dyn RegistryProvisioner,
    ctx: &SecurityContext,
    key: RowKey,
    kept: &[RowKey],
    error: &DomainError,
) -> anyhow::Result<Option<anyhow::Error>> {
    let DomainError::Conflict { resource, .. } = error else {
        return Ok(None);
    };
    for holder in kept.iter().filter(|k| k.tenant_id == key.tenant_id) {
        let Some(upstream) = lookup("upstream", *holder, cp.get_upstream(ctx, holder.id).await)?
        else {
            continue;
        };
        if upstream.alias != *resource {
            continue;
        }
        let routes = cp
            .list_routes(ctx, Some(holder.id), &ListQuery { top: 10, skip: 0 })
            .await
            .map_err(|e| provisioning_failed("upstream", *holder, &e))?;
        let routes: Vec<String> = routes.iter().map(|r| r.id.to_string()).collect();
        return Ok(Some(anyhow::anyhow!(
            "Failed to provision upstream {} (tenant={}): alias '{}' is still held by upstream {}, \
             which types-registry no longer has but routes still use ({}); delete those routes \
             or move them to another upstream, then restart",
            key.id,
            key.tenant_id,
            upstream.alias,
            holder.id,
            routes.join(", ")
        )));
    }
    Ok(None)
}

async fn apply_route(
    cp: &dyn RegistryProvisioner,
    key: RowKey,
    r: &ProvisionedRoute,
) -> anyhow::Result<Applied> {
    let ctx = provisioning_ctx(key.tenant_id)?;
    let failed = |e: &DomainError| provisioning_failed("route", key, e);
    let stored = match lookup("route", key, cp.get_route(&ctx, key.id).await)? {
        Some(stored) => stored,
        None => match cp.create_registry_route(&ctx, r.request.clone()).await {
            Ok(_) => {
                info!(
                    id = %key.id,
                    tenant_id = %key.tenant_id,
                    "Provisioned route from types-registry"
                );
                return Ok(Applied::Created);
            }
            // Another replica may have created it since the lookup.
            Err(e @ DomainError::Conflict { .. }) => {
                lookup("route", key, cp.get_route(&ctx, key.id).await)?.ok_or_else(|| failed(&e))?
            }
            Err(e) => return Err(failed(&e)),
        },
    };
    ensure_registry_managed("route", key, stored.managed_by)?;
    let drift = route_drift(&stored, &r.request);
    if drift.is_empty() {
        debug!(id = %key.id, tenant_id = %key.tenant_id, "route from types-registry is up to date");
        return Ok(Applied::Unchanged);
    }
    if stored.upstream_id != r.request.upstream_id {
        // Moved since the removal phase, by a replica on other registry
        // content (a rolling deploy).
        replace_route(cp, &ctx, key, stored.upstream_id, r.request.upstream_id).await?;
        return match cp.create_registry_route(&ctx, r.request.clone()).await {
            Ok(_) => Ok(Applied::Updated),
            // Another replica may have re-created it since the delete.
            Err(e @ DomainError::Conflict { .. }) => {
                match lookup("route", key, cp.get_route(&ctx, key.id).await)? {
                    Some(stored)
                        if stored.managed_by == ManagedBy::Registry
                            && route_drift(&stored, &r.request).is_empty() =>
                    {
                        Ok(Applied::Unchanged)
                    }
                    _ => Err(failed(&e)),
                }
            }
            Err(e) => Err(failed(&e)),
        };
    }
    cp.update_registry_route(&ctx, key.id, route_update(&r.request))
        .await
        .map_err(|e| failed(&e))?;
    info!(id = %key.id, tenant_id = %key.tenant_id, ?drift, "Updated route from types-registry");
    Ok(Applied::Updated)
}

/// Delete a registry route the registry moved from upstream `from` to `to`,
/// so it can be re-created on `to` under the same ID. `false` when another
/// replica deleted it first.
async fn replace_route(
    cp: &dyn RegistryProvisioner,
    ctx: &SecurityContext,
    key: RowKey,
    from: Uuid,
    to: Uuid,
) -> anyhow::Result<bool> {
    info!(
        id = %key.id,
        tenant_id = %key.tenant_id,
        %from,
        %to,
        "Replacing route moved to another upstream in types-registry"
    );
    deleted("route", key, cp.delete_registry_route(ctx, key.id).await)
}

/// The row key of a registry instance: its GTS instance UUID in its tenant,
/// or the root tenant when the instance names none.
fn instance_key(
    kind: &str,
    id: Option<Uuid>,
    tenant_id: Option<Uuid>,
    root_tenant_id: Uuid,
) -> anyhow::Result<RowKey> {
    let id = id.ok_or_else(|| anyhow::anyhow!("{kind} instance from types-registry has no ID"))?;
    Ok(RowKey {
        tenant_id: tenant_id.unwrap_or(root_tenant_id),
        id,
    })
}

/// A row with a registry instance's ID that the Management API created can
/// only come from a bug; never overwrite it.
fn ensure_registry_managed(kind: &str, key: RowKey, managed_by: ManagedBy) -> anyhow::Result<()> {
    if managed_by == ManagedBy::Registry {
        return Ok(());
    }
    anyhow::bail!(
        "{kind} {} (tenant={}) has the ID of a types-registry instance but was created \
         through the Management API; refusing to overwrite it",
        key.id,
        key.tenant_id
    )
}

/// The full replacement that makes a stored upstream match `req`.
fn upstream_update(req: &CreateUpstreamRequest) -> UpdateUpstreamRequest {
    UpdateUpstreamRequest {
        server: req.server.clone(),
        protocol: req.protocol.clone(),
        alias: req.alias.clone(),
        auth: req.auth.clone(),
        headers: req.headers.clone(),
        plugins: req.plugins.clone(),
        rate_limit: req.rate_limit.clone(),
        cors: req.cors.clone(),
        tags: req.tags.clone(),
        enabled: req.enabled,
    }
}

/// The full replacement that makes a stored route on the same upstream match `req`.
fn route_update(req: &CreateRouteRequest) -> UpdateRouteRequest {
    UpdateRouteRequest {
        match_rules: req.match_rules.clone(),
        plugins: req.plugins.clone(),
        rate_limit: req.rate_limit.clone(),
        cors: req.cors.clone(),
        tags: req.tags.clone(),
        priority: req.priority,
        enabled: req.enabled,
    }
}

/// The instance a lookup found, or `None` on `NotFound`. Any other error
/// fails provisioning rather than reading as absent.
fn lookup<T>(kind: &str, key: RowKey, result: Result<T, DomainError>) -> anyhow::Result<Option<T>> {
    match result {
        Ok(stored) => Ok(Some(stored)),
        Err(DomainError::NotFound { .. }) => Ok(None),
        Err(e) => Err(anyhow::anyhow!(
            "Failed to look up {kind} {} (tenant={}) for provisioning: {e}",
            key.id,
            key.tenant_id
        )),
    }
}

/// Whether a delete removed the row; `NotFound` (another replica removed it
/// first) is `false`, any other error fails provisioning.
fn deleted(kind: &str, key: RowKey, result: Result<(), DomainError>) -> anyhow::Result<bool> {
    match result {
        Ok(()) => Ok(true),
        Err(DomainError::NotFound { .. }) => Ok(false),
        Err(e) => Err(provisioning_failed(kind, key, &e)),
    }
}

fn provisioning_failed(kind: &str, key: RowKey, error: &DomainError) -> anyhow::Error {
    anyhow::anyhow!(
        "Failed to provision {kind} {} (tenant={}): {error}",
        key.id,
        key.tenant_id
    )
}

/// Security context for provisioning writes in `tenant_id`, acting as the default subject.
pub(crate) fn provisioning_ctx(tenant_id: Uuid) -> anyhow::Result<SecurityContext> {
    Ok(SecurityContext::builder()
        .subject_tenant_id(tenant_id)
        .subject_id(toolkit_security::constants::DEFAULT_SUBJECT_ID)
        .build()?)
}

#[cfg(test)]
#[path = "registry_reconcile_tests.rs"]
pub(crate) mod tests;
