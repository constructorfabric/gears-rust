//! The write path: one atomic ingest, and soft delete.
//!
//! Everything — nodes, edges, the scope-replacement fence and the idempotency
//! receipt — commits in **one** transaction or not at all. Three traps the
//! prototype hit are closed here by construction: the transaction is real
//! (not a sequence of autocommits), the returned revision is read back rather
//! than reported as a literal zero, and a conflicting insert that changes
//! nothing is convergence rather than a failure.

use std::collections::BTreeMap;

use graph_storage_sdk::models::{
    DeleteOutcome, DeleteRequest, EdgeSpec, EffectiveTraits, GraphRevision, IngestCounts,
    IngestOutcome, IngestRequest, ItemError, ItemFamily, NodeSpec, ReplaceScope, Subject,
};
use graph_storage_sdk::plugin_api::{EmbeddingPlan, GraphStoreError, StoreCtx};
use sea_orm::sea_query::Expr;
use sea_orm::{ActiveValue, ColumnTrait, Condition, EntityTrait, QueryFilter};
use time::OffsetDateTime;
use toolkit_db::secure::{DBRunner, SecureEntityExt, SecureInsertExt, SecureUpdateExt};
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::embedding::{PlannedVector, StoredVector, VectorOutcome, decide_vector};
use crate::domain::identity;
use crate::infra::storage::entity::{edge, graph_meta, ingest_idempotency, node, scope_registry};
use crate::infra::store::types::interned_ids;
use crate::infra::store::{PgGraphStore, TxStoreError, map_db_error, map_scope_err};

/// What one type resolution yields on the write path.
struct TypeInfo {
    id: i32,
    uuid: Uuid,
    family: Option<String>,
    full_text_search: Vec<String>,
    /// Admissible endpoint types, as GTS patterns. Edge types only; the node
    /// base declares neither, so they arrive empty and constrain nothing.
    src_types: Vec<String>,
    dst_types: Vec<String>,
}

/// An endpoint resolved for an edge: which row, and what type it carries.
///
/// The type travels with the id because the endpoint constraint is checked
/// against it, and re-reading it per edge would mean a query per endpoint per
/// edge in a batch that may hold twenty thousand of them.
#[derive(Clone, Copy)]
struct Endpoint {
    id: i64,
    /// Interned type reference, resolved to a GTS identifier and a family
    /// only when a constraint actually has to be checked.
    type_id: i32,
}

fn item_error(index: usize, family: ItemFamily, type_id: &str, message: String) -> GraphStoreError {
    GraphStoreError::Validation {
        items: vec![ItemError {
            index,
            family,
            gts_type: Some(type_id.to_owned()),
            pointer: None,
            message,
        }],
    }
}

/// Compose the vectorizable/lexical text from the type's declared paths.
/// Names are always included; `full_text_search` adds payload paths.
fn compose_search_text(node: &NodeSpec, paths: &[String]) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(name) = &node.name {
        parts.push(name.clone());
    }
    if let Some(payload) = &node.payload {
        for path in paths {
            if path == "/name" {
                continue;
            }
            if let Some(value) = payload.pointer(path.strip_prefix("/payload").unwrap_or(path)) {
                match value {
                    serde_json::Value::String(text) => parts.push(text.clone()),
                    other => parts.push(other.to_string()),
                }
            }
        }
    }
    parts.join(" ")
}

async fn resolve_types(
    scope: &AccessScope,
    runner: &impl DBRunner,
    request: &IngestRequest,
) -> Result<BTreeMap<String, TypeInfo>, GraphStoreError> {
    let mut wanted: Vec<String> = request
        .nodes
        .iter()
        .map(|n| n.type_id.clone())
        .chain(request.edges.iter().map(|e| e.type_id.clone()))
        .collect();
    // The phantom type is never named by a producer — it is `x-gts-final` and
    // authored only by the gear — so resolving it from the batch's own types
    // would find it only by accident. It is always this one identifier.
    if !request.edges.is_empty() {
        wanted.push(graph_storage_sdk::gts::PHANTOM_NODE_TYPE.to_owned());
    }
    wanted.sort();
    wanted.dedup();

    let raw = interned_ids(scope, runner, &wanted).await?;
    Ok(raw
        .into_iter()
        .map(|(type_id, (id, uuid, traits))| {
            let family = traits
                .get("family")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned);
            let resolved = crate::infra::store::types::traits_from_json(&traits);
            (
                type_id,
                TypeInfo {
                    id,
                    uuid,
                    family,
                    full_text_search: resolved.full_text_search,
                    src_types: resolved.src_types,
                    dst_types: resolved.dst_types,
                },
            )
        })
        .collect())
}

/// The recorded outcome of an ingest, as the receipt stores it.
///
/// Hand-written rather than derived: the SDK models are transport-agnostic by
/// contract and carry no serde, so the receipt's JSON shape is owned here —
/// where the column it lands in is also defined.
fn outcome_to_json(outcome: &IngestOutcome) -> serde_json::Value {
    let c = &outcome.counts;
    serde_json::json!({
        "revision": {
            "source_epoch": outcome.revision.source_epoch,
            "revision": outcome.revision.revision,
        },
        "counts": {
            "nodes_inserted": c.nodes_inserted,
            "nodes_updated": c.nodes_updated,
            "nodes_unchanged": c.nodes_unchanged,
            "edges_inserted": c.edges_inserted,
            "edges_updated": c.edges_updated,
            "edges_unchanged": c.edges_unchanged,
            "phantoms_created": c.phantoms_created,
            "phantoms_materialized": c.phantoms_materialized,
            "scope_removed_nodes": c.scope_removed_nodes,
            "scope_removed_edges": c.scope_removed_edges,
        },
    })
}

