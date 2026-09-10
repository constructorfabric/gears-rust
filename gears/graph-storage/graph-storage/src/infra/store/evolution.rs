//! The data-backed half of a type update: counting a type's live rows and
//! re-validating them against a candidate schema.
//!
//! Nothing here decides admission — [`crate::domain::evolution`] does, from
//! the schemas alone. This module answers the one question the schemas cannot:
//! do *these* rows satisfy the candidate? It is reached only when the
//! comparison could not prove inclusion and the caller asked for it, so a
//! provably compatible update never reads a row.
//!
//! Batched by keyset over the primary key rather than by OFFSET: the scan runs
//! inside the update's transaction, and an offset walk re-reads its own prefix
//! once per batch.

use graph_storage_sdk::models::{ItemError, ItemFamily, TypeKind};
use graph_storage_sdk::plugin_api::GraphStoreError;
use sea_orm::{ColumnTrait, Condition, EntityTrait};
use std::collections::BTreeMap;
use toolkit_db::secure::{DBRunner, SecureEntityExt};

use crate::domain::ontology::ChainValidator;
use crate::infra::storage::entity::{edge, node};
use crate::infra::store::map_scope_err;

/// How a re-validating scan is paced, and how much of a failure it reports.
#[derive(Clone, Copy)]
pub(crate) struct ScanBounds {
    /// Rows per batch.
    pub batch: u64,
    /// Offending rows named in the refusal. Enough to see the pattern, not
    /// enough to make the refusal itself a data export.
    pub max_reported: usize,
}

/// Live rows of one interned type.
pub(crate) async fn count_live(
    scope: &toolkit_security::AccessScope,
    tx: &impl DBRunner,
    kind: TypeKind,
    interned: i32,
) -> Result<u64, GraphStoreError> {
    match kind {
        TypeKind::Node => node::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(
                Condition::all()
                    .add(node::Column::GtsNodeTypeId.eq(interned))
                    .add(node::Column::DeletedAt.is_null()),
            )
            .count(tx)
            .await
            .map_err(map_scope_err),
        TypeKind::Edge => edge::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(
                Condition::all()
                    .add(edge::Column::GtsEdgeTypeId.eq(interned))
                    .add(edge::Column::DeletedAt.is_null()),
            )
            .count(tx)
            .await
            .map_err(map_scope_err),
        // Attributes are payload fragments, not storable rows: there is
        // nothing to re-validate, and reporting zero would read as "checked".
        TypeKind::Attribute => Err(GraphStoreError::Unsupported {
            what: "re-validating an attribute type; it has no rows",
        }),
    }
}

/// Validate every live row of the type against `validator`, reporting the
/// first `max_reported` failures.
///
/// The whole scan runs even once a failure is found: a caller fixing a model
/// wants the shape of the problem, not its first instance — the same reason
/// ingest reports every item error in one answer.
pub(crate) async fn revalidate(
    scope: &toolkit_security::AccessScope,
    tx: &impl DBRunner,
    type_id: &str,
    kind: TypeKind,
    interned: i32,
    validator: &ChainValidator,
    bounds: ScanBounds,
) -> Result<Vec<ItemError>, GraphStoreError> {
    match kind {
        TypeKind::Node => {
            revalidate_nodes(scope, tx, type_id, interned, validator, bounds).await
        }
        TypeKind::Edge => {
            revalidate_edges(scope, tx, type_id, interned, validator, bounds).await
        }
        TypeKind::Attribute => Err(GraphStoreError::Unsupported {
            what: "re-validating an attribute type; it has no rows",
        }),
    }
}

/// The instance a node row presents for validation.
///
/// Exactly what ingest validated when the row was written (`domain::service`
/// § `check_node_item`): the producer-authored document, never the
/// gear-assigned envelope. Composed here from storage rather than shared with
/// the ingest path because the two start from different shapes — but if they
/// ever disagree, a row admitted at ingest could be refused by a re-validation
/// that changed nothing, so the comment is the contract.
fn node_instance(model: &node::Model) -> serde_json::Value {
    let mut instance = serde_json::json!({
        "node_key": model.node_key,
        "type": serde_json::Value::Null,
    });
    instance["name"] = serde_json::Value::String(model.name.clone());
    instance["payload"] = model.payload.clone();
    instance
}

