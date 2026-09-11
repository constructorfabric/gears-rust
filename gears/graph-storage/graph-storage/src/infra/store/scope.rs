//! Scope replacement: removing the static content a re-import no longer names
//! (`cpt-cf-graph-storage-fr-scope-replace`).
//!
//! A scope is `(attribute, value)` over a payload field — `repository =
//! acme/infra` — and a replacement says "this batch is the whole of that
//! scope now". What it removes is bounded three ways, and every one of them
//! is the reason the feature exists rather than a detail of it:
//!
//! - **Only scope-managed types.** The `scope_managed` trait is declared per
//!   type and defaults to true; a type that sets it false (the phantom
//!   family, anything a producer marks) is never removed by another
//!   producer's re-sync.
//! - **Only what the batch did not re-supply.** Membership is the payload
//!   attribute, so a node the batch wrote is by definition still in the
//!   scope.
//! - **Never analysis-originated content.** Static edges go first, then only
//!   those nodes that have no incident edge left. A node still referenced by
//!   an analysis edge stays, because the conclusion drawn about it must
//!   survive the re-import of the thing it was drawn about — that is
//!   `principle-provenance-survives-resync`, and the foreign key would refuse
//!   the delete anyway.
//!
//! Removal is a **hard delete**, not a tombstone, and that is deliberate: a
//! tombstoned node key is not reusable before purge (Soft Delete Contract), so
//! tombstoning here would make the next import of the same object a conflict —
//! the opposite of what a replacement is for.

use graph_storage_sdk::plugin_api::GraphStoreError;
use sea_orm::sea_query::{Expr, ExprTrait};
use sea_orm::{ColumnTrait, Condition, EntityTrait, QueryFilter};
use std::collections::BTreeSet;
use toolkit_db::secure::{DBRunner, SecureDeleteExt, SecureEntityExt};

use crate::infra::storage::entity::{edge, gts_type, node};
use crate::infra::store::map_scope_err;
use crate::infra::store::types::traits_from_json;

/// The characters a scope attribute may use.
///
/// It is rendered into the extraction expression as a literal, exactly like a
/// declared `index` path, so the alphabet is closed here rather than escaped
/// there.
fn plain(attribute: &str) -> bool {
    !attribute.is_empty()
        && attribute.len() <= 128
        && attribute
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.')
}

/// Which interned type ids are scope-managed nodes, and which are static
/// edges.
struct ScopedTypes {
    managed_nodes: Vec<i32>,
    static_edges: Vec<i32>,
}

async fn scoped_types(
    scope: &toolkit_security::AccessScope,
    tx: &impl DBRunner,
) -> Result<ScopedTypes, GraphStoreError> {
    let rows = gts_type::Entity::find()
        .secure()
        .scope_with(scope)
        .all(tx)
        .await
        .map_err(map_scope_err)?;

    let mut managed_nodes = Vec::new();
    let mut static_edges = Vec::new();
    for row in rows {
        let traits = traits_from_json(&row.effective_traits);
        match row.kind.as_str() {
            "node" if traits.scope_managed => managed_nodes.push(row.id),
            // The edge families: `static` content is re-derived by a re-sync,
            // `analysis` is a conclusion and survives it.
            "edge" if traits.family.as_deref() == Some("static") => static_edges.push(row.id),
            _ => {}
        }
    }
    Ok(ScopedTypes {
        managed_nodes,
        static_edges,
    })
}

/// Remove the scope's static content that this batch did not re-supply.
///
/// Runs *after* the batch's own writes, inside the same transaction and under
/// the fence row's lock: "absent from the submitted batch" can only be decided
/// once the batch is in.
pub(crate) async fn remove_stale(
    scope: &toolkit_security::AccessScope,
    tx: &impl DBRunner,
    attribute: &str,
    value: &str,
    written: &BTreeSet<String>,
) -> Result<(u64, u64), GraphStoreError> {
    if !plain(attribute) {
        return Err(GraphStoreError::InvalidQuery {
            what: format!(
                "scope attribute `{attribute}` is not a plain payload field name \
                 (`[A-Za-z0-9_.-]`, at most 128 characters)"
            ),
        });
    }
    let types = scoped_types(scope, tx).await?;
    if types.managed_nodes.is_empty() {
        return Ok((0, 0));
    }

    // Membership is the payload attribute. The field name is a checked
    // literal and the value is a bound parameter, the same shape the
    // projection renders.
    // The field name is a checked literal; the value is bound.
    let member =
        Expr::cust(format!("(payload #>> '{{{attribute}}}')")).eq(Expr::val(value.to_owned()));
    let candidates: Vec<node::Model> = node::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(
            Condition::all()
                .add(node::Column::GtsNodeTypeId.is_in(types.managed_nodes))
                .add(node::Column::DeletedAt.is_null())
                .add(member),
        )
        .all(tx)
        .await
        .map_err(map_scope_err)?;

    let stale: Vec<&node::Model> = candidates
        .iter()
        .filter(|row| !written.contains(&row.node_key))
        .collect();
    if stale.is_empty() {
        return Ok((0, 0));
    }
    let stale_ids: Vec<i64> = stale.iter().map(|row| row.id).collect();

    // Edges first, and only the static ones: an analysis edge is a conclusion
    // about the content, not a copy of it.
    let removed_edges = if types.static_edges.is_empty() {
        0
    } else {
        edge::Entity::delete_many()
            .filter(
                Condition::all()
                    .add(edge::Column::GtsEdgeTypeId.is_in(types.static_edges))
                    .add(
                        Condition::any()
                            .add(edge::Column::SrcNodeId.is_in(stale_ids.clone()))
                            .add(edge::Column::DstNodeId.is_in(stale_ids.clone())),
                    ),
            )
            .secure()
            .scope_with(scope)
            .exec(tx)
            .await
            .map_err(map_scope_err)?
            .rows_affected
    };

    // Then the nodes that nothing references any more. A node still carrying
    // an analysis edge stays: removing it would destroy the provenance the
    // edge holds, and the `ON DELETE RESTRICT` foreign key would refuse it in
    // any case — this predicate is what turns that refusal into a decision.
    let still_referenced: BTreeSet<i64> = edge::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(
            Condition::any()
                .add(edge::Column::SrcNodeId.is_in(stale_ids.clone()))
                .add(edge::Column::DstNodeId.is_in(stale_ids.clone())),
        )
        .all(tx)
        .await
        .map_err(map_scope_err)?
        .into_iter()
        .flat_map(|row| [row.src_node_id, row.dst_node_id])
        .collect();

    let removable: Vec<i64> = stale_ids
        .into_iter()
        .filter(|id| !still_referenced.contains(id))
        .collect();
    let removed_nodes = if removable.is_empty() {
        0
    } else {
        node::Entity::delete_many()
            .filter(Condition::all().add(node::Column::Id.is_in(removable)))
            .secure()
            .scope_with(scope)
            .exec(tx)
            .await
            .map_err(map_scope_err)?
            .rows_affected
    };

    Ok((removed_nodes, removed_edges))
}
