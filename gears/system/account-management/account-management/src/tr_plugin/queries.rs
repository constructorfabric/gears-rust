//! Query implementations for `PluginImpl` SDK methods.
//!
//! Every function in this gear:
//!
//! - Takes a borrowed `Arc<dyn TenantHierarchyReadPort>` (the seam --
//!   see [`crate::domain::tenant::hierarchy_read_port`]) and resolves
//!   reads through it. The port's adapter centralizes the
//!   `AccessScope::allow_all()` trust elevation at a single named
//!   call site so this gear no longer carries that concern.
//! - Hydrates the public `tenant_type` field through
//!   [`PlatformTypesRegistryApi`] — a named system read, see
//!   [`super::plugin_impl::PluginImpl`] — using either a single
//!   `resolve_type_id` (single-row results) or a batched
//!   `resolve_type_ids` (page-style results). Per DESIGN §3.4
//!   / §5, **any** registry failure (a failed read or one unknown
//!   uuid in a batch) fails the SDK call with
//!   [`TenantResolverError::Internal`]; the plugin must not return
//!   raw UUIDs in place of the public chained `tenant_type`.
//! - Maps `DomainError` to the SDK error taxonomy through
//!   [`super::error_map::domain_err_to_tr_err`] so the SDK boundary
//!   only ever sees `TenantNotFound` or `Internal`.
//!
//! # Pre-order for `get_descendants`
//!
//! AM's `tenant_closure` does not carry a pre-order column or a
//! depth-from-ancestor column. The DESIGN names a "single subtree
//! recursive read" implemented as a SQL recursive CTE; the secure
//! `toolkit-db` extension does not expose raw `ConnectionTrait`
//! access today, so the v1 implementation here builds the parent
//! map in-memory from the **barrier-only** closure subtree
//! (system-invariants only via the port), walks it pre-order on the
//! client, and applies the caller's `status_filter` as an emission
//! predicate. Splitting graph construction (system invariants) from
//! emission (caller predicate) is required for correctness --
//! folding the caller predicate into the closure scan would prune
//! whole branches whose intermediate parent fails the filter even
//! when deeper descendants match (e.g. `Root -> Suspended -> Active`
//! filtered by `[Active]`). The recursive CTE optimization is
//! tracked as a follow-up once `toolkit-db` exposes a safe raw-SQL
//! hook.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use uuid::Uuid;

use tenant_resolver_sdk::{
    BarrierMode as SdkBarrierMode, GetAncestorsResponse, GetDescendantsResponse, TenantId,
    TenantInfo, TenantRef, TenantResolverError, TenantStatus as SdkTenantStatus,
};
use toolkit_security::PlatformSecurityContext;
use types_registry_sdk::{PlatformTypesRegistryApi, PlatformTypesRegistryApiExt};

use crate::domain::tenant::hierarchy_read_port::{
    BarrierMode, StatusFilter, TenantHierarchyReadPort,
};
use crate::domain::tenant::model::{TenantModel, TenantStatus as DomainTenantStatus};

use super::error_map::domain_err_to_tr_err;
use super::projection::{map_status_to_sdk, row_to_tenant_info, row_to_tenant_ref};

/// Projection failure helper. The projection helpers return `None`
/// when a provisioning row reaches them despite the port's
/// provisioning-exclusion (defense-in-depth -- should never fire).
/// Logged server-side rather than embedded in the SDK error so we
/// don't leak storage shape to plugin consumers.
fn projection_internal(id: Uuid) -> TenantResolverError {
    tracing::warn!(
        target: "tr_plugin",
        tenant_id = %id,
        "TenantModel failed SDK projection (provisioning leak through port)"
    );
    TenantResolverError::Internal("tenant resolver projection failure".to_owned())
}