fn outcome_from_json(value: &serde_json::Value) -> Result<IngestOutcome, GraphStoreError> {
    let corrupt = |what: &str| GraphStoreError::Corrupt {
        reason: format!("idempotency receipt is missing `{what}`"),
    };
    let revision = value.get("revision").ok_or_else(|| corrupt("revision"))?;
    let counts = value.get("counts").ok_or_else(|| corrupt("counts"))?;
    let number = |parent: &serde_json::Value, key: &str| -> u64 {
        parent
            .get(key)
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0)
    };
    Ok(IngestOutcome {
        revision: GraphRevision {
            source_epoch: revision
                .get("source_epoch")
                .and_then(serde_json::Value::as_i64)
                .ok_or_else(|| corrupt("revision.source_epoch"))?,
            revision: revision
                .get("revision")
                .and_then(serde_json::Value::as_i64)
                .ok_or_else(|| corrupt("revision.revision"))?,
        },
        replayed: false,
        counts: IngestCounts {
            nodes_inserted: number(counts, "nodes_inserted"),
            nodes_updated: number(counts, "nodes_updated"),
            nodes_unchanged: number(counts, "nodes_unchanged"),
            edges_inserted: number(counts, "edges_inserted"),
            edges_updated: number(counts, "edges_updated"),
            edges_unchanged: number(counts, "edges_unchanged"),
            phantoms_created: number(counts, "phantoms_created"),
            phantoms_materialized: number(counts, "phantoms_materialized"),
            scope_removed_nodes: number(counts, "scope_removed_nodes"),
            scope_removed_edges: number(counts, "scope_removed_edges"),
        },
        per_item_nodes: None,
        per_item_edges: None,
    })
}

/// Read the tenant's revision inside the write transaction.
async fn current_revision(
    scope: &AccessScope,
    runner: &impl DBRunner,
) -> Result<i64, GraphStoreError> {
    let row = graph_meta::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(Condition::all().add(graph_meta::Column::Key.eq(graph_meta::KEY_GRAPH_REVISION)))
        .one(runner)
        .await
        .map_err(map_scope_err)?;
    Ok(row.and_then(|r| r.value.as_i64()).unwrap_or(0))
}

async fn source_epoch(scope: &AccessScope, runner: &impl DBRunner) -> Result<i64, GraphStoreError> {
    let row = graph_meta::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(Condition::all().add(graph_meta::Column::Key.eq(graph_meta::KEY_SOURCE_EPOCH)))
        .one(runner)
        .await
        .map_err(map_scope_err)?;
    Ok(row.and_then(|r| r.value.as_i64()).unwrap_or(1))
}

/// Advance the revision. Called **only** when the transaction actually
/// changed stored state, so a convergent replay leaves the counter alone.
///
/// Reachable from the ontology path as well as this one: an accepted type
/// update changes what an existing read answers (a newly declared `index` path
/// becomes filterable, and payloads validate against a different schema), and
/// the Read Consistency Contract's promise is that two reads at one revision
/// cannot observe different content — the same reason a label attach advances
/// it (ADR-0006).
pub(crate) async fn bump_revision(
    tenant: Uuid,
    scope: &AccessScope,
    runner: &impl DBRunner,
) -> Result<i64, GraphStoreError> {
    let next = current_revision(scope, runner).await? + 1;
    let active = graph_meta::ActiveModel {
        tenant_id: ActiveValue::Set(tenant),
        key: ActiveValue::Set(graph_meta::KEY_GRAPH_REVISION.to_owned()),
        value: ActiveValue::Set(serde_json::json!(next)),
    };
    let on_conflict = toolkit_db::secure::SecureOnConflict::<graph_meta::Entity>::columns([
        graph_meta::Column::TenantId,
        graph_meta::Column::Key,
    ])
    .update_columns([graph_meta::Column::Value])
    .map_err(map_scope_err)?;
    graph_meta::Entity::insert(active)
        .secure()
        .scope_unchecked(scope)
        .map_err(map_scope_err)?
        .on_conflict(on_conflict)
        .exec(runner)
        .await
        .map_err(map_scope_err)?;
    Ok(next)
}

pub async fn ingest(
    store: &PgGraphStore,
    ctx: &StoreCtx<'_>,
    request: IngestRequest,
    embedding: EmbeddingPlan,
) -> Result<IngestOutcome, GraphStoreError> {
    let tenant = ctx.tenant;
    let scope = ctx.scope.clone();
    let subject = ctx.subject.clone();
    let producer = String::new();

    store
        .db()
        .transaction_ref_mapped::<_, IngestOutcome, TxStoreError>(move |tx| {
            let request = request.clone();
            let embedding = embedding.clone();
            let scope = scope.clone();
            let subject = subject.clone();
            let producer = producer.clone();
            Box::pin(async move {
                ingest_in_tx(
                    Writer {
                        tenant,
                        scope: &scope,
                        subject: &subject,
                    },
                    &producer,
                    tx,
                    request,
                    &embedding,
                )
                .await
                .map_err(TxStoreError::from)
            })
        })
        .await
        .map_err(|error| error.0)
}

/// The three values every write in a batch carries and never carries apart:
/// whose graph, under what compiled scope, and on whose behalf. Threaded as
/// one because the alternative -- three parameters -- made four signatures
/// wider than they had any reason to be.
#[derive(Clone, Copy)]
struct Writer<'a> {
    tenant: Uuid,
    scope: &'a AccessScope,
    subject: &'a Subject,
}

