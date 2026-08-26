//! Read paths of the built-in store.
//!
//! Every statement is scoped by the secure ORM, and every read-path predicate
//! carries `deleted_at IS NULL` — matching the partial indexes exactly, so a
//! tombstone filter never costs a sequential scan.

use std::collections::BTreeMap;

use graph_storage_sdk::models::NodeFilterField as Field;
use graph_storage_sdk::models::{
    AdjacencyEntry, AdjacencySide, GraphRevision, NodeId, NodeKey, NodeRow, NodeView,
    ProjectionRequest, ReadSnapshot,
};
use graph_storage_sdk::plugin_api::{GraphStoreError, StoreCtx};
use sea_orm::{ColumnTrait, Condition, EntityTrait};
use toolkit_db::odata::{LimitCfg, paginate_odata};
use toolkit_db::secure::{DBRunner, SecureEntityExt};
use toolkit_odata::{Page as OdataPage, SortDir};
use uuid::Uuid;

use crate::infra::storage::entity::{edge, graph_meta, gts_type, node};
use crate::infra::storage::odata_mapper::NodeODataMapper;
use crate::infra::store::{PgGraphStore, map_db_error, map_scope_err};

/// The per-tenant revision and the deployment epoch, read together.
pub async fn revision(
    store: &PgGraphStore,
    ctx: &StoreCtx<'_>,
) -> Result<GraphRevision, GraphStoreError> {
    let conn = store.db().conn().map_err(|error| map_db_error(&error))?;
    read_revision(ctx, &conn).await
}

pub async fn read_revision(
    ctx: &StoreCtx<'_>,
    runner: &impl DBRunner,
) -> Result<GraphRevision, GraphStoreError> {
    let rows = graph_meta::Entity::find()
        .secure()
        .scope_with(ctx.scope)
        .filter(
            Condition::all().add(
                graph_meta::Column::Key
                    .is_in([graph_meta::KEY_GRAPH_REVISION, graph_meta::KEY_SOURCE_EPOCH]),
            ),
        )
        .all(runner)
        .await
        .map_err(map_scope_err)?;

    let mut revision = GraphRevision::default();
    for row in rows {
        let value = row.value.as_i64().unwrap_or(0);
        if row.key == graph_meta::KEY_GRAPH_REVISION {
            revision.revision = value;
        } else if row.key == graph_meta::KEY_SOURCE_EPOCH {
            revision.source_epoch = value;
        }
    }
    Ok(revision)
}

/// Open a compound read.
///
/// **Weaker than the contract asks for.** A true repeatable-read snapshot
/// needs one transaction held across the calls that share it, which the
/// sealed runner cannot express: `Db::transaction_ref_mapped` owns the
/// transaction for the duration of one closure. What this returns is the
/// revision observed when the read began; responses are stamped with it, so a
/// caller can detect that two arms disagreed, but the arms are not isolated
/// from a concurrent commit. The capability is declared absent
/// (`StoreCapabilities::snapshots = false`) rather than claimed weakly.
pub async fn begin_read(
    store: &PgGraphStore,
    ctx: &StoreCtx<'_>,
) -> Result<ReadSnapshot, GraphStoreError> {
    Ok(ReadSnapshot {
        id: Uuid::now_v7(),
        revision: revision(store, ctx).await?,
    })
}

/// Resolve producer keys to internal ids. Unknown and unauthorized keys are
/// alike absent from the answer (anti-enumeration).
pub async fn resolve_node_ids(
    store: &PgGraphStore,
    ctx: &StoreCtx<'_>,
    keys: &[NodeKey],
) -> Result<Vec<(NodeKey, NodeId)>, GraphStoreError> {
    if keys.is_empty() {
        return Ok(Vec::new());
    }
    let conn = store.db().conn().map_err(|error| map_db_error(&error))?;
    let rows = node::Entity::find()
        .secure()
        .scope_with(ctx.scope)
        .filter(Condition::all().add(node::Column::NodeKey.is_in(keys.to_vec())))
        .filter(Condition::all().add(node::Column::DeletedAt.is_null()))
        .all(&conn)
        .await
        .map_err(map_scope_err)?;
    Ok(rows.into_iter().map(|r| (r.node_key, r.id)).collect())
}