/// Emit a structured audit event when a caller selects
/// [`SdkBarrierMode::Ignore`] on a hierarchy read.
///
/// Per FEATURE §6 (`cpt-cf-tr-plugin-nfr-audit-trail`) and DESIGN §4
/// the plugin MUST surface a per-call audit signal whenever the
/// barrier predicate is bypassed; the gateway is trusted to gate
/// the `Ignore` mode at the public API surface, but the plugin
/// still records the bypass so operator audit can correlate authz
/// decisions made above it. `target: "tr_plugin.audit"` separates
/// these events from the operational `target: "tr_plugin"` so log
/// pipelines can route the audit stream independently.
///
/// No-op when `mode` is [`SdkBarrierMode::Respect`].
fn audit_barrier_bypass(
    method: &'static str,
    mode: SdkBarrierMode,
    pivot: Uuid,
    extra: Option<Uuid>,
) {
    if !matches!(mode, SdkBarrierMode::Ignore) {
        return;
    }
    if let Some(other) = extra {
        tracing::info!(
            target: "tr_plugin.audit",
            event = "barrier_bypass",
            method = method,
            pivot = %pivot,
            other = %other,
            "BarrierMode::Ignore selected"
        );
    } else {
        tracing::info!(
            target: "tr_plugin.audit",
            event = "barrier_bypass",
            method = method,
            pivot = %pivot,
            "BarrierMode::Ignore selected"
        );
    }
}

/// Translate SDK `BarrierMode` -> domain `BarrierMode`.
fn map_barrier_mode(sdk: SdkBarrierMode) -> BarrierMode {
    match sdk {
        SdkBarrierMode::Respect => BarrierMode::Respect,
        SdkBarrierMode::Ignore => BarrierMode::Ignore,
    }
}

/// Translate SDK `TenantStatus` -> domain `TenantStatus`. The SDK
/// surface excludes `Provisioning`, so this conversion is total.
fn map_sdk_status_to_domain(sdk: SdkTenantStatus) -> DomainTenantStatus {
    match sdk {
        SdkTenantStatus::Active => DomainTenantStatus::Active,
        SdkTenantStatus::Suspended => DomainTenantStatus::Suspended,
        SdkTenantStatus::Deleted => DomainTenantStatus::Deleted,
    }
}

/// Build a [`StatusFilter`] from the caller-supplied SDK status
/// slice. Empty input -> `VisibleAll`; non-empty -> `VisibleIn(...)`.
fn sdk_statuses_to_filter(statuses: &[SdkTenantStatus]) -> StatusFilter {
    if statuses.is_empty() {
        StatusFilter::VisibleAll
    } else {
        StatusFilter::VisibleIn(
            statuses
                .iter()
                .copied()
                .map(map_sdk_status_to_domain)
                .collect(),
        )
    }
}

/// Resolve a single `tenant_type_uuid` to its chained GTS identifier
/// via the registry's read by UUID.
///
/// Per DESIGN §3.4 / §5: registry unavailable -> fail with
/// [`TenantResolverError::Internal`]; must not return raw UUIDs in
/// place of public `tenant_type`. The detailed cause is logged
/// server-side.
async fn resolve_tenant_type_one(
    registry: &Arc<dyn PlatformTypesRegistryApi>,
    type_uuid: Uuid,
) -> Result<String, TenantResolverError> {
    match registry
        .resolve_type_id(&PlatformSecurityContext::outbound_marker(), type_uuid)
        .await
    {
        Ok(type_id) => Ok(type_id.as_ref().to_owned()),
        Err(err) => {
            tracing::warn!(
                target: "tr_plugin",
                tenant_type_uuid = %type_uuid,
                error = %err,
                "tenant_type uuid -> chained-id resolution failed"
            );
            Err(TenantResolverError::Internal(
                "tenant resolver tenant_type hydration failed".to_owned(),
            ))
        }
    }
}

/// Batched companion to [`resolve_tenant_type_one`]. Issues one
/// batch read by UUID for the *distinct* uuids
/// supplied so latency scales with pages, not rows.
///
/// Per DESIGN §3.4 / §5: any per-UUID resolution failure fails the
/// entire call with `TenantResolverError::Internal`.
async fn resolve_tenant_types_for_uuids(
    registry: &Arc<dyn PlatformTypesRegistryApi>,
    uuids: impl IntoIterator<Item = Uuid>,
) -> Result<HashMap<Uuid, String>, TenantResolverError> {
    let mut distinct: Vec<Uuid> = uuids.into_iter().collect();
    distinct.sort_unstable();
    distinct.dedup();
    if distinct.is_empty() {
        return Ok(HashMap::new());
    }
    let resolved = registry
        .resolve_type_ids(&PlatformSecurityContext::outbound_marker(), &distinct)
        .await
        .map_err(|err| {
            tracing::warn!(
                target: "tr_plugin",
                error = %err,
                "tenant_type uuid -> chained-id batch resolution failed"
            );
            TenantResolverError::Internal("tenant resolver tenant_type hydration failed".to_owned())
        })?;
    let mut out: HashMap<Uuid, String> = HashMap::with_capacity(resolved.len());
    for (uuid, type_id) in resolved {
        let Some(type_id) = type_id else {
            tracing::warn!(
                target: "tr_plugin",
                tenant_type_uuid = %uuid,
                "tenant_type uuid is not registered; chained-id resolution failed"
            );
            return Err(TenantResolverError::Internal(
                "tenant resolver tenant_type hydration failed".to_owned(),
            ));
        };
        out.insert(uuid, type_id.as_ref().to_owned());
    }
    Ok(out)
}