async fn ingest_in_tx(
    w: Writer<'_>,
    producer: &str,
    tx: &impl DBRunner,
    request: IngestRequest,
    embedding: &EmbeddingPlan,
) -> Result<IngestOutcome, GraphStoreError> {
    let (tenant, scope) = (w.tenant, w.scope);
    let epoch = source_epoch(scope, tx).await?;
    let request_hash = identity::ingest_request_hash(&request);

    // A recorded key answers without touching state.
    if let Some(key) = &request.idempotency_key
        && let Some(replayed) =
            replay_receipt(scope, producer, tx, key, &request_hash, epoch).await?
    {
        return Ok(replayed);
    }

    // --- scope replacement: lock the fence row before anything else --------
    let mut counts = IngestCounts::default();
    if let Some(replace) = &request.replace_scope {
        let (removed_nodes, removed_edges) =
            fence_and_clear_scope(tenant, scope, producer, tx, replace, &request_hash).await?;
        counts.scope_removed_nodes = removed_nodes;
        counts.scope_removed_edges = removed_edges;
    }

    let types = resolve_types(scope, tx, &request).await?;
    let mut changed = counts.scope_removed_nodes > 0 || counts.scope_removed_edges > 0;

    let mut node_ids: BTreeMap<String, Endpoint> = BTreeMap::new();
    changed |= write_nodes(
        w,
        tx,
        &request,
        &types,
        &mut node_ids,
        &mut counts,
        embedding,
    )
    .await?;

    changed |= write_edges(w, tx, &request, &types, &mut node_ids, &mut counts).await?;

    // The revision advances if and only if stored state actually changed.
    let revision_value = if changed {
        bump_revision(tenant, scope, tx).await?
    } else {
        current_revision(scope, tx).await?
    };

    let outcome = IngestOutcome {
        revision: GraphRevision {
            source_epoch: epoch,
            revision: revision_value,
        },
        replayed: false,
        counts,
        per_item_nodes: None,
        per_item_edges: None,
    };

    // The receipt commits with the batch, never after it.
    if let Some(key) = &request.idempotency_key {
        let response = outcome_to_json(&outcome);
        let active = ingest_idempotency::ActiveModel {
            tenant_id: ActiveValue::Set(tenant),
            producer: ActiveValue::Set(producer.to_owned()),
            idempotency_key: ActiveValue::Set(key.clone()),
            request_hash: ActiveValue::Set(request_hash),
            source_epoch: ActiveValue::Set(epoch),
            graph_revision: ActiveValue::Set(revision_value),
            response: ActiveValue::Set(response),
            created_at: ActiveValue::Set(OffsetDateTime::now_utc()),
        };
        ingest_idempotency::Entity::insert(active)
            .secure()
            .scope_unchecked(scope)
            .map_err(map_scope_err)?
            .exec(tx)
            .await
            .map_err(map_scope_err)?;
    }

    Ok(outcome)
}

/// Lock the scope's fence row, apply generation fencing, and tombstone the
/// scope's static content. Analysis-originated edges are never removed.
async fn fence_and_clear_scope(
    tenant: Uuid,
    scope: &AccessScope,
    producer: &str,
    tx: &impl DBRunner,
    replace: &ReplaceScope,
    request_hash: &str,
) -> Result<(u64, u64), GraphStoreError> {
    let existing = scope_registry::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(
            Condition::all()
                .add(scope_registry::Column::ScopeAttribute.eq(replace.attribute.clone())),
        )
        .filter(Condition::all().add(scope_registry::Column::ScopeValue.eq(replace.value.clone())))
        .one(tx)
        .await
        .map_err(map_scope_err)?;

    if let Some(row) = &existing {
        if row.owner_producer != producer {
            return Err(GraphStoreError::Conflict {
                reason: "this scope is owned by another producer".into(),
            });
        }
        if replace.generation < row.generation {
            return Err(GraphStoreError::StaleGeneration {
                recorded: row.generation,
                offered: replace.generation,
            });
        }
        if replace.generation == row.generation && row.request_hash != request_hash {
            return Err(GraphStoreError::Conflict {
                reason: "same source generation with different content".into(),
            });
        }
    }

    let active = scope_registry::ActiveModel {
        tenant_id: ActiveValue::Set(tenant),
        scope_attribute: ActiveValue::Set(replace.attribute.clone()),
        scope_value: ActiveValue::Set(replace.value.clone()),
        owner_producer: ActiveValue::Set(producer.to_owned()),
        generation: ActiveValue::Set(replace.generation),
        request_hash: ActiveValue::Set(request_hash.to_owned()),
        updated_at: ActiveValue::Set(OffsetDateTime::now_utc()),
    };
    let on_conflict = toolkit_db::secure::SecureOnConflict::<scope_registry::Entity>::columns([
        scope_registry::Column::TenantId,
        scope_registry::Column::ScopeAttribute,
        scope_registry::Column::ScopeValue,
    ])
    .update_columns([
        scope_registry::Column::Generation,
        scope_registry::Column::RequestHash,
        scope_registry::Column::UpdatedAt,
    ])
    .map_err(map_scope_err)?;
    scope_registry::Entity::insert(active)
        .secure()
        .scope_unchecked(scope)
        .map_err(map_scope_err)?
        .on_conflict(on_conflict)
        .exec(tx)
        .await
        .map_err(map_scope_err)?;

    // Scope-managed content of this scope is replaced; nothing is removed in
    // this iteration beyond what the caller re-supplies, so the counts are
    // zero. (Full declarative replacement is a scope cut — see DEVIATIONS.)
    Ok((0, 0))
}

enum NodeWrite {
    Inserted,
    Updated,
    Unchanged,
    Materialized,
}

enum EdgeWrite {
    Inserted,
    Updated,
    Unchanged,
}

async fn lookup_endpoint(
    scope: &AccessScope,
    tx: &impl DBRunner,
    key: &str,
) -> Result<Option<Endpoint>, GraphStoreError> {
    Ok(node::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(Condition::all().add(node::Column::NodeKey.eq(key.to_owned())))
        .one(tx)
        .await
        .map_err(map_scope_err)?
        .map(|m| Endpoint {
            id: m.id,
            type_id: m.gts_node_type_id,
        }))
}

/// The GTS identifier and family of each interned type named, for the
/// endpoints of one batch.
async fn endpoint_types(
    scope: &AccessScope,
    tx: &impl DBRunner,
    ids: &[i32],
) -> Result<BTreeMap<i32, (String, Option<String>)>, GraphStoreError> {
    if ids.is_empty() {
        return Ok(BTreeMap::new());
    }
    let rows = crate::infra::storage::entity::gts_type::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(
            Condition::all()
                .add(crate::infra::storage::entity::gts_type::Column::Id.is_in(ids.to_vec())),
        )
        .all(tx)
        .await
        .map_err(map_scope_err)?;
    Ok(rows
        .into_iter()
        .map(|row| {
            let family = row
                .effective_traits
                .get("family")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned);
            (row.id, (row.gts_type_id, family))
        })
        .collect())
}