async fn type_names(
    ctx: &StoreCtx<'_>,
    runner: &impl DBRunner,
    ids: &[i32],
) -> Result<BTreeMap<i32, String>, GraphStoreError> {
    if ids.is_empty() {
        return Ok(BTreeMap::new());
    }
    let rows = gts_type::Entity::find()
        .secure()
        .scope_with(ctx.scope)
        .filter(Condition::all().add(gts_type::Column::Id.is_in(ids.to_vec())))
        .all(runner)
        .await
        .map_err(map_scope_err)?;
    Ok(rows.into_iter().map(|r| (r.id, r.gts_type_id)).collect())
}

fn to_view(
    model: node::Model,
    type_id: String,
    adjacency: Vec<AdjacencyEntry>,
    truncated: bool,
) -> NodeView {
    NodeView {
        node_key: model.node_key,
        type_id,
        name: (!model.name.is_empty()).then_some(model.name),
        payload: Some(model.payload),
        has_embedding: model.embedding.is_some(),
        labels: Vec::new(),
        adjacency,
        adjacency_truncated: truncated,
        created_at: model.created_at,
        updated_at: model.updated_at,
    }
}

pub async fn get_node(
    store: &PgGraphStore,
    ctx: &StoreCtx<'_>,
    key: &NodeKey,
    adjacency_limit: u32,
) -> Result<NodeView, GraphStoreError> {
    let conn = store.db().conn().map_err(|error| map_db_error(&error))?;
    let model = node::Entity::find()
        .secure()
        .scope_with(ctx.scope)
        .filter(Condition::all().add(node::Column::NodeKey.eq(key.clone())))
        .filter(Condition::all().add(node::Column::DeletedAt.is_null()))
        .one(&conn)
        .await
        .map_err(map_scope_err)?
        .ok_or(GraphStoreError::NotFound)?;

    // Bidirectional adjacency, one extra row so truncation is observed rather
    // than inferred from a full page.
    let probe = u64::from(adjacency_limit) + 1;
    let outgoing = edge::Entity::find()
        .secure()
        .scope_with(ctx.scope)
        .filter(Condition::all().add(edge::Column::SrcNodeId.eq(model.id)))
        .filter(Condition::all().add(edge::Column::DeletedAt.is_null()))
        .order_by(edge::Column::Id, sea_orm::Order::Asc)
        .limit(probe)
        .all(&conn)
        .await
        .map_err(map_scope_err)?;
    let incoming = edge::Entity::find()
        .secure()
        .scope_with(ctx.scope)
        .filter(Condition::all().add(edge::Column::DstNodeId.eq(model.id)))
        .filter(Condition::all().add(edge::Column::DeletedAt.is_null()))
        .order_by(edge::Column::Id, sea_orm::Order::Asc)
        .limit(probe)
        .all(&conn)
        .await
        .map_err(map_scope_err)?;

    let truncated = outgoing.len() as u64 > u64::from(adjacency_limit)
        || incoming.len() as u64 > u64::from(adjacency_limit);

    let mut neighbour_ids: Vec<i64> = outgoing
        .iter()
        .map(|e| e.dst_node_id)
        .chain(incoming.iter().map(|e| e.src_node_id))
        .collect();
    neighbour_ids.sort_unstable();
    neighbour_ids.dedup();

    let neighbours = node::Entity::find()
        .secure()
        .scope_with(ctx.scope)
        .filter(Condition::all().add(node::Column::Id.is_in(neighbour_ids)))
        .filter(Condition::all().add(node::Column::DeletedAt.is_null()))
        .all(&conn)
        .await
        .map_err(map_scope_err)?;
    let by_id: BTreeMap<i64, node::Model> = neighbours.into_iter().map(|n| (n.id, n)).collect();

    let mut type_ids: Vec<i32> = outgoing
        .iter()
        .chain(incoming.iter())
        .map(|e| e.gts_edge_type_id)
        .chain(by_id.values().map(|n| n.gts_node_type_id))
        .chain(std::iter::once(model.gts_node_type_id))
        .collect();
    type_ids.sort_unstable();
    type_ids.dedup();
    let names = type_names(ctx, &conn, &type_ids).await?;

    let unknown = || String::new();
    let mut adjacency = Vec::new();
    for (edges, side) in [
        (&outgoing, AdjacencySide::Outgoing),
        (&incoming, AdjacencySide::Incoming),
    ] {
        for e in edges.iter().take(adjacency_limit as usize) {
            let neighbour_id = match side {
                AdjacencySide::Outgoing => e.dst_node_id,
                AdjacencySide::Incoming => e.src_node_id,
            };
            // A neighbour the caller cannot see is simply absent — denied and
            // nonexistent are indistinguishable.
            let Some(neighbour) = by_id.get(&neighbour_id) else {
                continue;
            };
            adjacency.push(AdjacencyEntry {
                edge_key: e.edge_key.clone(),
                edge_type_id: names
                    .get(&e.gts_edge_type_id)
                    .cloned()
                    .unwrap_or_else(unknown),
                side,
                neighbor_key: neighbour.node_key.clone(),
                neighbor_type_id: names
                    .get(&neighbour.gts_node_type_id)
                    .cloned()
                    .unwrap_or_else(unknown),
            });
        }
    }

    let type_id = names
        .get(&model.gts_node_type_id)
        .cloned()
        .unwrap_or_else(unknown);
    Ok(to_view(model, type_id, adjacency, truncated))
}