/// `get_tenant`.
pub(super) async fn get_tenant(
    port: &Arc<dyn TenantHierarchyReadPort>,
    registry: &Arc<dyn PlatformTypesRegistryApi>,
    id: TenantId,
) -> Result<TenantInfo, TenantResolverError> {
    let row = port
        .get(id.0)
        .await
        .map_err(|e| domain_err_to_tr_err(&e))?
        .ok_or(TenantResolverError::TenantNotFound { tenant_id: id })?;
    let tenant_type = resolve_tenant_type_one(registry, row.tenant_type_uuid).await?;
    row_to_tenant_info(row, Some(tenant_type)).ok_or_else(|| projection_internal(id.0))
}

/// `get_root_tenant`.
pub(super) async fn get_root_tenant(
    port: &Arc<dyn TenantHierarchyReadPort>,
    registry: &Arc<dyn PlatformTypesRegistryApi>,
) -> Result<TenantInfo, TenantResolverError> {
    // Port returns AT MOST 2 rows so the plugin can distinguish
    // 0 / 1 / many without an unbounded scan.
    let mut roots = port
        .get_root()
        .await
        .map_err(|e| domain_err_to_tr_err(&e))?;

    match roots.len() {
        0 => {
            tracing::warn!(
                target: "tr_plugin",
                "am storage has no non-provisioning root tenant \
                 (bootstrap incomplete or hierarchy corrupt)"
            );
            Err(TenantResolverError::Internal(
                "tenant resolver root tenant unavailable".to_owned(),
            ))
        }
        1 => {
            let row = roots.swap_remove(0);
            let id = row.id;
            let tenant_type = resolve_tenant_type_one(registry, row.tenant_type_uuid).await?;
            row_to_tenant_info(row, Some(tenant_type)).ok_or_else(|| projection_internal(id))
        }
        _ => {
            tracing::warn!(
                target: "tr_plugin",
                "am storage single-root invariant violated: \
                 found multiple non-provisioning root tenants"
            );
            Err(TenantResolverError::Internal(
                "tenant resolver root tenant invariant violated".to_owned(),
            ))
        }
    }
}

/// `get_tenants`.
pub(super) async fn get_tenants(
    port: &Arc<dyn TenantHierarchyReadPort>,
    registry: &Arc<dyn PlatformTypesRegistryApi>,
    ids: &[TenantId],
    status_filter: &[SdkTenantStatus],
) -> Result<Vec<TenantInfo>, TenantResolverError> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    // Dedup while preserving membership; output order is not required
    // to match input order per the SDK contract.
    let mut seen: HashSet<Uuid> = HashSet::with_capacity(ids.len());
    let unique_ids: Vec<Uuid> = ids
        .iter()
        .filter_map(|id| seen.insert(id.0).then_some(id.0))
        .collect();

    let filter = sdk_statuses_to_filter(status_filter);
    let rows = port
        .get_bulk(&unique_ids, &filter)
        .await
        .map_err(|e| domain_err_to_tr_err(&e))?;

    let type_strings =
        resolve_tenant_types_for_uuids(registry, rows.iter().map(|r| r.tenant_type_uuid)).await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let id = row.id;
        let tenant_type = type_strings.get(&row.tenant_type_uuid).cloned();
        out.push(row_to_tenant_info(row, tenant_type).ok_or_else(|| projection_internal(id))?);
    }
    Ok(out)
}

