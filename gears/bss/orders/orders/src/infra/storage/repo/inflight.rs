//! The D-180 interim addend: for one payer, the lines of *other* in-flight orders that carry an
//! overlap scope key under a live claim held by an order of **another resource tenant** (F03
//! §4.2 predicate 7, the cross-customer case; DESIGN §3.7 `orders_inflight_overlap_claim`).
//! Same-resource siblings are read separately by [`held_same_tuple`] for predicate 9. Both
//! queries exclude the requester and run under [`PrivateScope::for_payer_count`] (D-225 item 6).
//! The count returns only counts; the same-tuple read returns only requested keys, never any
//! holder identity. Read-only and outside the transition transaction: step 17 is authoritative.
//!
//! Count unit (F03 predicate 7): the lines carrying the key on each holder's current version. A
//! claim is reached only through an inner join on its `order_id` foreign key from the orders the
//! scope admits (as `claims::visible_holder` reads holders), counted only when its `version` is
//! the holder's `current_version`, and joined to that version's `order_line` rows with the same
//! `overlap_scope_key`; every row counted is reached through the secured select.
use super::{DBRunner, EntityTrait, QuerySelect, ScopeError, SecureEntityExt, order};
use crate::domain::overlap::ClaimParties;
use crate::infra::storage::entity::inflight_overlap_claim as claim;
use crate::infra::storage::entity::order_line as line;
use crate::infra::storage::scoped::PrivateScope;
use sea_orm::sea_query::{
    Alias, Expr, ExprTrait, Func, IntoCondition, extension::postgres::PgExpr,
};
use sea_orm::{ColumnTrait, JoinType, QueryFilter, RelationTrait};
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;

#[derive(Debug, sea_orm::FromQueryResult)]
struct HeldKey {
    overlap_scope_key: String,
}

#[derive(Debug, sea_orm::FromQueryResult)]
struct KeyLines {
    overlap_scope_key: String,
    lines: i64,
}

/// For each of `keys`, the current-version lines carrying it on orders `scope` admits that
/// hold a live claim (`released_at IS NULL`) on `(payer, key)` under a resource tenant other
/// than the tuple's resource, excluding `exclude_order`. Every key asked has an entry (zero when no line
/// counts); an empty key set reads nothing.
///
/// # Errors
/// Scope/store failure.
pub async fn count_other_orders(
    runner: &impl DBRunner,
    scope: &PrivateScope,
    tuple: ClaimParties,
    keys: &[String],
    exclude_order: Option<Uuid>,
) -> Result<BTreeMap<String, u32>, ScopeError> {
    let mut counts: BTreeMap<String, u32> = keys.iter().map(|k| (k.clone(), 0)).collect();
    if counts.is_empty() {
        return Ok(counts);
    }
    // The holder's current-version lines with the claimed key: `order_line` joins the order on
    // `order_id`, narrowed to the claim's version and key.
    let lines_of_claim = line::Relation::Order.def().on_condition(|_, _| {
        Expr::col((line::Entity, line::Column::Version))
            .eq(Expr::col((claim::Entity, claim::Column::Version)))
            .and(
                Expr::col((line::Entity, line::Column::OverlapItemKeys))
                    .is_null()
                    .and(
                        Expr::col((line::Entity, line::Column::OverlapScopeKey))
                            .eq(Expr::col((claim::Entity, claim::Column::OverlapScopeKey))),
                    )
                    .or(
                        Expr::col((line::Entity, line::Column::OverlapItemKeys)).contains(
                            Func::cust(Alias::new("jsonb_build_array")).arg(
                                Func::cust(Alias::new("jsonb_build_object"))
                                    .arg(Expr::val("overlap_scope_key"))
                                    .arg(Expr::col((
                                        claim::Entity,
                                        claim::Column::OverlapScopeKey,
                                    ))),
                            ),
                        ),
                    ),
            )
            .into_condition()
    });
    let mut query = order::Entity::find()
        .join_rev(JoinType::InnerJoin, claim::Relation::Order.def())
        .join_rev(JoinType::InnerJoin, lines_of_claim)
        .filter(claim::Column::PayerTenantId.eq(tuple.payer_tenant_id))
        .filter(claim::Column::ResourceTenantId.ne(tuple.resource_tenant_id))
        .filter(claim::Column::OverlapScopeKey.is_in(counts.keys().map(String::as_str)))
        .filter(claim::Column::ReleasedAt.is_null())
        .filter(
            Expr::col((claim::Entity, claim::Column::Version))
                .eq(Expr::col((order::Entity, order::Column::CurrentVersion))),
        );
    if let Some(order_id) = exclude_order {
        query = query.filter(order::Column::OrderId.ne(order_id));
    }
    let rows = query
        .secure()
        .scope_with(scope.access_scope())
        .project_all(runner, |q| {
            q.select_only()
                .column_as(claim::Column::OverlapScopeKey, "overlap_scope_key")
                .column_as(
                    Expr::col((line::Entity, line::Column::LineId)).count(),
                    "lines",
                )
                .group_by(claim::Column::OverlapScopeKey)
                .into_model::<KeyLines>()
        })
        .await?;
    for row in rows {
        if let Some(n) = counts.get_mut(&row.overlap_scope_key) {
            *n = u32::try_from(row.lines).unwrap_or(u32::MAX);
        }
    }
    Ok(counts)
}

/// Keys held by another order on precisely `(payer, resource, key)`. Reads only live claims,
/// including retained claims from an earlier version; never returns a holder identity.
/// # Errors
/// Scope/store failure.
pub async fn held_same_tuple(
    runner: &impl DBRunner,
    scope: &PrivateScope,
    tuple: ClaimParties,
    keys: &[String],
    exclude_order: Option<Uuid>,
) -> Result<BTreeSet<String>, ScopeError> {
    if keys.is_empty() {
        return Ok(BTreeSet::new());
    }
    let mut query = order::Entity::find()
        .join_rev(JoinType::InnerJoin, claim::Relation::Order.def())
        .filter(claim::Column::PayerTenantId.eq(tuple.payer_tenant_id))
        .filter(claim::Column::ResourceTenantId.eq(tuple.resource_tenant_id))
        .filter(claim::Column::OverlapScopeKey.is_in(keys.iter().map(String::as_str)))
        .filter(claim::Column::ReleasedAt.is_null());
    if let Some(id) = exclude_order {
        query = query.filter(order::Column::OrderId.ne(id));
    }
    let rows = query
        .secure()
        .scope_with(scope.access_scope())
        .project_all(runner, |q| {
            q.select_only()
                .column(claim::Column::OverlapScopeKey)
                .distinct()
                .into_model::<HeldKey>()
        })
        .await?;
    Ok(rows.into_iter().map(|r| r.overlap_scope_key).collect())
}