pub async fn hydrate_nodes(
    store: &PgGraphStore,
    ctx: &StoreCtx<'_>,
    ids: &[NodeId],
) -> Result<Vec<NodeView>, GraphStoreError> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let conn = store.db().conn().map_err(|error| map_db_error(&error))?;
    let models = node::Entity::find()
        .secure()
        .scope_with(ctx.scope)
        .filter(Condition::all().add(node::Column::Id.is_in(ids.to_vec())))
        .filter(Condition::all().add(node::Column::DeletedAt.is_null()))
        .all(&conn)
        .await
        .map_err(map_scope_err)?;

    let mut type_ids: Vec<i32> = models.iter().map(|m| m.gts_node_type_id).collect();
    type_ids.sort_unstable();
    type_ids.dedup();
    let names = type_names(ctx, &conn, &type_ids).await?;

    // Preserve the caller's order — the walk's order is deterministic and
    // callers rely on seeds coming first.
    let mut by_id: BTreeMap<i64, node::Model> = models.into_iter().map(|m| (m.id, m)).collect();
    let mut views = Vec::new();
    for id in ids {
        if let Some(model) = by_id.remove(id) {
            let type_id = names
                .get(&model.gts_node_type_id)
                .cloned()
                .unwrap_or_default();
            views.push(to_view(model, type_id, Vec::new(), false));
        }
    }
    Ok(views)
}

pub async fn project_table(
    store: &PgGraphStore,
    ctx: &StoreCtx<'_>,
    req: ProjectionRequest,
) -> Result<OdataPage<NodeRow>, GraphStoreError> {
    let conn = store.db().conn().map_err(|error| map_db_error(&error))?;

    let mut select = node::Entity::find()
        .secure()
        .scope_with(ctx.scope)
        .filter(Condition::all().add(node::Column::DeletedAt.is_null()));

    if let Some(set) = &req.type_set {
        let names: Vec<String> = set.0.iter().cloned().collect();
        let ids = gts_type::Entity::find()
            .secure()
            .scope_with(ctx.scope)
            .filter(Condition::all().add(gts_type::Column::GtsTypeId.is_in(names)))
            .all(&conn)
            .await
            .map_err(map_scope_err)?
            .into_iter()
            .map(|t| t.id)
            .collect::<Vec<_>>();
        select = select.filter(Condition::all().add(node::Column::GtsNodeTypeId.is_in(ids)));
    }

    // Interned type ids are not carried on the row, so the names are resolved
    // for the page that comes back rather than joined per row.
    let page = paginate_odata::<Field, NodeODataMapper, _, node::Model, _, _>(
        select,
        &conn,
        &req.query,
        ("node_key", SortDir::Asc),
        LimitCfg {
            default: store.config().projection_max_page as u64,
            max: store.config().projection_max_page as u64,
        },
        |model| model,
    )
    .await
    .map_err(|error| GraphStoreError::LimitExceeded {
        what: error.to_string(),
    })?;

    let mut type_ids: Vec<i32> = page.items.iter().map(|m| m.gts_node_type_id).collect();
    type_ids.sort_unstable();
    type_ids.dedup();
    let names = type_names(ctx, &conn, &type_ids).await?;

    Ok(OdataPage {
        items: page
            .items
            .into_iter()
            .map(|m| NodeRow {
                type_id: names.get(&m.gts_node_type_id).cloned().unwrap_or_default(),
                node_key: m.node_key,
                name: (!m.name.is_empty()).then_some(m.name),
                payload: Some(m.payload),
                created_at: m.created_at,
                updated_at: m.updated_at,
            })
            .collect(),
        page_info: page.page_info,
    })
}