/// `is_ancestor`.
pub(super) async fn is_ancestor(
    port: &Arc<dyn TenantHierarchyReadPort>,
    ancestor_id: TenantId,
    descendant_id: TenantId,
    barrier_mode: SdkBarrierMode,
) -> Result<bool, TenantResolverError> {
    // Existence probe of both endpoints. The single-endpoint case
    // uses `port.get` (1 row); the two-endpoint case uses
    // `port.get_bulk` (a single bulk read) so the round-trip count
    // stays at 2 indexed reads (existence probe + closure probe)
    // matching DESIGN §3.6 `seq-is-ancestor`.
    if ancestor_id == descendant_id {
        let exists = port
            .get(ancestor_id.0)
            .await
            .map_err(|e| domain_err_to_tr_err(&e))?
            .is_some();
        if !exists {
            return Err(TenantResolverError::TenantNotFound {
                tenant_id: ancestor_id,
            });
        }
        // Emit audit after the visibility probe and before the
        // self-reference short-circuit so the stream records the
        // (successfully probed) bypass exactly once.
        audit_barrier_bypass(
            "is_ancestor",
            barrier_mode,
            ancestor_id.0,
            Some(descendant_id.0),
        );
        // Strict-descendant contract: self is not an ancestor of self.
        return Ok(false);
    }

    let probes = port
        .get_bulk(&[ancestor_id.0, descendant_id.0], &StatusFilter::VisibleAll)
        .await
        .map_err(|e| domain_err_to_tr_err(&e))?;
    let visible: HashSet<Uuid> = probes.iter().map(|r| r.id).collect();
    if !visible.contains(&ancestor_id.0) || !visible.contains(&descendant_id.0) {
        // SDK contract: `TenantNotFound` when either endpoint is
        // absent. We blame the ancestor when it's the missing one;
        // otherwise the descendant.
        let missing = if visible.contains(&ancestor_id.0) {
            descendant_id
        } else {
            ancestor_id
        };
        return Err(TenantResolverError::TenantNotFound { tenant_id: missing });
    }

    // Both endpoints are visible and distinct -> the bypass actually
    // executes. Emit the audit signal here (after the visibility +
    // self-reference probes) rather than at function entry so the
    // `tr_plugin.audit` stream records only completed bypasses, not
    // requests rejected at the existence probe.
    audit_barrier_bypass(
        "is_ancestor",
        barrier_mode,
        ancestor_id.0,
        Some(descendant_id.0),
    );

    port.is_ancestor(
        ancestor_id.0,
        descendant_id.0,
        map_barrier_mode(barrier_mode),
    )
    .await
    .map_err(|e| domain_err_to_tr_err(&e))
}

/// `get_ancestors`.
pub(super) async fn get_ancestors(
    port: &Arc<dyn TenantHierarchyReadPort>,
    registry: &Arc<dyn PlatformTypesRegistryApi>,
    id: TenantId,
    barrier_mode: SdkBarrierMode,
) -> Result<GetAncestorsResponse, TenantResolverError> {
    let starting = port
        .get(id.0)
        .await
        .map_err(|e| domain_err_to_tr_err(&e))?
        .ok_or(TenantResolverError::TenantNotFound { tenant_id: id })?;
    // Emit the bypass audit AFTER the visibility probe so the audit
    // stream records only requests that actually proceed (not
    // those rejected with `TenantNotFound`).
    audit_barrier_bypass("get_ancestors", barrier_mode, id.0, None);

    let ancestor_ids = port
        .get_ancestors(id.0, map_barrier_mode(barrier_mode))
        .await
        .map_err(|e| domain_err_to_tr_err(&e))?;

    // Hydrate ancestors via the port. Per AM's invariant
    // (`cpt-cf-account-management-fr-tenant-closure-storage-floor`)
    // every closure row's `ancestor_id` must resolve to a non-
    // provisioning `tenants` row, because provisioning tenants
    // carry no closure rows in either direction. A hydration miss
    // signals storage corruption and we fail closed with `Internal`
    // rather than silently truncating the chain.
    let ancestor_rows = port
        .get_bulk(&ancestor_ids, &StatusFilter::VisibleAll)
        .await
        .map_err(|e| domain_err_to_tr_err(&e))?;
    let hydrated_ids: HashSet<Uuid> = ancestor_rows.iter().map(|r| r.id).collect();
    let closure_ids: HashSet<Uuid> = ancestor_ids.iter().copied().collect();
    if hydrated_ids != closure_ids {
        let missing: Vec<Uuid> = closure_ids.difference(&hydrated_ids).copied().collect();
        tracing::warn!(
            target: "tr_plugin",
            pivot = %id.0,
            missing = ?missing,
            "tenant_closure references ancestor ids that did not resolve to visible tenants rows; \
             AM hierarchy invariant violated"
        );
        return Err(TenantResolverError::Internal(
            "tenant resolver hierarchy invariant violated".to_owned(),
        ));
    }

    // Order: depth DESC (direct parent first, root last) with `id` ASC
    // as tie-break.
    let mut sorted = ancestor_rows;
    sorted.sort_by(|a, b| b.depth.cmp(&a.depth).then_with(|| a.id.cmp(&b.id)));

    let type_strings = resolve_tenant_types_for_uuids(
        registry,
        std::iter::once(starting.tenant_type_uuid).chain(sorted.iter().map(|r| r.tenant_type_uuid)),
    )
    .await?;

    let starting_ref = row_to_tenant_ref(
        &starting,
        type_strings.get(&starting.tenant_type_uuid).cloned(),
    )
    .ok_or_else(|| projection_internal(id.0))?;
    let mut ancestors = Vec::with_capacity(sorted.len());
    for row in &sorted {
        let tt = type_strings.get(&row.tenant_type_uuid).cloned();
        ancestors.push(row_to_tenant_ref(row, tt).ok_or_else(|| projection_internal(row.id))?);
    }
    Ok(GetAncestorsResponse {
        tenant: starting_ref,
        ancestors,
    })
}