/// Revalidate every live edge incident to a node that has just become
/// concrete.
///
/// Edges attached while the node was a phantom could not be endpoint-checked —
/// the placeholder type names nothing a producer pattern would admit — so the
/// check is deferred to here. A violation rejects the whole batch with a
/// per-item error naming the edge; nothing is mutated, because this runs
/// inside the ingest transaction (Phantom Materialization Contract, rule 3).
async fn revalidate_incident_edges(
    scope: &AccessScope,
    tx: &impl DBRunner,
    node_id: i64,
    concrete_type: &str,
    index: usize,
) -> Result<(), GraphStoreError> {
    let incident = edge::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(
            Condition::any()
                .add(edge::Column::SrcNodeId.eq(node_id))
                .add(edge::Column::DstNodeId.eq(node_id)),
        )
        .filter(Condition::all().add(edge::Column::DeletedAt.is_null()))
        .all(tx)
        .await
        .map_err(map_scope_err)?;
    if incident.is_empty() {
        return Ok(());
    }

    let mut edge_type_ids: Vec<i32> = incident.iter().map(|e| e.gts_edge_type_id).collect();
    edge_type_ids.sort_unstable();
    edge_type_ids.dedup();
    let edge_types = crate::infra::storage::entity::gts_type::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(
            Condition::all()
                .add(crate::infra::storage::entity::gts_type::Column::Id.is_in(edge_type_ids)),
        )
        .all(tx)
        .await
        .map_err(map_scope_err)?;
    let by_id: BTreeMap<i32, (String, EffectiveTraits)> = edge_types
        .into_iter()
        .map(|row| {
            (
                row.id,
                (
                    row.gts_type_id,
                    crate::infra::store::types::traits_from_json(&row.effective_traits),
                ),
            )
        })
        .collect();

    for e in incident {
        let Some((edge_type, traits)) = by_id.get(&e.gts_edge_type_id) else {
            continue;
        };
        // The node may sit at either end, or both on a self-edge.
        for (is_end, patterns, which) in [
            (e.src_node_id == node_id, &traits.src_types, "source"),
            (e.dst_node_id == node_id, &traits.dst_types, "destination"),
        ] {
            if !is_end {
                continue;
            }
            if !endpoint_admitted(concrete_type, None, patterns)? {
                return Err(GraphStoreError::Validation {
                    items: vec![ItemError {
                        index,
                        family: ItemFamily::Node,
                        gts_type: Some(concrete_type.to_owned()),
                        pointer: Some("/type".to_owned()),
                        message: format!(
                            "materializing this node as `{concrete_type}` would leave edge \
                             `{}` invalid: `{edge_type}` does not admit it as a {which} \
                             (accepts {})",
                            e.edge_key,
                            patterns.join(", ")
                        ),
                    }],
                });
            }
        }
    }
    Ok(())
}

/// Whether an endpoint's type satisfies the patterns its edge type declares.
///
/// A phantom endpoint is **not** checked: it carries the gear's own placeholder
/// type, which no producer pattern names, and the concrete type it will become
/// is not known yet. The Phantom Materialization Contract closes that hole from
/// the other side — every incident edge is revalidated when the phantom becomes
/// concrete — so skipping here defers the check rather than dropping it.
fn endpoint_admitted(
    endpoint_type: &str,
    family: Option<&str>,
    patterns: &[String],
) -> Result<bool, GraphStoreError> {
    if family == Some("phantom") || patterns.is_empty() {
        return Ok(true);
    }
    crate::domain::ontology::matches_any_pattern(endpoint_type, patterns).map_err(|error| {
        GraphStoreError::Internal(format!(
            "endpoint constraint is not a valid pattern: {error}"
        ))
    })
}

/// The three vector columns an upsert writes, resolved together because they
/// are only meaningful together.
///
/// The *decision* is `domain::embedding::decide_vector`, shared with every
/// other store; this only spells it onto columns. The encoding (recorded in
/// `dev/DEVIATIONS.md`, since the FR names the states and not their
/// representation):
///
/// | state | `embedding` | `embedding_epoch` | `embedding_input_hash` |
/// |---|---|---|---|
/// | embedded and current | the new vector | active epoch | the new input's hash |
/// | absent | NULL | NULL | the current input's hash |
/// | preserved | kept | kept | kept |
/// | stale | kept | **NULL** | kept |
///
/// The vector arm reads `embedding_epoch = <active>`, so "only current
/// vectors are searchable" is one equality rather than a rule every query has
/// to remember.
struct VectorWrite {
    embedding: Option<sea_orm::entity::prelude::PgVector>,
    epoch: Option<i64>,
    input_hash: Option<String>,
}

fn plan_vector(current: Option<&node::Model>, planned: PlannedVector<'_>) -> VectorWrite {
    let stored = current.map(|row| StoredVector {
        has_vector: row.embedding.is_some(),
        input_hash: row.embedding_input_hash.as_deref(),
    });
    match decide_vector(stored, planned) {
        VectorOutcome::Store {
            vector,
            epoch,
            input_hash,
        } => VectorWrite {
            embedding: Some(sea_orm::entity::prelude::PgVector::from(vector)),
            epoch,
            input_hash: Some(input_hash),
        },
        VectorOutcome::Absent { input_hash } => VectorWrite {
            embedding: None,
            epoch: None,
            input_hash: Some(input_hash),
        },
        VectorOutcome::Preserve => VectorWrite {
            embedding: current.and_then(|row| row.embedding.clone()),
            epoch: current.and_then(|row| row.embedding_epoch),
            input_hash: current.and_then(|row| row.embedding_input_hash.clone()),
        },
        VectorOutcome::Stale => VectorWrite {
            embedding: current.and_then(|row| row.embedding.clone()),
            epoch: None,
            input_hash: current.and_then(|row| row.embedding_input_hash.clone()),
        },
    }
}

