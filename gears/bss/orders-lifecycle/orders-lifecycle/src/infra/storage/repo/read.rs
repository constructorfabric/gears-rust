//! Scoped keyset reads of the early S6-01 draft subset (08 §2.2, §3.6 *List Orders* step 6,
//! *Read One Order* step 4). Every query applies the PDP scope, the validated filters and the
//! strict cursor boundary in SQL before `ORDER BY`/`LIMIT`; nothing is post-filtered. Child
//! reads bind to the current scoped parent through the aggregate join; draft membership is a
//! SQL predicate on the working set, so a removed draft identity is never a current line.
use super::{AccessScope, DBRunner, EntityTrait, ScopeError, SecureEntityExt, Uuid, order};
use crate::domain::read::Position;
use crate::infra::storage::entity::{draft_content, order_line_identity};
use bss_orders_lifecycle_sdk::reads::OrderFilters;
use sea_orm::sea_query::Query;
use sea_orm::{ColumnTrait, Condition, QueryFilter, QueryOrder, QuerySelect};

/// Rows strictly after the position in `(created_at, id)` order, microseconds and binary UUID
/// comparison preserved exactly (08 §2.2 *Exact continuation*).
fn after<C: ColumnTrait>(
    created_at: C,
    id: C,
    position: Position,
) -> Result<Condition, ScopeError> {
    let instant = position
        .instant()
        .map_err(|_| ScopeError::Invalid("cursor instant outside the stored precision"))?;
    Ok(Condition::any().add(created_at.gt(instant)).add(
        Condition::all()
            .add(created_at.eq(instant))
            .add(id.gt(position.id)),
    ))
}

/// One scoped order page: PDP scope, filters and cursor boundary in SQL, ordered
/// `(created_at, order_id)`, at most `limit` rows (the caller passes page size + 1).
///
/// # Errors
/// Scope/store failure.
pub async fn order_page(
    runner: &impl DBRunner,
    scope: &AccessScope,
    filters: &OrderFilters,
    cursor: Option<Position>,
    limit: u64,
) -> Result<Vec<order::Model>, ScopeError> {
    use order::Column;
    let mut query = order::Entity::find();
    if let Some(state) = filters.state {
        query = query.filter(Column::State.eq(crate::domain::audit::state_token(state)));
    }
    if let Some(from) = filters.created_from {
        query = query.filter(Column::CreatedAt.gte(from));
    }
    if let Some(to) = filters.created_to {
        query = query.filter(Column::CreatedAt.lt(to));
    }
    if let Some(since) = filters.state_entered_before {
        query = query.filter(Column::StateEnteredAt.lte(since));
    }
    if let Some(contract) = filters.contract_id {
        query = query.filter(Column::ContractId.eq(contract));
    }
    if let Some(position) = cursor {
        query = query.filter(after(Column::CreatedAt, Column::OrderId, position)?);
    }
    query
        .order_by_asc(Column::CreatedAt)
        .order_by_asc(Column::OrderId)
        .limit(limit)
        .secure()
        .scope_with(scope)
        .all(runner)
        .await
}

/// One page of the current parent's **draft working** line identities: identity joined to
/// membership in SQL, ordered `(created_at, line_id)` from the append-only identity row, cursor
/// boundary before `LIMIT`. Removed draft identities are excluded by the membership subquery.
///
/// # Errors
/// Scope/store failure.
pub async fn draft_line_page(
    runner: &impl DBRunner,
    scope: &AccessScope,
    order_id: Uuid,
    cursor: Option<Position>,
    limit: Option<u64>,
) -> Result<Vec<order_line_identity::Model>, ScopeError> {
    use order_line_identity::Column;
    let members = Query::select()
        .column(draft_content::Column::LineId)
        .from(draft_content::Entity)
        .and_where(draft_content::Column::OrderId.eq(order_id))
        .to_owned();
    let mut query = order_line_identity::Entity::find()
        .inner_join(order::Entity)
        .filter(Column::OrderId.eq(order_id))
        .filter(Column::LineId.in_subquery(members));
    if let Some(position) = cursor {
        query = query.filter(after(Column::CreatedAt, Column::LineId, position)?);
    }
    query = query
        .order_by_asc(Column::CreatedAt)
        .order_by_asc(Column::LineId);
    if let Some(limit) = limit {
        query = query.limit(limit);
    }
    query
        .secure()
        .scope_with(&AccessScope::for_resources(vec![order_id]))
        .and_scope_for::<order::Entity>(scope)
        .all(runner)
        .await
}
