use sea_orm::{ColumnTrait, Condition, EntityTrait, sea_query::Expr};

use crate::secure::{AccessScope, ScopableEntity};
use modkit_security::access_scope::{ScopeConstraint, ScopeFilter, ScopeValue};

/// Convert a [`ScopeValue`] to a `sea_query::SimpleExpr` for SQL binding.
fn scope_value_to_sea_expr(v: &ScopeValue) -> sea_orm::sea_query::SimpleExpr {
    match v {
        ScopeValue::Uuid(u) => Expr::value(*u),
        ScopeValue::String(s) => Expr::value(s.clone()),
        ScopeValue::Int(n) => Expr::value(*n),
        ScopeValue::Bool(b) => Expr::value(*b),
    }
}

/// Convert a slice of [`ScopeValue`] to `Vec<sea_orm::Value>` for IN clauses.
fn scope_values_to_sea_values(values: &[ScopeValue]) -> Vec<sea_orm::Value> {
    values
        .iter()
        .map(|v| match v {
            ScopeValue::Uuid(u) => sea_orm::Value::from(*u),
            ScopeValue::String(s) => sea_orm::Value::from(s.clone()),
            ScopeValue::Int(n) => sea_orm::Value::from(*n),
            ScopeValue::Bool(b) => sea_orm::Value::from(*b),
        })
        .collect()
}

/// Build a deny-all condition (`WHERE false`).
fn deny_all() -> Condition {
    Condition::all().add(Expr::value(false))
}

/// Builds a `SeaORM` `Condition` from an `AccessScope` using property resolution.
///
/// # OR/AND Semantics
///
/// - Multiple constraints are OR-ed (alternative access paths)
/// - Filters within a constraint are AND-ed (all must match)
/// - Unknown `pep_properties` fail that constraint (fail-closed)
/// - If all constraints fail resolution, deny-all
///
/// # Policy Rules
///
/// | Scope | Behavior |
/// |-------|----------|
/// | deny-all (default) | `WHERE false` |
/// | unconstrained (allow-all) | No filtering (`WHERE true`) |
/// | single constraint | AND of resolved filters |
/// | multiple constraints | OR of ANDed filter groups |
pub fn build_scope_condition<E>(scope: &AccessScope) -> Condition
where
    E: ScopableEntity + EntityTrait,
    E::Column: ColumnTrait + Copy,
{
    if scope.is_unconstrained() {
        return Condition::all();
    }
    if scope.is_deny_all() {
        return deny_all();
    }

    let compiled: Vec<Condition> = scope
        .constraints()
        .iter()
        .filter_map(build_constraint_condition::<E>)
        .collect();

    match compiled.len() {
        0 => deny_all(),
        1 => compiled.into_iter().next().unwrap_or_else(deny_all),
        _ => {
            let mut or_cond = Condition::any();
            for c in compiled {
                or_cond = or_cond.add(c);
            }
            or_cond
        }
    }
}

/// Build SQL for a single constraint (AND of filters).
///
/// Returns `None` if any filter references an unknown property (fail-closed).
fn build_constraint_condition<E>(constraint: &ScopeConstraint) -> Option<Condition>
where
    E: ScopableEntity + EntityTrait,
    E::Column: ColumnTrait + Copy,
{
    if constraint.is_empty() {
        return Some(Condition::all());
    }
    let mut and_cond = Condition::all();
    for filter in constraint.filters() {
        let col = E::resolve_property(filter.property())?;
        match filter {
            ScopeFilter::Eq(eq) => {
                let expr = scope_value_to_sea_expr(eq.value());
                and_cond = and_cond.add(col.into_expr().eq(expr));
            }
            ScopeFilter::In(inf) => {
                let sea_values = scope_values_to_sea_values(inf.values());
                and_cond = and_cond.add(col.is_in(sea_values));
            }
        }
    }
    Some(and_cond)
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "cond_tests.rs"]
mod tests;