async fn revalidate_nodes(
    scope: &toolkit_security::AccessScope,
    tx: &impl DBRunner,
    type_id: &str,
    interned: i32,
    validator: &ChainValidator,
    bounds: ScanBounds,
) -> Result<Vec<ItemError>, GraphStoreError> {
    let mut errors: Vec<ItemError> = Vec::new();
    let mut after: i64 = i64::MIN;
    let mut index = 0usize;
    loop {
        let rows = node::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(
                Condition::all()
                    .add(node::Column::GtsNodeTypeId.eq(interned))
                    .add(node::Column::DeletedAt.is_null())
                    .add(node::Column::Id.gt(after)),
            )
            .order_by(node::Column::Id, sea_orm::Order::Asc)
            .limit(bounds.batch)
            .all(tx)
            .await
            .map_err(map_scope_err)?;
        if rows.is_empty() {
            return Ok(errors);
        }
        for model in &rows {
            after = model.id;
            let mut instance = node_instance(model);
            instance["type"] = serde_json::Value::String(type_id.to_owned());
            for (pointer, message) in validator.validate(&instance) {
                if errors.len() < bounds.max_reported {
                    errors.push(ItemError {
                        index,
                        family: ItemFamily::Node,
                        gts_type: Some(type_id.to_owned()),
                        pointer: Some(pointer),
                        message: format!("node `{}`: {message}", model.node_key),
                    });
                }
            }
            index += 1;
        }
    }
}

async fn revalidate_edges(
    scope: &toolkit_security::AccessScope,
    tx: &impl DBRunner,
    type_id: &str,
    interned: i32,
    validator: &ChainValidator,
    bounds: ScanBounds,
) -> Result<Vec<ItemError>, GraphStoreError> {
    let mut errors: Vec<ItemError> = Vec::new();
    let mut after: i64 = i64::MIN;
    let mut index = 0usize;
    loop {
        let rows = edge::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(
                Condition::all()
                    .add(edge::Column::GtsEdgeTypeId.eq(interned))
                    .add(edge::Column::DeletedAt.is_null())
                    .add(edge::Column::Id.gt(after)),
            )
            .order_by(edge::Column::Id, sea_orm::Order::Asc)
            .limit(bounds.batch)
            .all(tx)
            .await
            .map_err(map_scope_err)?;
        if rows.is_empty() {
            return Ok(errors);
        }
        // An edge's validated document names its endpoints by producer key,
        // not by internal id, so the batch resolves the keys it needs. One
        // query per batch, never one per edge.
        let mut endpoint_ids: Vec<i64> = rows
            .iter()
            .flat_map(|row| [row.src_node_id, row.dst_node_id])
            .collect();
        endpoint_ids.sort_unstable();
        endpoint_ids.dedup();
        let keys: BTreeMap<i64, String> = node::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(Condition::all().add(node::Column::Id.is_in(endpoint_ids)))
            .all(tx)
            .await
            .map_err(map_scope_err)?
            .into_iter()
            .map(|model| (model.id, model.node_key))
            .collect();

        for model in &rows {
            after = model.id;
            let mut instance = serde_json::json!({
                "type": type_id,
                "src_node_key": keys.get(&model.src_node_id).cloned().unwrap_or_default(),
                "dst_node_key": keys.get(&model.dst_node_id).cloned().unwrap_or_default(),
            });
            if let Some(discriminator) = &model.discriminator {
                instance["discriminator"] = serde_json::Value::String(discriminator.clone());
            }
            instance["payload"] = model.payload.clone();
            for (pointer, message) in validator.validate(&instance) {
                if errors.len() < bounds.max_reported {
                    errors.push(ItemError {
                        index,
                        family: ItemFamily::Edge,
                        gts_type: Some(type_id.to_owned()),
                        pointer: Some(pointer),
                        message: format!("edge `{}`: {message}", model.edge_key),
                    });
                }
            }
            index += 1;
        }
    }
}
