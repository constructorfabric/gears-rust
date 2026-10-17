// Created: 2026-10-08 by Virtuozzo International GmbH
//! One page and the size of the set it was cut from, read under one predicate.
//!
//! Every listing of this gear answers with a `total_count` beside its
//! cursors. The count has to describe the same set the page is cut from, or
//! a client would show one number and page through another; so the two
//! queries are built here from one base select, the `$filter` applied to
//! both, and nothing applied to the count that the page does not share — the
//! cursor in particular, which bounds a page and not the walk.

use sea_orm::{EntityTrait, QueryFilter, Select};
use toolkit_db::odata::{LimitCfg, ODataFieldMapping, filter_node_to_condition, paginate_odata};
use toolkit_db::secure::{DBRunner, ScopableEntity, SecureEntityExt};
use toolkit_odata::filter::{FilterField, convert_expr_to_filter_node};
use toolkit_odata::{Error as ODataError, ODataQuery, SortDir};
use toolkit_security::AccessScope;

use crate::domain::odata::Listing;

/// Page `select` as [`paginate_odata`] does, and count the rows the walk
/// holds in all — under the caller's scope and the query's `$filter`, never
/// under its cursor.
///
/// The count runs first and apart from the page: a row written between the
/// two is in one and not the other, which a client reading a total "at the
/// time of the read" expects and a transaction here would only pretend to
/// prevent across the pages of one walk.
///
/// # Errors
/// [`ODataError`] as [`paginate_odata`] reports it: an unmapped filter field,
/// an undecodable or mismatched cursor, or the database failing.
pub async fn list_counted<F, M, E, D, Mapper, C>(
    select: Select<E>,
    scope: &AccessScope,
    conn: &C,
    query: &ODataQuery,
    tiebreaker: (&str, SortDir),
    limit_cfg: LimitCfg,
    model_to_domain: Mapper,
) -> Result<Listing<D>, ODataError>
where
    F: FilterField,
    M: ODataFieldMapping<F, Entity = E>,
    E: EntityTrait + ScopableEntity,
    E::Column: sea_orm::ColumnTrait + Copy,
    E::Model: sea_orm::FromQueryResult + Send + Sync,
    Mapper: Fn(E::Model) -> D,
    C: DBRunner,
{
    let mut counted = select.clone();
    if let Some(ast) = query.filter.as_deref() {
        let node = convert_expr_to_filter_node::<F>(ast)
            .map_err(|e| ODataError::InvalidFilter(e.to_string()))?;
        counted = counted
            .filter(filter_node_to_condition::<F, M>(&node).map_err(ODataError::InvalidFilter)?);
    }
    let total_count = counted
        .secure()
        .scope_with(scope)
        .count(conn)
        .await
        .map_err(|e| ODataError::Db(e.to_string()))?;
    let page = paginate_odata::<F, M, E, D, Mapper, C>(
        select.secure().scope_with(scope),
        conn,
        query,
        tiebreaker,
        limit_cfg,
        model_to_domain,
    )
    .await?;
    Ok(Listing {
        items: page.items,
        page_info: page.page_info,
        total_count,
    })
}
