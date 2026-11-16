// Updated: 2026-10-06 by Constructor Tech
//! Scope helpers over the tenant and row properties of a PDP decision.
//!
//! The repository gates every read and write on the caller's own tenant being
//! within the scope the PDP returned; the PEP calls themselves arrive with the
//! domain service.

use toolkit_security::{AccessScope, ScopeConstraint, ScopeFilter, pep_properties};
use uuid::Uuid;

/// PDP property carrying the credential's type (ADR-0010). Its value is the
/// deterministic v5 UUID of the type's GTS id (`credstore_sdk::type_uuid`) -
/// the representation the row stores - and the entity maps it to the
/// `secret_type_uuid` column, so a PDP constraint on it compiles to a SQL
/// predicate.
pub const SECRET_TYPE_PROP: &str = "secret_type";

/// PDP property carrying the credential's reference (its per-tenant name).
/// Per-instance grants are expressed by reference, not by record id: a
/// re-created record gets a new id, so an id-based grant would silently stop
/// matching. The entity maps it to the `reference` column.
pub const REFERENCE_PROP: &str = "reference";

/// The row properties a PDP may constrain besides the tenant.
const ROW_PROPS: [&str; 2] = [SECRET_TYPE_PROP, REFERENCE_PROP];

// ── Scope algebra over the tenant and row properties ────────────────────────

/// Whether a single constraint affirms `tenant`, ignoring the row properties.
///
/// Fail-closed tenant gate: every non-row filter must be a plain
/// `owner_tenant_id` `Eq`/`In` containing `tenant`, and at least one such
/// filter must be present. A sibling filter that narrows below tenant
/// granularity (`owner_id`, `id`, group membership, ...) or one this gate
/// cannot evaluate makes the constraint non-admitting. Row filters narrow
/// rows, not tenants - they are applied by SQL.
fn constraint_admits_tenant(constraint: &ScopeConstraint, tenant: Uuid) -> bool {
    let mut affirmed = false;
    for filter in constraint.filters() {
        if ROW_PROPS.contains(&filter.property()) {
            continue;
        }
        if filter.property() != pep_properties::OWNER_TENANT_ID {
            return false;
        }
        match filter {
            ScopeFilter::Eq(_) | ScopeFilter::In(_)
                if filter.values().iter().any(|v| v.as_uuid() == Some(tenant)) =>
            {
                affirmed = true;
            }
            // Structured subtree/group predicates are a capability-contract
            // breach (credstore advertises no capabilities), and any
            // variant added later is one this build cannot resolve.
            _ => return false,
        }
    }
    affirmed
}

/// True iff `tenant` is within `scope`, ignoring row predicates (the
/// fail-closed own-tenant gate; see `constraint_admits_tenant`).
#[must_use]
pub fn scope_admits_tenant(scope: &AccessScope, tenant: Uuid) -> bool {
    if scope.is_unconstrained() {
        return true;
    }
    scope
        .constraints()
        .iter()
        .any(|c| constraint_admits_tenant(c, tenant))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_branch_that_matches_nothing_admits_no_tenant() {
        let dead_tenant = Uuid::from_u128(1);
        let live_tenant = Uuid::from_u128(2);
        // The empty set sits on a row property, which the tenant gate
        // otherwise ignores: the branch still matches nothing.
        let dead = ScopeConstraint::new(vec![
            ScopeFilter::eq(pep_properties::OWNER_TENANT_ID, dead_tenant),
            ScopeFilter::in_uuids(REFERENCE_PROP, vec![]),
        ]);
        let live = ScopeConstraint::new(vec![ScopeFilter::eq(
            pep_properties::OWNER_TENANT_ID,
            live_tenant,
        )]);

        let mixed = AccessScope::from_constraints(vec![dead.clone(), live]);
        assert!(!scope_admits_tenant(&mixed, dead_tenant));
        assert!(scope_admits_tenant(&mixed, live_tenant));

        let all_dead = AccessScope::from_constraints(vec![dead]);
        assert!(!scope_admits_tenant(&all_dead, dead_tenant));
    }
}
