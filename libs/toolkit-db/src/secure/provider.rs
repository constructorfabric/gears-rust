use sea_orm::{ColumnTrait, Condition, EntityTrait, ExprTrait, sea_query::Expr};

use crate::secure::{AccessScope, ScopableEntity};
use toolkit_security::pep_properties;

/// Provides tenant filtering logic for scoped queries.
///
/// This trait abstracts the tenant filtering mechanism, allowing for future
/// enhancements like hierarchical tenant structures ("effective tenants")
/// without changing calling code.
///
/// # Current Implementation
///
/// `SimpleTenantFilter` uses direct `tenant_id IN (...)` filtering.
///
/// # Future Enhancement
///
/// A `HierarchicalTenantFilter` could query "effective tenant IDs" from
/// a tenant hierarchy service and expand the filter accordingly.
pub trait TenantFilterProvider {
    /// Build a condition for tenant filtering based on the scope.
    ///
    /// Returns:
    /// - `None` only for an unconstrained scope (no tenant filtering needed)
    /// - `Some(FALSE)` for a constrained scope with no tenant values in live
    ///   branches (including deny-all and statically empty scopes)
    /// - `Some(deny_all)` if entity has no tenant column but tenants requested
    /// - `Some(filter)` with appropriate tenant IN clause
    fn tenant_condition<E>(scope: &AccessScope) -> Option<Condition>
    where
        E: ScopableEntity + EntityTrait,
        E::Column: ColumnTrait + Copy;
}

/// Tenant-only `tenant_id IN (...)` adapter using values from
/// the normalized [`AccessScope::constraints`]; does not enforce the full scope.
/// Only unconstrained scopes omit the filter. Constrained scopes without
/// projectable tenant IDs produce SQL `FALSE`.
///
/// A hierarchical provider could expand tenant IDs to include descendants.
pub struct SimpleTenantFilter;

impl TenantFilterProvider for SimpleTenantFilter {
    fn tenant_condition<E>(scope: &AccessScope) -> Option<Condition>
    where
        E: ScopableEntity + EntityTrait,
        E::Column: ColumnTrait + Copy,
    {
        if scope.matches_nothing() {
            return Some(Condition::all().add(Expr::value(false)));
        }
        let tenant_ids = scope.all_uuid_values_for(pep_properties::OWNER_TENANT_ID);

        if tenant_ids.is_empty() {
            // A constrained scope without tenant IDs must still fail closed.
            if scope.is_unconstrained() {
                return None;
            }
            return Some(Condition::all().add(Expr::value(false)));
        }

        // Entity has no tenant column but tenant IDs requested → deny all
        let Some(tcol) = E::SCOPE.tenant() else {
            return Some(Condition::all().add(Expr::value(false)));
        };

        // Build tenant IN filter
        Some(Condition::all().add(Expr::col(tcol).is_in(tenant_ids)))
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    #[test]
    fn test_provider_trait_compiles() {
        let scope = AccessScope::default();
        assert!(scope.is_deny_all());
    }
    #[test]
    fn empty_scope_is_false_not_an_absent_tenant_condition() {
        use sea_orm::sea_query::{PostgresQueryBuilder, Query};
        use toolkit_security::{ScopeConstraint, ScopeFilter};
        mod entity {
            use sea_orm::entity::prelude::*;
            #[derive(
                Clone, Debug, PartialEq, Eq, DeriveEntityModel, toolkit_db_macros::Scopable,
            )]
            #[sea_orm(table_name = "tenant_projection")]
            #[secure(tenant_col = "tenant_id", resource_col = "id", no_owner, no_type)]
            pub struct Model {
                #[sea_orm(primary_key, auto_increment = false)]
                pub id: Uuid,
                pub tenant_id: Uuid,
            }
            #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
            pub enum Relation {}
            impl ActiveModelBehavior for ActiveModel {}
        }
        let dead_tenant = uuid::Uuid::from_u128(1);
        let live_tenant = uuid::Uuid::from_u128(2);
        let dead = ScopeConstraint::new(vec![
            ScopeFilter::eq(pep_properties::OWNER_TENANT_ID, dead_tenant),
            ScopeFilter::in_uuids(pep_properties::RESOURCE_ID, vec![]),
        ]);
        for live in [false, true] {
            let mut constraints = vec![dead.clone()];
            if live {
                constraints.push(ScopeConstraint::new(vec![ScopeFilter::eq(
                    pep_properties::OWNER_TENANT_ID,
                    live_tenant,
                )]));
            }
            let scope = AccessScope::from_constraints(constraints);
            let condition = SimpleTenantFilter::tenant_condition::<entity::Entity>(&scope)
                .expect("empty scope must have an explicit FALSE condition");
            let sql = Query::select()
                .expr(Expr::value(1))
                .cond_where(condition)
                .to_string(PostgresQueryBuilder);
            assert_eq!(sql.contains("FALSE"), !live, "{sql}");
            assert_eq!(sql.contains(&live_tenant.to_string()), live, "{sql}");
            assert!(!sql.contains(&dead_tenant.to_string()), "{sql}");
        }
        assert!(
            SimpleTenantFilter::tenant_condition::<entity::Entity>(&AccessScope::allow_all())
                .is_none()
        );

        let render = |scope: &AccessScope| {
            SimpleTenantFilter::tenant_condition::<entity::Entity>(scope).map(|condition| {
                Query::select()
                    .expr(Expr::value(1))
                    .cond_where(condition)
                    .to_string(PostgresQueryBuilder)
            })
        };
        let tenant_free = ScopeConstraint::new(vec![ScopeFilter::eq(
            pep_properties::RESOURCE_ID,
            uuid::Uuid::from_u128(3),
        )]);
        // Deny-all has no tenant IDs, yet must not mean "no tenant filter".
        let deny_all = render(&AccessScope::deny_all()).expect("deny-all is FALSE");
        assert!(deny_all.contains("FALSE"), "{deny_all}");
        // A tenant found only in a dead branch cannot authorize the live branch.
        let mixed = render(&AccessScope::from_constraints(vec![
            dead,
            tenant_free.clone(),
        ]))
        .expect("a dead-only tenant reference is FALSE, not an absent filter");
        assert!(mixed.contains("FALSE"), "{mixed}");
        assert!(!mixed.contains(&dead_tenant.to_string()), "{mixed}");
        // Removing the dead branch must preserve FALSE.
        let live_only = AccessScope::from_constraints(vec![tenant_free.clone()]);
        assert_eq!(render(&live_only), Some(mixed));
        let tenant_free_only = render(&AccessScope::from_constraints(vec![tenant_free]))
            .expect("a constrained tenant-free scope is FALSE, not an absent filter");
        assert!(tenant_free_only.contains("FALSE"), "{tenant_free_only}");
    }
}