async fn upsert_node(
    w: Writer<'_>,
    tx: &impl DBRunner,
    spec: &NodeSpec,
    info: &TypeInfo,
    index: usize,
    planned: PlannedVector<'_>,
) -> Result<(i64, NodeWrite), GraphStoreError> {
    let (tenant, scope, subject) = (w.tenant, w.scope, w.subject);
    let existing = node::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(Condition::all().add(node::Column::NodeKey.eq(spec.node_key.clone())))
        .one(tx)
        .await
        .map_err(map_scope_err)?;

    let payload = spec
        .payload
        .clone()
        .unwrap_or_else(|| serde_json::json!({}));
    let name = spec.name.clone().unwrap_or_default();
    let search_text = compose_search_text(spec, &info.full_text_search);
    let vector = plan_vector(existing.as_ref(), planned);
    let now = OffsetDateTime::now_utc();

    let Some(current) = existing else {
        let active = node::ActiveModel {
            tenant_id: ActiveValue::Set(tenant),
            id: ActiveValue::NotSet,
            node_key: ActiveValue::Set(spec.node_key.clone()),
            gts_node_type_id: ActiveValue::Set(info.id),
            name: ActiveValue::Set(name),
            payload: ActiveValue::Set(payload),
            search_text: ActiveValue::Set(search_text),
            embedding: ActiveValue::Set(vector.embedding),
            embedding_epoch: ActiveValue::Set(vector.epoch),
            embedding_input_hash: ActiveValue::Set(vector.input_hash),
            source_namespace: ActiveValue::Set(None),
            owner_principal: ActiveValue::Set(String::new()),
            version: ActiveValue::Set(1),
            created_at: ActiveValue::Set(now),
            updated_at: ActiveValue::Set(now),
            deleted_at: ActiveValue::Set(None),
            created_by_subject_id: ActiveValue::Set(subject.subject_id),
            created_by_subject_type: ActiveValue::Set(subject.subject_type.clone()),
            updated_by_subject_id: ActiveValue::Set(subject.subject_id),
            updated_by_subject_type: ActiveValue::Set(subject.subject_type.clone()),
            deleted_by_subject_id: ActiveValue::Set(None),
            deleted_by_subject_type: ActiveValue::Set(None),
        };
        let model = node::Entity::insert(active)
            .secure()
            .scope_unchecked(scope)
            .map_err(map_scope_err)?
            .exec_with_returning(tx)
            .await
            .map_err(map_scope_err)?;
        return Ok((model.id, NodeWrite::Inserted));
    };

    // A tombstoned key is not reusable before purge.
    if current.deleted_at.is_some() {
        return Err(GraphStoreError::Conflict {
            reason: format!(
                "node key `{}` is tombstoned and cannot be re-ingested before purge",
                spec.node_key
            ),
        });
    }

    // A concrete node's type is immutable under ordinary upsert; the only
    // permitted transition is phantom materialization.
    let materializing = current.gts_node_type_id != info.id;
    if materializing {
        let previous_is_phantom = is_phantom_type(scope, tx, current.gts_node_type_id).await?;
        if !previous_is_phantom {
            return Err(item_error(
                index,
                ItemFamily::Node,
                &spec.type_id,
                format!(
                    "node `{}` is already registered under a different type; \
                     a same-key ingest may not change it",
                    spec.node_key
                ),
            ));
        }
    }

    if let Some(expected) = spec.expected_version
        && expected != current.version
    {
        return Err(GraphStoreError::Conflict {
            reason: format!(
                "expected version {expected}, stored version is {}",
                current.version
            ),
        });
    }

    // Upsert replaces the mutable state wholesale — an omitted field is
    // cleared, never preserved. Convergence is detected by comparison, so a
    // replay leaves the revision alone.
    let unchanged = current.name == name
        && current.payload == payload
        && current.search_text == search_text
        && current.embedding == vector.embedding
        && current.embedding_epoch == vector.epoch
        && current.embedding_input_hash == vector.input_hash
        && !materializing;
    if unchanged {
        return Ok((current.id, NodeWrite::Unchanged));
    }

    if materializing {
        revalidate_incident_edges(scope, tx, current.id, &spec.type_id, index).await?;
    }

    let id = current.id;
    let version = current.version + 1;
    node::Entity::update_many()
        .col_expr(node::Column::GtsNodeTypeId, Expr::value(info.id))
        .col_expr(node::Column::Name, Expr::value(name))
        .col_expr(node::Column::Payload, Expr::value(payload))
        .col_expr(node::Column::SearchText, Expr::value(search_text))
        .col_expr(node::Column::Embedding, Expr::value(vector.embedding))
        .col_expr(node::Column::EmbeddingEpoch, Expr::value(vector.epoch))
        .col_expr(
            node::Column::EmbeddingInputHash,
            Expr::value(vector.input_hash),
        )
        .col_expr(node::Column::Version, Expr::value(version))
        .col_expr(node::Column::UpdatedAt, Expr::value(now))
        .col_expr(
            node::Column::UpdatedBySubjectId,
            Expr::value(subject.subject_id),
        )
        .col_expr(
            node::Column::UpdatedBySubjectType,
            Expr::value(subject.subject_type.clone()),
        )
        .filter(Condition::all().add(node::Column::Id.eq(id)))
        .secure()
        .scope_with(scope)
        .exec(tx)
        .await
        .map_err(map_scope_err)?;

    Ok((
        id,
        if materializing {
            NodeWrite::Materialized
        } else {
            NodeWrite::Updated
        },
    ))
}

async fn is_phantom_type(
    scope: &AccessScope,
    tx: &impl DBRunner,
    type_id: i32,
) -> Result<bool, GraphStoreError> {
    let model = crate::infra::storage::entity::gts_type::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(
            Condition::all().add(crate::infra::storage::entity::gts_type::Column::Id.eq(type_id)),
        )
        .one(tx)
        .await
        .map_err(map_scope_err)?;
    Ok(model
        .and_then(|m| {
            m.effective_traits
                .get("family")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        })
        .as_deref()
        == Some("phantom"))
}

