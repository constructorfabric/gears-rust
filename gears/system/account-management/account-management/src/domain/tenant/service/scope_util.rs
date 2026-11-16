//! Scope transformations used by AM's identity-level read surface
//! ([`TenantService::list_children`], [`TenantService::get_tenant`]) to
//! materialise the **direct-child carve-out across self-managed
//! barriers** — the contract that lets `P` learn the *identity* of any
//! direct child of any tenant `P` already sees, including self-managed
//! children, without leaking anything below that child's barrier.
//!
//! # Background
//!
//! AM's PDP returns `InTenantSubtree(...)` constraints with the default
//! `BarrierMode::Respect` (see [`InTenantSubtreeScopeFilter::new`] in
//! `toolkit-security`), which compiles at the secure-ORM seam to
//!
//! ```text
//! id IN (SELECT descendant_id FROM tenant_closure
//!        WHERE ancestor_id = $root AND barrier = 0)
//! ```
//!
//! `barrier = 0` excludes every row whose `(ancestor, descendant]` path
//! crosses a `self_managed = true` tenant — including the self-managed
//! direct child itself, because the closure rule counts the descendant
//! `D` in the "strict path" set. Result: a caller scoped to `R` does
//! not see `R`'s self-managed direct children through any read at all.
//!
//! For *identity-level* reads we want the opposite: the existence and
//! identity of direct children must be visible, but anything below
//! that boundary stays private. The mechanism — implemented by
//! [`relax_barriers`] below — is to keep the PDP-emitted scope as the
//! authorization *gate* (a Respect-scope lookup naturally collapses
//! any past-barrier target to `NotFound`) and run only the
//! *enumeration* / *fallback lookup* under a barrier-relaxed clone of
//! that same scope. Combined with the depth-1 SQL pin
//! `tenants.parent_id = $target` in [`list_children`](super::TenantService::list_children),
//! or with the explicit parent-reachability re-check in
//! [`get_tenant`](super::TenantService::get_tenant), this can never
//! widen access below a barrier.
//!
//! # Invariants the carve-out preserves
//!
//! | Caller `P` accessing | Behaviour | Reason |
//! |---|---|---|
//! | `list_children(P)` | sees self-managed direct child `S` | gate (P) passes Respect; listing `parent_id = P` pins depth 1 |
//! | `list_children(X)` for `X` Respect-visible | sees `X`'s self-managed direct children | same |
//! | `list_children(S)` (`S` past barrier) | `NotFound` | gate fails (S unreachable under Respect) |
//! | `get_tenant(S)` | row returned | fallback finds S, then re-checks `S.parent_id` is Respect-visible (it is — `P`) |
//! | `get_tenant(GC)` (`GC.parent = S`, `S` past barrier) | `NotFound` | fallback finds GC, parent-recheck against `S` fails (S unreachable) |
//! | `update_tenant(S)` / `suspend_tenant(S)` / `delete_tenant(S)` | `NotFound` | unchanged — mutation surface keeps the original Respect scope |
//! | `list_metadata(S)` / `list_users(S)` | unchanged (barrier-clamped) | helper not applied on those surfaces |
//!
//! # Why not flip `BarrierMode::Ignore` at the PDP for these actions
//!
//! A per-action PDP flip would be applied uniformly to *every* repo
//! call inside the service method — including the parent-existence
//! gate. That would let `P` set `target = S` and have the gate accept
//! `S` (now reachable under barrier-ignore), then list `S`'s direct
//! children — exactly the subtree leak the barrier was designed to
//! prevent. By keeping the relaxation a service-layer transformation
//! applied only to the *enumeration* call, the gate stays
//! barrier-respecting and the carve-out is bounded to "direct children
//! of a Respect-reachable tenant".

use toolkit_security::{AccessScope, InTenantSubtreeScopeFilter, ScopeConstraint, ScopeFilter};

/// Clone `scope`, flipping every [`ScopeFilter::InTenantSubtree`] to
/// `respect_barriers = false`. All other filter shapes, constraint
/// composition, the per-filter `descendant_status` list, and
/// `allow_all` / `deny_all` behavior is preserved. The result is a fresh scope.
///
/// Intended as a service-layer-only transformation on the
/// PDP-emitted scope, applied only to the *enumeration* / *fallback
/// lookup* sub-query of an identity-level read. See the gear-level
/// docs for why this must not be expressed as a PDP-side
/// `BarrierMode::Ignore`.
#[must_use]
pub(super) fn relax_barriers(scope: &AccessScope) -> AccessScope {
    if scope.is_unconstrained() {
        return AccessScope::allow_all();
    }
    if scope.is_deny_all() {
        return AccessScope::deny_all();
    }
    let constraints = scope
        .constraints()
        .iter()
        .map(|c| {
            let filters = c
                .filters()
                .iter()
                .map(|f| match f {
                    ScopeFilter::InTenantSubtree(its) => ScopeFilter::InTenantSubtree(
                        InTenantSubtreeScopeFilter::with_descendant_status(
                            its.property().to_owned(),
                            its.root_tenant_id().clone(),
                            false,
                            its.descendant_status().to_vec(),
                        ),
                    ),
                    other => other.clone(),
                })
                .collect();
            ScopeConstraint::new(filters)
        })
        .collect();
    AccessScope::from_constraints(constraints)
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "scope_util_tests.rs"]
mod tests;
