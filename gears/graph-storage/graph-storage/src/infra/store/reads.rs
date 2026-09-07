//! Read paths of the built-in store.
//!
//! Every statement is scoped by the secure ORM, and every read-path predicate
//! carries `deleted_at IS NULL` — matching the partial indexes exactly, so a
//! tombstone filter never costs a sequential scan.

use std::collections::BTreeMap;

use graph_storage_sdk::models::NodeFilterField as Field;
use graph_storage_sdk::models::{
    AdjacencyEntry, AdjacencySide, ElementEnvelope, GraphRevision, NodeId, NodeKey, NodeRow,
    NodeView, ProjectionRequest, ReadSnapshot, Subject,
};
use graph_storage_sdk::plugin_api::{EmbeddingState, GraphStoreError, StoreCtx};
use sea_orm::{ColumnTrait, Condition, EntityTrait};
use toolkit_db::odata::{LimitCfg, paginate_odata};
use toolkit_db::secure::{DBRunner, SecureEntityExt};
use toolkit_odata::{Page as OdataPage, SortDir};
use uuid::Uuid;

use crate::infra::storage::entity::{edge, graph_meta, gts_type, node};
use crate::infra::storage::odata_mapper::NodeODataMapper;
use crate::infra::store::{PgGraphStore, map_db_error, map_scope_err};

/// Classify a failure of the `OData` binding.
///
/// A malformed query is not a breached bound: answering "reduce the value" to
/// a caller who named a field that does not exist sends them the wrong way,
/// and the two carry different canonical categories. Only the page-size
/// variant is genuinely `out_of_range`.
fn map_odata_err(error: toolkit_odata::Error) -> GraphStoreError {
    use toolkit_odata::Error as E;
    match error {
        E::InvalidLimit => GraphStoreError::LimitExceeded {
            what: error.to_string(),
        },
        // The caller named something the projection does not expose. The
        // declared alternatives are what makes this actionable, so they are
        // named rather than left for the caller to guess.
        E::InvalidFilter(_) | E::InvalidOrderByField(_) => GraphStoreError::InvalidQuery {
            what: format!("{error}; the projection accepts {}", declared_fields()),
        },
        other => GraphStoreError::InvalidQuery {
            what: other.to_string(),
        },
    }
}

/// The fields `$filter` and `$orderby` may name, in the order the filter-field
/// schema declares them.
fn declared_fields() -> String {
    use toolkit_odata::filter::FilterField as _;
    let columns = Field::FIELDS
        .iter()
        .map(|f| f.name().to_owned())
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "{columns}, and `payload/<path>` for the paths the selected types declare in their \
         `index` trait"
    )
}

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

    // An absent epoch row reads as 1, the same value the write path assumes,
    // so a receipt recorded before an operator ever rotates the epoch is not
    // instantly "from a previous epoch" and therefore expired.
    let mut revision = GraphRevision {
        source_epoch: 1,
        revision: 0,
    };
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