async fn insert_phantom(
    w: Writer<'_>,
    tx: &impl DBRunner,
    key: &str,
    info: &TypeInfo,
) -> Result<i64, GraphStoreError> {
    let (tenant, scope, subject) = (w.tenant, w.scope, w.subject);
    let now = OffsetDateTime::now_utc();
    let active = node::ActiveModel {
        tenant_id: ActiveValue::Set(tenant),
        id: ActiveValue::NotSet,
        node_key: ActiveValue::Set(key.to_owned()),
        gts_node_type_id: ActiveValue::Set(info.id),
        name: ActiveValue::Set(String::new()),
        payload: ActiveValue::Set(serde_json::json!({})),
        search_text: ActiveValue::Set(String::new()),
        embedding: ActiveValue::Set(None),
        embedding_epoch: ActiveValue::Set(None),
        embedding_input_hash: ActiveValue::Set(None),
        source_namespace: ActiveValue::Set(None),
        owner_principal: ActiveValue::Set(String::new()),
        version: ActiveValue::Set(1),
        created_at: ActiveValue::Set(now),
        updated_at: ActiveValue::Set(now),
        deleted_at: ActiveValue::Set(None),
        // A phantom is materialized by the edge that named it, so the subject
        // that wrote that edge is the one that brought this row into being.
        created_by_subject_id: ActiveValue::Set(subject.subject_id),
        created_by_subject_type: ActiveValue::Set(subject.subject_type.clone()),
        updated_by_subject_id: ActiveValue::Set(subject.subject_id),
        updated_by_subject_type: ActiveValue::Set(subject.subject_type.clone()),
        deleted_by_subject_id: ActiveValue::Set(None),
        deleted_by_subject_type: ActiveValue::Set(None),
    };
    let model = node::Entity::insert(active)
        .secure()
        .scope_unchecked(scope)
        .map_err(map_scope_err)?
        .exec_with_returning(tx)
        .await
        .map_err(map_scope_err)?;
    Ok(model.id)
}

async fn upsert_edge(
    w: Writer<'_>,
    tx: &impl DBRunner,
    spec: &EdgeSpec,
    info: &TypeInfo,
    src: i64,
    dst: i64,
) -> Result<EdgeWrite, GraphStoreError> {
    let (tenant, scope, subject) = (w.tenant, w.scope, w.subject);
    let edge_key = identity::derive_edge_key(info.uuid, spec);
    let now = OffsetDateTime::now_utc();
    let payload = spec
        .payload
        .clone()
        .unwrap_or_else(|| serde_json::json!({}));

    let existing = edge::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(Condition::all().add(edge::Column::EdgeKey.eq(edge_key.clone())))
        .one(tx)
        .await
        .map_err(map_scope_err)?;

    let Some(current) = existing else {
        let active = edge::ActiveModel {
            tenant_id: ActiveValue::Set(tenant),
            id: ActiveValue::NotSet,
            edge_key: ActiveValue::Set(edge_key),
            gts_edge_type_id: ActiveValue::Set(info.id),
            src_node_id: ActiveValue::Set(src),
            dst_node_id: ActiveValue::Set(dst),
            discriminator: ActiveValue::Set(spec.discriminator.clone()),
            payload: ActiveValue::Set(payload),
            created_at: ActiveValue::Set(now),
            updated_at: ActiveValue::Set(now),
            deleted_at: ActiveValue::Set(None),
            created_by_subject_id: ActiveValue::Set(subject.subject_id),
            created_by_subject_type: ActiveValue::Set(subject.subject_type.clone()),
            updated_by_subject_id: ActiveValue::Set(subject.subject_id),
            updated_by_subject_type: ActiveValue::Set(subject.subject_type.clone()),
            deleted_by_subject_id: ActiveValue::Set(None),
            deleted_by_subject_type: ActiveValue::Set(None),
        };
        edge::Entity::insert(active)
            .secure()
            .scope_unchecked(scope)
            .map_err(map_scope_err)?
            .exec(tx)
            .await
            .map_err(map_scope_err)?;
        return Ok(EdgeWrite::Inserted);
    };

    if current.payload == payload && current.deleted_at.is_none() {
        return Ok(EdgeWrite::Unchanged);
    }

    let id = current.id;
    edge::Entity::update_many()
        .col_expr(edge::Column::Payload, Expr::value(payload))
        .col_expr(edge::Column::UpdatedAt, Expr::value(now))
        .col_expr(
            edge::Column::UpdatedBySubjectId,
            Expr::value(subject.subject_id),
        )
        .col_expr(
            edge::Column::UpdatedBySubjectType,
            Expr::value(subject.subject_type.clone()),
        )
        .col_expr(
            edge::Column::DeletedAt,
            Expr::value(Option::<OffsetDateTime>::None),
        )
        .col_expr(
            edge::Column::DeletedBySubjectId,
            Expr::value(Option::<Uuid>::None),
        )
        .col_expr(
            edge::Column::DeletedBySubjectType,
            Expr::value(Option::<String>::None),
        )
        .filter(Condition::all().add(edge::Column::Id.eq(id)))
        .secure()
        .scope_with(scope)
        .exec(tx)
        .await
        .map_err(map_scope_err)?;
    Ok(EdgeWrite::Updated)
}