/// `get_descendants`.
///
/// # Filtering split: graph (system invariants) vs emission (caller predicate)
///
/// The port returns the barrier-bounded SDK-visible subtree
/// (system invariants only). Caller `status_filter` is applied as
/// an **emission** predicate during the in-memory walk: the graph
/// remains coherent even when an intermediate parent fails the
/// caller's filter (e.g. `Root -> Suspended -> Active` filtered by
/// `[Active]` correctly yields `Active`).
///
/// # Cycle protection
///
/// A `visited: HashSet<Uuid>` short-circuits any revisit through
/// the `parent_id` graph. The starting tenant is pre-marked so a
/// back-edge to it cannot revisit.
#[allow(
    clippy::cognitive_complexity,
    reason = "single linear pipeline (closure scan -> bulk hydrate -> graph build -> \
              tenant_type batch resolve -> DFS with cycle/depth/emit predicates) whose \
              stages share state; splitting would only obscure the per-stage docstrings above"
)]
pub(super) async fn get_descendants(
    port: &Arc<dyn TenantHierarchyReadPort>,
    registry: &Arc<dyn PlatformTypesRegistryApi>,
    id: TenantId,
    barrier_mode: SdkBarrierMode,
    status_filter: &[SdkTenantStatus],
    max_depth: Option<u32>,
) -> Result<GetDescendantsResponse, TenantResolverError> {
    let starting = port
        .get(id.0)
        .await
        .map_err(|e| domain_err_to_tr_err(&e))?
        .ok_or(TenantResolverError::TenantNotFound { tenant_id: id })?;
    audit_barrier_bypass("get_descendants", barrier_mode, id.0, None);

    let descendant_ids = port
        .get_descendants(id.0, map_barrier_mode(barrier_mode))
        .await
        .map_err(|e| domain_err_to_tr_err(&e))?;

    let descendant_rows = port
        .get_bulk(&descendant_ids, &StatusFilter::VisibleAll)
        .await
        .map_err(|e| domain_err_to_tr_err(&e))?;
    let hydrated_ids: HashSet<Uuid> = descendant_rows.iter().map(|r| r.id).collect();
    let closure_ids: HashSet<Uuid> = descendant_ids.iter().copied().collect();
    if hydrated_ids != closure_ids {
        let missing: Vec<Uuid> = closure_ids.difference(&hydrated_ids).copied().collect();
        tracing::warn!(
            target: "tr_plugin",
            pivot = %id.0,
            missing = ?missing,
            "tenant_closure references descendant ids that did not \
             resolve to visible tenants rows; AM hierarchy invariant \
             violated"
        );
        return Err(TenantResolverError::Internal(
            "tenant resolver hierarchy invariant violated".to_owned(),
        ));
    }

    // Caller's status filter as O(1) emission predicate.
    let user_statuses: Option<HashSet<SdkTenantStatus>> = if status_filter.is_empty() {
        None
    } else {
        Some(status_filter.iter().copied().collect())
    };
    let emit_allowed = |status: SdkTenantStatus| -> bool {
        user_statuses
            .as_ref()
            .is_none_or(|set| set.contains(&status))
    };

    // Build id->row and parent_id->children maps from the full
    // barrier-bounded SDK-visible subtree. Sorting children by id
    // ASC gives a deterministic SDK pre-order without per-node
    // sorting at walk time.
    let row_by_id: HashMap<Uuid, TenantModel> =
        descendant_rows.into_iter().map(|r| (r.id, r)).collect();
    let mut children_by_parent: HashMap<Uuid, Vec<Uuid>> = HashMap::with_capacity(row_by_id.len());
    for row in row_by_id.values() {
        if let Some(parent) = row.parent_id {
            children_by_parent.entry(parent).or_default().push(row.id);
        }
    }
    for v in children_by_parent.values_mut() {
        v.sort_unstable();
    }

    let type_strings = resolve_tenant_types_for_uuids(
        registry,
        std::iter::once(starting.tenant_type_uuid)
            .chain(row_by_id.values().map(|r| r.tenant_type_uuid)),
    )
    .await?;

    // Pre-order walk from the starting tenant.
    let mut emitted: Vec<TenantRef> = Vec::with_capacity(row_by_id.len());
    let mut stack: Vec<(Uuid, u32)> = Vec::new();
    let mut visited: HashSet<Uuid> = HashSet::with_capacity(row_by_id.len() + 1);
    visited.insert(id.0);

    if let Some(initial_children) = children_by_parent.get(&id.0) {
        for child in initial_children.iter().rev() {
            stack.push((*child, 1));
        }
    }
    while let Some((node_id, depth)) = stack.pop() {
        if !visited.insert(node_id) {
            tracing::warn!(
                target: "tr_plugin",
                tenant_id = %node_id,
                pivot = %id.0,
                "tenant_closure / parent_id graph revisit detected during pre-order walk; \
                 hierarchy invariant violated"
            );
            return Err(TenantResolverError::Internal(
                "tenant resolver hierarchy invariant violated".to_owned(),
            ));
        }
        if max_depth.is_some_and(|limit| depth > limit) {
            continue;
        }
        let Some(row) = row_by_id.get(&node_id) else {
            tracing::warn!(
                target: "tr_plugin",
                tenant_id = %node_id,
                pivot = %id.0,
                "pre-order walk reached tenant id absent from \
                 row_by_id; hierarchy invariant violated"
            );
            return Err(TenantResolverError::Internal(
                "tenant resolver hierarchy invariant violated".to_owned(),
            ));
        };
        let Some(sdk_status) = map_status_to_sdk(row.status) else {
            return Err(projection_internal(node_id));
        };
        if emit_allowed(sdk_status) {
            let tt = type_strings.get(&row.tenant_type_uuid).cloned();
            emitted.push(row_to_tenant_ref(row, tt).ok_or_else(|| projection_internal(node_id))?);
        }
        if let Some(children) = children_by_parent.get(&node_id) {
            for child in children.iter().rev() {
                stack.push((*child, depth.saturating_add(1)));
            }
        }
    }

    // Completeness check (see plan / DESIGN). Bounded by max_depth
    // so closure descendants intentionally outside the walk
    // envelope (legit depth-bound trim) are not flagged as
    // corruption.
    //
    // `TenantModel.depth` is `u32`, so the bound math runs in `u32`
    // arithmetic with `saturating_add`.
    let bound_inclusive: Option<u32> = max_depth.map(|n| starting.depth.saturating_add(n));
    let unreached: Vec<Uuid> = row_by_id
        .iter()
        .filter(|(closure_id, row)| {
            !visited.contains(*closure_id) && bound_inclusive.is_none_or(|cap| row.depth <= cap)
        })
        .map(|(closure_id, _)| *closure_id)
        .collect();
    if !unreached.is_empty() {
        tracing::warn!(
            target: "tr_plugin",
            pivot = %id.0,
            unreached = ?unreached,
            max_depth = ?max_depth,
            "tenant_closure descendants not reachable from pivot via parent_id graph; \
             hierarchy invariant violated"
        );
        return Err(TenantResolverError::Internal(
            "tenant resolver hierarchy invariant violated".to_owned(),
        ));
    }

    let starting_ref = row_to_tenant_ref(
        &starting,
        type_strings.get(&starting.tenant_type_uuid).cloned(),
    )
    .ok_or_else(|| projection_internal(id.0))?;
    Ok(GetDescendantsResponse {
        tenant: starting_ref,
        descendants: emitted,
    })
}