/// What the store holds of each key's vector, index-aligned with `keys`.
///
/// Scoped like every read: a key outside the caller's scope reads as unknown,
/// so the coordinator embeds it and the write path then refuses it — nothing
/// about another tenant's rows leaks through the skip decision.
pub async fn embedding_state(
    store: &PgGraphStore,
    ctx: &StoreCtx<'_>,
    keys: &[NodeKey],
) -> Result<Vec<Option<EmbeddingState>>, GraphStoreError> {
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
    let by_key: BTreeMap<String, EmbeddingState> = rows
        .into_iter()
        .map(|r| {
            let has_vector = r.embedding.is_some();
            (
                r.node_key,
                EmbeddingState {
                    input_hash: r.embedding_input_hash,
                    // A vector without an epoch is stale; no vector, no epoch.
                    vector_epoch: if has_vector { r.embedding_epoch } else { None },
                },
            )
        })
        .collect();
    Ok(keys.iter().map(|key| by_key.get(key).cloned()).collect())
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

/// The gear-assigned envelope of a node row (`fr-audit-envelope`).
///
/// `key` repeats `node_key` deliberately: the envelope is the same shape for
/// a node and an edge, and an edge has no key of its own in its body -- the
/// gear derives it. The producer-authored `node_key` stays on the body, where
/// its type declares it, so a document read back can be sent to ingest
/// unchanged.
fn envelope_of_node(model: &node::Model, revision: GraphRevision) -> ElementEnvelope {
    ElementEnvelope {
        tenant_id: model.tenant_id,
        key: model.node_key.clone(),
        created_at: model.created_at,
        created_by: Subject {
            subject_id: model.created_by_subject_id,
            subject_type: model.created_by_subject_type.clone(),
        },
        updated_at: model.updated_at,
        updated_by: Subject {
            subject_id: model.updated_by_subject_id,
            subject_type: model.updated_by_subject_type.clone(),
        },
        deleted_at: model.deleted_at,
        deleted_by: model.deleted_by_subject_id.map(|subject_id| Subject {
            subject_id,
            subject_type: model.deleted_by_subject_type.clone(),
        }),
        graph_revision: revision,
    }
}

/// The revision a read observes: the one its compound-read snapshot pinned,
/// or the current one when the read stands alone.
async fn observed_revision(
    ctx: &StoreCtx<'_>,
    runner: &impl DBRunner,
) -> Result<GraphRevision, GraphStoreError> {
    match ctx.snapshot {
        Some(snapshot) => Ok(snapshot.revision),
        None => read_revision(ctx, runner).await,
    }
}

fn to_view(
    model: node::Model,
    type_id: String,
    adjacency: Vec<AdjacencyEntry>,
    truncated: bool,
    revision: GraphRevision,
) -> NodeView {
    let envelope = envelope_of_node(&model, revision);
    NodeView {
        node_key: model.node_key,
        type_id,
        name: (!model.name.is_empty()).then_some(model.name),
        payload: Some(model.payload),
        has_embedding: model.embedding.is_some(),
        labels: Vec::new(),
        adjacency,
        adjacency_truncated: truncated,
        envelope,
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
    let revision = observed_revision(ctx, &conn).await?;
    Ok(to_view(model, type_id, adjacency, truncated, revision))
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
    let revision = observed_revision(ctx, &conn).await?;
    let mut by_id: BTreeMap<i64, node::Model> = models.into_iter().map(|m| (m.id, m)).collect();
    let mut views = Vec::new();
    for id in ids {
        if let Some(model) = by_id.remove(id) {
            let type_id = names
                .get(&model.gts_node_type_id)
                .cloned()
                .unwrap_or_default();
            views.push(to_view(model, type_id, Vec::new(), false, revision));
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

    // The selected types, with the payload paths each declares: the interned
    // ids narrow the statement, the declarations decide what `$filter` and
    // `$orderby` may name (DEVIATIONS D-104).
    let mut declared_kinds: Vec<BTreeMap<String, crate::domain::ontology::ScalarKind>> = Vec::new();
    if let Some(set) = &req.type_set {
        let names: Vec<String> = set.0.iter().cloned().collect();
        let types = gts_type::Entity::find()
            .secure()
            .scope_with(ctx.scope)
            .filter(Condition::all().add(gts_type::Column::GtsTypeId.is_in(names)))
            .all(&conn)
            .await
            .map_err(map_scope_err)?;
        let ids: Vec<i32> = types.iter().map(|t| t.id).collect();
        declared_kinds = types
            .iter()
            .map(|t| super::types::index_kinds_from_json(&t.effective_traits))
            .collect();
        select = select.filter(Condition::all().add(node::Column::GtsNodeTypeId.is_in(ids)));
    }

    if crate::domain::projection::mentions_payload(&req.query) {
        let admitted = req
            .type_set
            .as_ref()
            .map(|_| crate::domain::projection::admitted_paths(&declared_kinds));
        return project_over_payload(store, ctx, &conn, select, &req, admitted.as_ref()).await;
    }

    // Interned type ids are not carried on the row, so the names are resolved
    // for the page that comes back rather than joined per row.
    let page = paginate_odata::<Field, NodeODataMapper, _, node::Model, _, _>(
        select,
        &conn,
        &req.query,
        ("node_key", SortDir::Asc),
        LimitCfg {
            default: u64::from(store.config().projection_max_page),
            max: u64::from(store.config().projection_max_page),
        },
        |model| model,
    )
    .await
    .map_err(map_odata_err)?;

    rows_to_page(ctx, &conn, page.items, page.page_info).await
}

/// The projection when a payload path is named: the shared plan rendered by
/// `store::projection`, in the platform pager's statement shape.
async fn project_over_payload(
    store: &PgGraphStore,
    ctx: &StoreCtx<'_>,
    conn: &impl DBRunner,
    select: toolkit_db::secure::SecureSelect<node::Entity, toolkit_db::secure::Scoped>,
    req: &ProjectionRequest,
    admitted: Option<&BTreeMap<String, crate::domain::ontology::ScalarKind>>,
) -> Result<OdataPage<NodeRow>, GraphStoreError> {
    use crate::domain::projection as plan;
    use crate::infra::store::projection as sql;

    let invalid = |what: String| GraphStoreError::InvalidQuery { what };

    let plan = plan::plan(&req.query, admitted).map_err(|error| invalid(error.0))?;

    let max = u64::from(store.config().projection_max_page);
    let limit = req.query.limit.unwrap_or(max).clamp(1, max);

    if let Some(cursor) = &req.query.cursor {
        if cursor.d == "bwd" {
            return Err(invalid(
                "backward paging is not available over a payload ordering".to_owned(),
            ));
        }
        if let (Some(hash), Some(recorded)) =
            (req.query.filter_hash.as_deref(), cursor.f.as_deref())
            && hash != recorded
        {
            return Err(invalid(
                "the cursor was minted under a different $filter".to_owned(),
            ));
        }
    }

    let mut statement = select;
    if let Some(predicate) = &plan.filter {
        statement = statement.filter(sql::condition(predicate));
    }
    if let Some(cursor) = &req.query.cursor {
        statement = statement.filter(sql::keyset(&plan, cursor).map_err(invalid)?);
    }
    for (expr, order) in sql::order_terms(&plan) {
        statement = statement.order_by(expr, order);
    }
    statement = statement.limit(limit + 1);

    let mut rows = statement.all(conn).await.map_err(map_scope_err)?;
    let has_more = rows.len() as u64 > limit;
    if has_more {
        rows.truncate(usize::try_from(limit).unwrap_or(usize::MAX));
    }

    let next_cursor = match (has_more, rows.last()) {
        (true, Some(last)) => {
            let keys = sql::cursor_keys(last, &plan)
                .map_err(|error| GraphStoreError::Corrupt { reason: error })?;
            let cursor = toolkit_odata::CursorV1 {
                k: keys,
                o: plan.order.first().map_or(SortDir::Asc, |t| t.dir),
                s: sql::signed_tokens(&plan),
                f: req.query.filter_hash.clone(),
                d: "fwd".to_owned(),
            };
            Some(cursor.encode().map_err(|error| GraphStoreError::Corrupt {
                reason: format!("cannot encode the continuation token: {error}"),
            })?)
        }
        _ => None,
    };

    rows_to_page(
        ctx,
        conn,
        rows,
        toolkit_odata::page::PageInfo {
            next_cursor,
            prev_cursor: None,
            limit,
        },
    )
    .await
}

/// Rows to the page the SDK reports, with type names resolved once per page
/// and the observed revision on every envelope.
async fn rows_to_page(
    ctx: &StoreCtx<'_>,
    conn: &impl DBRunner,
    rows: Vec<node::Model>,
    page_info: toolkit_odata::page::PageInfo,
) -> Result<OdataPage<NodeRow>, GraphStoreError> {
    // The page wrapper is the platform's `toolkit_odata::Page`, which carries
    // items and cursors and nothing else -- so the revision this projection
    // observed rides on each row's envelope or is not reported at all
    // (PRD § fr-tabular-projection).
    let revision = observed_revision(ctx, conn).await?;

    let mut type_ids: Vec<i32> = rows.iter().map(|m| m.gts_node_type_id).collect();
    type_ids.sort_unstable();
    type_ids.dedup();
    let names = type_names(ctx, conn, &type_ids).await?;

    Ok(OdataPage {
        items: rows
            .into_iter()
            .map(|m| NodeRow {
                type_id: names.get(&m.gts_node_type_id).cloned().unwrap_or_default(),
                envelope: envelope_of_node(&m, revision),
                node_key: m.node_key,
                name: (!m.name.is_empty()).then_some(m.name),
                payload: Some(m.payload),
            })
            .collect(),
        page_info,
    })
}