pub async fn soft_delete(
    store: &PgGraphStore,
    ctx: &StoreCtx<'_>,
    request: DeleteRequest,
) -> Result<DeleteOutcome, GraphStoreError> {
    let tenant = ctx.tenant;
    let scope = ctx.scope.clone();
    let subject = ctx.subject.clone();

    store
        .db()
        .transaction_ref_mapped::<_, DeleteOutcome, TxStoreError>(move |tx| {
            let request = request.clone();
            let scope = scope.clone();
            let subject = subject.clone();
            Box::pin(async move {
                let epoch = source_epoch(&scope, tx).await?;
                let now = OffsetDateTime::now_utc();
                let (nodes, edges) = match request {
                    DeleteRequest::Node(key) => {
                        let model = node::Entity::find()
                            .secure()
                            .scope_with(&scope)
                            .filter(Condition::all().add(node::Column::NodeKey.eq(key)))
                            .filter(Condition::all().add(node::Column::DeletedAt.is_null()))
                            .one(tx)
                            .await
                            .map_err(map_scope_err)?
                            .ok_or(GraphStoreError::NotFound)?;

                        // Incident edges are tombstoned in the same
                        // transaction: a node never outlives its edges'
                        // visibility, and never the reverse.
                        let incident = edge::Entity::find()
                            .secure()
                            .scope_with(&scope)
                            .filter(
                                sea_orm::Condition::any()
                                    .add(edge::Column::SrcNodeId.eq(model.id))
                                    .add(edge::Column::DstNodeId.eq(model.id)),
                            )
                            .filter(Condition::all().add(edge::Column::DeletedAt.is_null()))
                            .all(tx)
                            .await
                            .map_err(map_scope_err)?;

                        let mut edges = 0u64;
                        for e in incident {
                            edge::Entity::update_many()
                                .col_expr(edge::Column::DeletedAt, Expr::value(Some(now)))
                                .col_expr(
                                    edge::Column::DeletedBySubjectId,
                                    Expr::value(Some(subject.subject_id)),
                                )
                                .col_expr(
                                    edge::Column::DeletedBySubjectType,
                                    Expr::value(subject.subject_type.clone()),
                                )
                                .filter(Condition::all().add(edge::Column::Id.eq(e.id)))
                                .secure()
                                .scope_with(&scope)
                                .exec(tx)
                                .await
                                .map_err(map_scope_err)?;
                            edges += 1;
                        }

                        node::Entity::update_many()
                            .col_expr(node::Column::DeletedAt, Expr::value(Some(now)))
                            .col_expr(
                                node::Column::DeletedBySubjectId,
                                Expr::value(Some(subject.subject_id)),
                            )
                            .col_expr(
                                node::Column::DeletedBySubjectType,
                                Expr::value(subject.subject_type.clone()),
                            )
                            .filter(Condition::all().add(node::Column::Id.eq(model.id)))
                            .secure()
                            .scope_with(&scope)
                            .exec(tx)
                            .await
                            .map_err(map_scope_err)?;
                        (1u64, edges)
                    }
                    DeleteRequest::Edge(key) => {
                        let model = edge::Entity::find()
                            .secure()
                            .scope_with(&scope)
                            .filter(Condition::all().add(edge::Column::EdgeKey.eq(key)))
                            .filter(Condition::all().add(edge::Column::DeletedAt.is_null()))
                            .one(tx)
                            .await
                            .map_err(map_scope_err)?
                            .ok_or(GraphStoreError::NotFound)?;
                        edge::Entity::update_many()
                            .col_expr(edge::Column::DeletedAt, Expr::value(Some(now)))
                            .col_expr(
                                edge::Column::DeletedBySubjectId,
                                Expr::value(Some(subject.subject_id)),
                            )
                            .col_expr(
                                edge::Column::DeletedBySubjectType,
                                Expr::value(subject.subject_type.clone()),
                            )
                            .filter(Condition::all().add(edge::Column::Id.eq(model.id)))
                            .secure()
                            .scope_with(&scope)
                            .exec(tx)
                            .await
                            .map_err(map_scope_err)?;
                        (0u64, 1u64)
                    }
                };

                let revision = bump_revision(tenant, &scope, tx).await?;
                Ok(DeleteOutcome {
                    revision: GraphRevision {
                        source_epoch: epoch,
                        revision,
                    },
                    tombstoned_nodes: nodes,
                    tombstoned_edges: edges,
                })
            })
        })
        .await
        .map_err(|error| error.0)
}

/// Ensure the tenant's meta rows exist, and the deployment epoch.
pub async fn ensure_meta(
    store: &PgGraphStore,
    tenant: Uuid,
    scope: &AccessScope,
) -> Result<(), GraphStoreError> {
    let conn = store.db().conn().map_err(|error| map_db_error(&error))?;
    for (key, value) in [
        (graph_meta::KEY_GRAPH_REVISION, serde_json::json!(0)),
        (graph_meta::KEY_SOURCE_EPOCH, serde_json::json!(1)),
    ] {
        let active = graph_meta::ActiveModel {
            tenant_id: ActiveValue::Set(tenant),
            key: ActiveValue::Set(key.to_owned()),
            value: ActiveValue::Set(value),
        };
        let on_conflict = toolkit_db::secure::SecureOnConflict::<graph_meta::Entity>::columns([
            graph_meta::Column::TenantId,
            graph_meta::Column::Key,
        ])
        .build();
        let mut on_conflict = on_conflict;
        on_conflict.do_nothing();
        graph_meta::Entity::insert(active)
            .secure()
            .scope_unchecked(scope)
            .map_err(map_scope_err)?
            .on_conflict_raw(on_conflict)
            .exec(&conn)
            .await
            .map_err(map_scope_err)?;
    }
    Ok(())
}

/// The vector width the schema was migrated with.
///
/// Readiness compares the configured dimension against this constant rather
/// than against `pg_attribute`: the sealed runner exposes no way for a gear
/// to issue a catalog query, so the migration constant is the only in-process
/// authority (see `dev/DEVIATIONS.md`).
#[must_use]
pub const fn migrated_embedding_dimension() -> u32 {
    crate::infra::storage::migrations::m0001_initial_schema::EMBEDDING_DIMENSION
}

/// Upsert every node of the batch, recording its id and how it landed.
/// Returns whether any stored state changed.
async fn write_nodes(
    w: Writer<'_>,
    tx: &impl DBRunner,
    request: &IngestRequest,
    types: &BTreeMap<String, TypeInfo>,
    node_ids: &mut BTreeMap<String, Endpoint>,
    counts: &mut IngestCounts,
    embedding: &EmbeddingPlan,
) -> Result<bool, GraphStoreError> {
    let mut changed = false;
    for (index, spec) in request.nodes.iter().enumerate() {
        let info = types.get(&spec.type_id).ok_or_else(|| {
            item_error(
                index,
                ItemFamily::Node,
                &spec.type_id,
                "type is not registered".into(),
            )
        })?;
        // Index-aligned with the request's nodes, by the port's contract. A
        // missing entry would silently unembed a node, so it is a store
        // failure rather than a default.
        let decided = embedding.nodes.get(index).ok_or_else(|| {
            GraphStoreError::Internal(format!(
                "embedding plan covers {} nodes; the batch has {}",
                embedding.nodes.len(),
                request.nodes.len()
            ))
        })?;
        let (id, write) = upsert_node(
            w,
            tx,
            spec,
            info,
            index,
            PlannedVector {
                decided,
                active_epoch: embedding.epoch,
            },
        )
        .await?;
        node_ids.insert(
            spec.node_key.clone(),
            Endpoint {
                id,
                type_id: info.id,
            },
        );
        match write {
            NodeWrite::Inserted => {
                counts.nodes_inserted += 1;
                changed = true;
            }
            NodeWrite::Updated => {
                counts.nodes_updated += 1;
                changed = true;
            }
            NodeWrite::Unchanged => counts.nodes_unchanged += 1,
            NodeWrite::Materialized => {
                counts.phantoms_materialized += 1;
                changed = true;
            }
        }
    }
    Ok(changed)
}

/// Resolve one edge endpoint: from this batch, from storage, or — when the
/// request allows it — as a freshly created phantom.
#[expect(
    clippy::too_many_arguments,
    reason = "one resolution step over the transaction's whole working state; bundling it would name a struct for a single call site"
)]
async fn resolve_endpoint(
    w: Writer<'_>,
    tx: &impl DBRunner,
    key: &str,
    index: usize,
    type_id: &str,
    types: &BTreeMap<String, TypeInfo>,
    node_ids: &mut BTreeMap<String, Endpoint>,
    create_phantoms: bool,
    counts: &mut IngestCounts,
) -> Result<bool, GraphStoreError> {
    if node_ids.contains_key(key) {
        return Ok(false);
    }
    if let Some(endpoint) = lookup_endpoint(w.scope, tx, key).await? {
        node_ids.insert(key.to_owned(), endpoint);
        return Ok(false);
    }
    if !create_phantoms {
        return Err(item_error(
            index,
            ItemFamily::Edge,
            type_id,
            format!("endpoint `{key}` does not exist and phantom creation is disabled"),
        ));
    }
    let phantom_type = types
        .values()
        .find(|t| t.family.as_deref() == Some("phantom"))
        .ok_or_else(|| {
            item_error(
                index,
                ItemFamily::Edge,
                type_id,
                format!("endpoint `{key}` does not exist and no phantom node type is registered"),
            )
        })?;
    let id = insert_phantom(w, tx, key, phantom_type).await?;
    node_ids.insert(
        key.to_owned(),
        Endpoint {
            id,
            type_id: phantom_type.id,
        },
    );
    counts.phantoms_created += 1;
    Ok(true)
}

/// Upsert every edge of the batch, materialising phantom endpoints as needed.
/// Returns whether any stored state changed.
async fn write_edges(
    w: Writer<'_>,
    tx: &impl DBRunner,
    request: &IngestRequest,
    types: &BTreeMap<String, TypeInfo>,
    node_ids: &mut BTreeMap<String, Endpoint>,
    counts: &mut IngestCounts,
) -> Result<bool, GraphStoreError> {
    let create_phantoms = request.options.create_phantoms.unwrap_or(true);
    let mut changed = false;

    for (index, spec) in request.edges.iter().enumerate() {
        let info = types.get(&spec.type_id).ok_or_else(|| {
            item_error(
                index,
                ItemFamily::Edge,
                &spec.type_id,
                "type is not registered".into(),
            )
        })?;

        for key in [&spec.src_node_key, &spec.dst_node_key] {
            changed |= resolve_endpoint(
                w,
                tx,
                key,
                index,
                &spec.type_id,
                types,
                node_ids,
                create_phantoms,
                counts,
            )
            .await?;
        }

        let src = node_ids[&spec.src_node_key];
        let dst = node_ids[&spec.dst_node_key];

        // Endpoint constraints, checked here because this is the only place
        // both endpoints are resolved and still inside the ingest transaction,
        // so an endpoint's type cannot change between the check and the commit.
        let resolved = endpoint_types(w.scope, tx, &[src.type_id, dst.type_id]).await?;
        for (end, endpoint, patterns, pointer) in [
            (&spec.src_node_key, src, &info.src_types, "/src_node_key"),
            (&spec.dst_node_key, dst, &info.dst_types, "/dst_node_key"),
        ] {
            let Some((endpoint_type, family)) = resolved.get(&endpoint.type_id) else {
                continue;
            };
            if !endpoint_admitted(endpoint_type, family.as_deref(), patterns)? {
                return Err(GraphStoreError::Validation {
                    items: vec![ItemError {
                        index,
                        family: ItemFamily::Edge,
                        gts_type: Some(spec.type_id.clone()),
                        pointer: Some(pointer.to_owned()),
                        message: format!(
                            "endpoint `{end}` is a `{endpoint_type}`, which `{}` does not admit; \
                             this edge type accepts {}",
                            spec.type_id,
                            patterns.join(", ")
                        ),
                    }],
                });
            }
        }

        match upsert_edge(w, tx, spec, info, src.id, dst.id).await? {
            EdgeWrite::Inserted => {
                counts.edges_inserted += 1;
                changed = true;
            }
            EdgeWrite::Updated => {
                counts.edges_updated += 1;
                changed = true;
            }
            EdgeWrite::Unchanged => counts.edges_unchanged += 1,
        }
    }
    Ok(changed)
}

/// A recorded receipt for this key, when the request matches it.
async fn replay_receipt(
    scope: &AccessScope,
    producer: &str,
    tx: &impl DBRunner,
    key: &str,
    request_hash: &str,
    epoch: i64,
) -> Result<Option<IngestOutcome>, GraphStoreError> {
    let existing = ingest_idempotency::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(Condition::all().add(ingest_idempotency::Column::Producer.eq(producer.to_owned())))
        .filter(Condition::all().add(ingest_idempotency::Column::IdempotencyKey.eq(key.to_owned())))
        .one(tx)
        .await
        .map_err(map_scope_err)?;

    let Some(receipt) = existing else {
        return Ok(None);
    };
    if receipt.request_hash != request_hash {
        return Err(GraphStoreError::IdempotencyMismatch);
    }
    // A receipt from a previous epoch is treated exactly as an expired one:
    // the retry needs reconciliation, never automatic re-execution.
    if receipt.source_epoch != epoch {
        return Err(GraphStoreError::IdempotencyExpired);
    }
    let mut outcome = outcome_from_json(&receipt.response)?;
    outcome.replayed = true;
    Ok(Some(outcome))
}
