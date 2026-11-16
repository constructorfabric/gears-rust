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
    IngestOutcome, IngestRequest, ItemError, ItemFamily, ItemOutcome, NodeSpec, RemainingBudget,
    ReplaceScope, Subject,
};
use graph_storage_sdk::plugin_api::{EmbeddingPlan, GraphStoreError, StoreCtx};
use sea_orm::sea_query::{Expr, OnConflict};
use sea_orm::{
    ActiveValue, ColumnTrait, Condition, EntityTrait, ExprTrait, QueryFilter, QueryOrder,
    QuerySelect,
};
use time::OffsetDateTime;
use toolkit_db::secure::{
    DBRunner, SecureEntityExt, SecureInsertExt, SecureInsertManyExt, SecureUpdateExt,
};
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::embedding::{PlannedVector, StoredVector, VectorOutcome, decide_vector};
use crate::domain::identity;
use crate::domain::ownership;
use crate::domain::tally::IngestTally;
use crate::infra::projections::{
    EdgeEnds, EdgeHop, EdgeOwner, EdgeState, NodeIdent, NodeState, NodeTyped, TypeMeta,
    edge_ends_columns, edge_hop_columns, edge_owner_columns, edge_state_columns,
    node_ident_columns, node_state_columns, node_typed_columns, type_meta_columns,
};
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
///
/// Takes the two fields rather than the spec so a migration can recompose the
/// same text from a stored row: a rewritten payload whose lexical text still
/// describes the old one is a row that answers searches by a value it no
/// longer has.
pub(crate) fn compose_search_text(
    name: Option<&str>,
    payload: Option<&serde_json::Value>,
    paths: &[String],
) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(name) = name {
        parts.push(name.to_owned());
    }
    if let Some(payload) = payload {
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
    // `PostgreSQL` computes the increment from the conflicting row's own
    // value. This used to read the value, add one in Rust, and write that
    // number back -- and a read-compute-write loses under concurrency in a way
    // the row conflict does not save it from. Two transactions mutating
    // different rows of one tenant both read `N` and both prepare `N + 1`; the
    // second waits on the conflict, and then writes its own stale `N + 1` over
    // the committed one. Two distinct committed states, one revision, and the
    // contract this counter exists for -- a revision advances if and only if
    // stored state changed -- silently broken. It is easiest to see on a fresh
    // tenant, where both first writes answer `1`.
    let incremented = Expr::cust("to_jsonb(((graph_meta.value #>> '{}')::bigint) + 1)");
    let active = graph_meta::ActiveModel {
        tenant_id: ActiveValue::Set(tenant),
        key: ActiveValue::Set(graph_meta::KEY_GRAPH_REVISION.to_owned()),
        // No row yet: the insert stands, and the tenant's first committed
        // state is revision 1. This path is load-bearing rather than
        // defensive -- nothing bootstraps the meta rows.
        value: ActiveValue::Set(serde_json::json!(1)),
    };
    let on_conflict = toolkit_db::secure::SecureOnConflict::<graph_meta::Entity>::columns([
        graph_meta::Column::TenantId,
        graph_meta::Column::Key,
    ])
    .value(graph_meta::Column::Value, incremented)
    .map_err(map_scope_err)?;
    graph_meta::Entity::insert(active)
        .secure()
        .scope_unchecked(scope)
        .map_err(map_scope_err)?
        .on_conflict(on_conflict)
        .exec(runner)
        .await
        .map_err(map_scope_err)?;
    // Read back inside the same transaction, which sees the row this
    // statement just wrote -- whichever of the racing transactions this is.
    current_revision(scope, runner).await
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
    let budget = ctx.budget;
    // The producer is the writing principal, not a placeholder: the
    // idempotency key is documented as tenant- *and* producer-scoped, and a
    // scope's canonical identity includes its owning producer (Concurrent
    // Ingest Protocol, rules 2 and 4). Both columns existed from the first
    // migration and both were written empty, which made two producers in one
    // tenant share an idempotency namespace and let any writer replace any
    // other's scope.
    let producer = subject.principal();

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
                        budget,
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
    /// What is left of the request's absolute deadline.
    ///
    /// A producer-sized batch's updates are still one statement each inside
    /// one transaction holding one pool connection (new rows are inserted
    /// in bulk, gears-rust #5040), so "the deadline passed" has to be able
    /// to stop it between items. It cannot stop a
    /// statement already in flight -- that needs a server-side bound
    /// toolkit-db does not offer yet (gears-rust #4761) -- but it can stop
    /// the gear from starting the next ten thousand.
    budget: RemainingBudget,
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

    // --- scope replacement, part one: fence before anything else ----------
    // The generation check and the row lock come first, so a stale snapshot
    // is refused before it writes; the *removal* comes after the batch's own
    // writes, because "absent from the submitted batch" cannot be decided
    // until the batch is in.
    let mut tally = IngestTally::new(request.options.report_per_item);
    if let Some(replace) = &request.replace_scope {
        fence_scope(tenant, scope, producer, tx, replace, &request_hash).await?;
    }

    let types = resolve_types(scope, tx, &request).await?;
    let mut changed = false;

    let mut node_ids: BTreeMap<String, Endpoint> = BTreeMap::new();
    changed |= write_nodes(
        w,
        tx,
        &request,
        &types,
        &mut node_ids,
        &mut tally,
        embedding,
    )
    .await?;

    changed |= write_edges(w, tx, &request, &types, &mut node_ids, &mut tally).await?;

    // --- scope replacement, part two: remove what the batch did not name ---
    if let Some(replace) = &request.replace_scope {
        let written: std::collections::BTreeSet<String> = request
            .nodes
            .iter()
            .map(|spec| spec.node_key.clone())
            .collect();
        // The deterministic key of every edge this batch declared. What the
        // scope owns and this set does not contain is what the producer
        // removed -- including an edge whose endpoints both survived, which
        // the node reckoning alone can never notice.
        let declared_edges: std::collections::BTreeSet<String> = request
            .edges
            .iter()
            .filter_map(|spec| {
                let info = types.get(&spec.type_id)?;
                Some(identity::derive_edge_key(info.uuid, spec))
            })
            .collect();
        let (removed_nodes, removed_edges) = super::scope::remove_stale(
            scope,
            tx,
            &replace.attribute,
            &replace.value,
            &written,
            &declared_edges,
        )
        .await?;
        tally.counts.scope_removed_nodes = removed_nodes;
        tally.counts.scope_removed_edges = removed_edges;
        changed |= removed_nodes > 0 || removed_edges > 0;
    }

    // The revision advances if and only if stored state actually changed.
    let revision_value = if changed {
        bump_revision(tenant, scope, tx).await?
    } else {
        current_revision(scope, tx).await?
    };

    let (counts, per_item_nodes, per_item_edges) = tally.into_parts();
    let outcome = IngestOutcome {
        revision: GraphRevision {
            source_epoch: epoch,
            revision: revision_value,
        },
        replayed: false,
        counts,
        per_item_nodes,
        per_item_edges,
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
/// Take the scope's fence and settle the generation, atomically.
///
/// The obligation is "replacements of one scope serialize on that identity
/// through a lock held to commit, and the highest accepted generation is
/// compared and updated atomically under that lock". The first version read
/// the row, decided, and then wrote — three steps with no lock between them,
/// so two concurrent replacements both read the old generation, both passed
/// the check, and the loser's lower generation overwrote the winner's. The
/// compare *is* the write now:
///
/// `ON CONFLICT DO UPDATE SET generation = GREATEST(stored, offered)` keeps
/// the higher generation whoever arrives second, and takes the row lock for
/// the rest of the transaction — which is what serializes the two
/// replacements, since everything after it (the batch's writes and the stale
/// removal) happens while that lock is held. Reading the row back afterwards
/// therefore reads a settled value, and the decision is made on that.
///
/// When this was written the platform's secure ORM exposed no row-locking
/// surface; it now does (`lock_shared`, and a lock set inside `project_all`,
/// which the node phase uses). The upsert stays the instrument here because
/// the row may not exist yet, and a locking read cannot lock a row that is
/// not there.
async fn fence_scope(
    tenant: Uuid,
    scope: &AccessScope,
    producer: &str,
    tx: &impl DBRunner,
    replace: &ReplaceScope,
    request_hash: &str,
) -> Result<(), GraphStoreError> {
    let active = scope_registry::ActiveModel {
        tenant_id: ActiveValue::Set(tenant),
        scope_attribute: ActiveValue::Set(replace.attribute.clone()),
        scope_value: ActiveValue::Set(replace.value.clone()),
        owner_producer: ActiveValue::Set(producer.to_owned()),
        generation: ActiveValue::Set(replace.generation),
        request_hash: ActiveValue::Set(request_hash.to_owned()),
        updated_at: ActiveValue::Set(OffsetDateTime::now_utc()),
    };
    // Unqualified names on the right of `DO UPDATE SET` are the stored row;
    // `excluded` is what this statement offered.
    let keep_higher = Expr::cust("GREATEST(scope_registry.generation, excluded.generation)");
    // An unowned scope is claimed by its first writer, under this row lock —
    // the rule source namespaces already follow, and what keeps a deployment
    // whose rows predate producer identity working: those rows carry an empty
    // owner, and the producer that next replaces the scope adopts it.
    let claim_if_unowned = Expr::cust(
        "CASE WHEN scope_registry.owner_producer = '' \
         THEN excluded.owner_producer ELSE scope_registry.owner_producer END",
    );
    let hash_of_winner = Expr::cust(
        "CASE WHEN excluded.generation > scope_registry.generation \
         THEN excluded.request_hash ELSE scope_registry.request_hash END",
    );
    let on_conflict = toolkit_db::secure::SecureOnConflict::<scope_registry::Entity>::columns([
        scope_registry::Column::TenantId,
        scope_registry::Column::ScopeAttribute,
        scope_registry::Column::ScopeValue,
    ])
    .value(scope_registry::Column::Generation, keep_higher)
    .map_err(map_scope_err)?
    .value(scope_registry::Column::RequestHash, hash_of_winner)
    .map_err(map_scope_err)?
    .value(scope_registry::Column::OwnerProducer, claim_if_unowned)
    .map_err(map_scope_err)?
    .update_columns([scope_registry::Column::UpdatedAt])
    .map_err(map_scope_err)?;
    scope_registry::Entity::insert(active)
        .secure()
        .scope_unchecked(scope)
        .map_err(map_scope_err)?
        .on_conflict(on_conflict)
        .exec(tx)
        .await
        .map_err(map_scope_err)?;

    // Settled: the row is ours to read until commit.
    let row = scope_registry::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(
            Condition::all()
                .add(scope_registry::Column::ScopeAttribute.eq(replace.attribute.clone()))
                .add(scope_registry::Column::ScopeValue.eq(replace.value.clone())),
        )
        .one(tx)
        .await
        .map_err(map_scope_err)?
        .ok_or_else(|| {
            GraphStoreError::Internal("the scope fence vanished after being written".to_owned())
        })?;

    if row.owner_producer != producer {
        // A scope is one producer's declarative set. Letting another writer
        // replace it would delete rows it never had a view of, which is the
        // union state rule 3 of the protocol exists to prevent — and unlike
        // a stale generation, no retry makes it right.
        return Err(GraphStoreError::Conflict {
            reason: format!(
                "scope `{}={}` is owned by another producer; a replacement may only be \
                 submitted by its owner",
                replace.attribute, replace.value
            ),
        });
    }
    if row.generation > replace.generation {
        // Either it was already ahead, or a concurrent replacement won the
        // row while this one waited for it.
        return Err(GraphStoreError::StaleGeneration {
            recorded: row.generation,
            offered: replace.generation,
        });
    }
    if row.request_hash != request_hash {
        // Equal generation, different content: two snapshots claim to be the
        // same state of the source and disagree about what it is.
        return Err(GraphStoreError::Conflict {
            reason: "same source generation with different content".into(),
        });
    }

    // The fence is set and the row is locked for the rest of the transaction;
    // what the replacement removes is decided after the batch's own writes,
    // in `scope::remove_stale`.
    Ok(())
}

/// The live node an edge endpoint names, if there is one.
///
/// Live only: a tombstoned node reads as absent everywhere else, and an edge
/// linked to one would be a statement about a node no read returns.
async fn lookup_endpoint(
    scope: &AccessScope,
    tx: &impl DBRunner,
    key: &str,
) -> Result<Option<Endpoint>, GraphStoreError> {
    Ok(node::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(Condition::all().add(node::Column::NodeKey.eq(key.to_owned())))
        .filter(Condition::all().add(node::Column::DeletedAt.is_null()))
        .limit(1)
        .project_all(tx, |query| {
            node_typed_columns(query).into_model::<NodeTyped>()
        })
        .await
        .map_err(map_scope_err)?
        .into_iter()
        .next()
        .map(|m| Endpoint {
            id: m.id,
            type_id: m.gts_node_type_id,
        }))
}

async fn endpoint_is_tombstoned(
    scope: &AccessScope,
    tx: &impl DBRunner,
    key: &str,
) -> Result<bool, GraphStoreError> {
    Ok(node::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(Condition::all().add(node::Column::NodeKey.eq(key.to_owned())))
        .filter(Condition::all().add(node::Column::DeletedAt.is_not_null()))
        .limit(1)
        .project_all(tx, |query| {
            node_state_columns(query).into_model::<NodeState>()
        })
        .await
        .map_err(map_scope_err)?
        .into_iter()
        .next()
        .is_some())
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
        .project_all(tx, |query| {
            type_meta_columns(query).into_model::<TypeMeta>()
        })
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

/// How many keys one up-front read carries, and how many rows one bulk insert
/// carries.
///
/// Both are bounded by the server's 65 535 bind parameters: a key is one, a
/// node row about two dozen, so a thousand rows stay near a third of it. The
/// size does not bound what a batch locks -- every row a statement writes
/// stays locked until the transaction ends, however the batch is split -- so
/// the one constant sets only how many statements a batch takes.
const PREFETCH_CHUNK: usize = 1_000;

/// The live nodes named by `keys`, read in one statement per
/// [`PREFETCH_CHUNK`] keys rather than one per key (gears-rust #5040).
async fn lookup_endpoints(
    scope: &AccessScope,
    tx: &impl DBRunner,
    keys: &[&str],
) -> Result<BTreeMap<String, Endpoint>, GraphStoreError> {
    let mut found = BTreeMap::new();
    for chunk in keys.chunks(PREFETCH_CHUNK) {
        let rows = node::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(
                Condition::all()
                    .add(node::Column::NodeKey.is_in(chunk.iter().map(|key| (*key).to_owned()))),
            )
            .filter(Condition::all().add(node::Column::DeletedAt.is_null()))
            .project_all(tx, |query| {
                node_typed_columns(query).into_model::<NodeTyped>()
            })
            .await
            .map_err(map_scope_err)?;
        for row in rows {
            found.insert(
                row.node_key,
                Endpoint {
                    id: row.id,
                    type_id: row.gts_node_type_id,
                },
            );
        }
    }
    Ok(found)
}

/// The stored rows, tombstoned ones included, of the nodes a batch carries,
/// keyed by node key: what `upsert_node` would read one key at a time.
async fn stored_nodes(
    scope: &AccessScope,
    tx: &impl DBRunner,
    keys: &[&str],
) -> Result<BTreeMap<String, node::Model>, GraphStoreError> {
    let mut found = BTreeMap::new();
    for chunk in keys.chunks(PREFETCH_CHUNK) {
        let rows = node::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(
                Condition::all()
                    .add(node::Column::NodeKey.is_in(chunk.iter().map(|key| (*key).to_owned()))),
            )
            .all(tx)
            .await
            .map_err(map_scope_err)?;
        found.extend(rows.into_iter().map(|row| (row.node_key.clone(), row)));
    }
    Ok(found)
}

/// Which lock a prefetched node is read again under at its turn.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Relock {
    /// A node the batch carries and writes.
    Written,
    /// An endpoint the batch only links to.
    Linked,
}

/// What decides whether a step's prefetched row is read again, and with which
/// lock, before the step runs.
struct RelockState<'a> {
    request: &'a IngestRequest,
    stored: &'a BTreeMap<String, node::Model>,
    seen: &'a std::collections::BTreeSet<String>,
    relocked: &'a std::collections::BTreeSet<String>,
    prefetched: &'a std::collections::BTreeSet<String>,
}

impl RelockState<'_> {
    /// The lock a step's prefetched row is read again under, or `None` when
    /// it has no prefetched row or was read again already.
    fn mode(&self, step: &NodeStep<'_>) -> Option<Relock> {
        match *step {
            NodeStep::Node(index) => {
                let key = self.request.nodes[index].node_key.as_str();
                (self.stored.contains_key(key)
                    && !self.seen.contains(key)
                    && !self.relocked.contains(key))
                .then_some(Relock::Written)
            }
            NodeStep::Endpoint { key, .. } => (self.prefetched.contains(key)
                && !self.relocked.contains(key))
            .then_some(Relock::Linked),
        }
    }

    /// The keys of the consecutive steps from the first of `steps` that are
    /// read again under the same lock, at most a chunk of them.
    fn run_keys(&self, steps: &[NodeStep<'_>], mode: Relock) -> Vec<String> {
        let mut keys: Vec<String> = Vec::new();
        for step in steps {
            if self.mode(step) != Some(mode) || keys.len() == PREFETCH_CHUNK {
                break;
            }
            let key = step.key(self.request).to_owned();
            if keys.last() != Some(&key) {
                keys.push(key);
            }
        }
        keys
    }
}

/// Read a run of prefetched rows again under its lock and replace what the
/// up-front read said about them.
async fn relock_run(
    scope: &AccessScope,
    tx: &impl DBRunner,
    mode: Relock,
    keys: &[String],
    stored: &mut BTreeMap<String, node::Model>,
    node_ids: &mut BTreeMap<String, Endpoint>,
) -> Result<(), GraphStoreError> {
    match mode {
        Relock::Written => {
            let mut fresh = relock_rows(scope, tx, keys).await?;
            for key in keys {
                // Gone since the up-front read -- purged or removed by a
                // scope replacement -- the key is new again.
                match fresh.remove(key) {
                    Some(row) => stored.insert(key.clone(), row),
                    None => stored.remove(key),
                };
            }
        }
        Relock::Linked => {
            let mut fresh = relock_endpoints(scope, tx, keys).await?;
            for key in keys {
                // Tombstoned or gone since: the per-key resolution decides,
                // refusal or phantom included.
                match fresh.remove(key) {
                    Some(endpoint) => node_ids.insert(key.clone(), endpoint),
                    None => node_ids.remove(key),
                };
            }
        }
    }
    Ok(())
}

/// The keys of the consecutive edges from the first of `checked` whose
/// up-front row has not been read again yet, at most a chunk of them; empty
/// when the first has none.
fn stale_edge_run(
    checked: &[(String, usize, Endpoint, Endpoint)],
    stored: &BTreeMap<String, edge::Model>,
    seen: &std::collections::BTreeSet<String>,
    relocked: &std::collections::BTreeSet<String>,
) -> Vec<String> {
    let stale =
        |key: &String| stored.contains_key(key) && !seen.contains(key) && !relocked.contains(key);
    let mut keys: Vec<String> = Vec::new();
    for (key, ..) in checked {
        if !stale(key) || keys.len() == PREFETCH_CHUNK {
            break;
        }
        if keys.last() != Some(key) {
            keys.push(key.clone());
        }
    }
    keys
}

/// Read a run of stored edges again under their lock and replace what the
/// up-front read said about them.
async fn relock_edge_run(
    scope: &AccessScope,
    tx: &impl DBRunner,
    keys: &[String],
    stored: &mut BTreeMap<String, edge::Model>,
) -> Result<(), GraphStoreError> {
    let mut fresh = relock_edge_rows(scope, tx, keys).await?;
    for key in keys {
        match fresh.remove(key) {
            Some(row) => stored.insert(key.clone(), row),
            // Gone since -- removed by a scope replacement or a purge -- the
            // edge is new again.
            None => stored.remove(key),
        };
    }
    Ok(())
}

/// The stored edges of `keys`, tombstoned ones included, read under
/// `FOR NO KEY UPDATE` in key order -- the lock their update takes anyway.
async fn relock_edge_rows(
    scope: &AccessScope,
    tx: &impl DBRunner,
    keys: &[String],
) -> Result<BTreeMap<String, edge::Model>, GraphStoreError> {
    let rows: Vec<edge::Model> = edge::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(Condition::all().add(edge::Column::EdgeKey.is_in(keys.to_vec())))
        .project_all(tx, |query| {
            query
                .order_by_asc(edge::Column::EdgeKey)
                .lock(sea_orm::sea_query::LockType::NoKeyUpdate)
                .into_model::<edge::Model>()
        })
        .await
        .map_err(map_scope_err)?;
    Ok(rows
        .into_iter()
        .map(|row| (row.edge_key.clone(), row))
        .collect())
}

/// The stored rows of `keys`, tombstoned ones included, read under
/// `FOR NO KEY UPDATE` in key order -- the lock their update takes anyway.
async fn relock_rows(
    scope: &AccessScope,
    tx: &impl DBRunner,
    keys: &[String],
) -> Result<BTreeMap<String, node::Model>, GraphStoreError> {
    let rows: Vec<node::Model> = node::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(Condition::all().add(node::Column::NodeKey.is_in(keys.to_vec())))
        .project_all(tx, |query| {
            query
                .order_by_asc(node::Column::NodeKey)
                .lock(sea_orm::sea_query::LockType::NoKeyUpdate)
                .into_model::<node::Model>()
        })
        .await
        .map_err(map_scope_err)?;
    Ok(rows
        .into_iter()
        .map(|row| (row.node_key.clone(), row))
        .collect())
}

/// The live endpoints among `keys`, read under `FOR SHARE` in key order.
async fn relock_endpoints(
    scope: &AccessScope,
    tx: &impl DBRunner,
    keys: &[String],
) -> Result<BTreeMap<String, Endpoint>, GraphStoreError> {
    let rows: Vec<NodeTyped> = node::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(Condition::all().add(node::Column::NodeKey.is_in(keys.to_vec())))
        .filter(Condition::all().add(node::Column::DeletedAt.is_null()))
        .project_all(tx, |query| {
            node_typed_columns(query)
                .order_by_asc(node::Column::NodeKey)
                .lock(sea_orm::sea_query::LockType::Share)
                .into_model::<NodeTyped>()
        })
        .await
        .map_err(map_scope_err)?;
    Ok(rows
        .into_iter()
        .map(|row| {
            (
                row.node_key,
                Endpoint {
                    id: row.id,
                    type_id: row.gts_node_type_id,
                },
            )
        })
        .collect())
}

/// The stored rows, tombstoned ones included, of the edges a batch carries,
/// keyed by edge key: what `upsert_edge` would read one key at a time.
async fn stored_edges(
    scope: &AccessScope,
    tx: &impl DBRunner,
    keys: &[&str],
) -> Result<BTreeMap<String, edge::Model>, GraphStoreError> {
    let mut found = BTreeMap::new();
    for chunk in keys.chunks(PREFETCH_CHUNK) {
        let rows = edge::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(
                Condition::all()
                    .add(edge::Column::EdgeKey.is_in(chunk.iter().map(|key| (*key).to_owned()))),
            )
            .all(tx)
            .await
            .map_err(map_scope_err)?;
        found.extend(rows.into_iter().map(|row| (row.edge_key.clone(), row)));
    }
    Ok(found)
}

/// What a prefetch answered for one item of the batch.
///
/// `Some(row)` is the prefetched answer, absent included; `None` means "read
/// it now", which is what a key the batch carries a second time gets, so the
/// second occurrence sees what the first one wrote.
type Known<T> = Option<Option<T>>;

/// Take the prefetched answer for `key` on its first occurrence in the batch.
fn known<T>(
    stored: &mut BTreeMap<String, T>,
    seen: &mut std::collections::BTreeSet<String>,
    key: &str,
) -> Known<T> {
    seen.insert(key.to_owned()).then(|| stored.remove(key))
}

/// The types of a batch's edge endpoints, read once per type rather than once
/// per edge: interned types are not changed by an ingest, so an answer stays
/// true for the rest of the transaction.
#[derive(Default)]
struct EndpointTypeCache(BTreeMap<i32, (String, Option<String>)>);

impl EndpointTypeCache {
    async fn resolve(
        &mut self,
        scope: &AccessScope,
        tx: &impl DBRunner,
        ids: &[i32],
    ) -> Result<BTreeMap<i32, (String, Option<String>)>, GraphStoreError> {
        let missing: Vec<i32> = ids
            .iter()
            .copied()
            .filter(|id| !self.0.contains_key(id))
            .collect();
        if !missing.is_empty() {
            self.0.extend(endpoint_types(scope, tx, &missing).await?);
        }
        Ok(ids
            .iter()
            .filter_map(|id| self.0.get(id).map(|meta| (*id, meta.clone())))
            .collect())
    }
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
        .project_all(tx, |query| edge_hop_columns(query).into_model::<EdgeHop>())
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
        .project_all(tx, |query| {
            type_meta_columns(query).into_model::<TypeMeta>()
        })
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
/// DESIGN § 3.7 (`node` table), since the FR names the states and not their
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

#[expect(
    clippy::too_many_arguments,
    reason = "one node write over the transaction's working state; the namespace set is the batch's, shared with the bulk path"
)]
async fn upsert_node(
    w: Writer<'_>,
    tx: &impl DBRunner,
    spec: &NodeSpec,
    info: &TypeInfo,
    index: usize,
    planned: PlannedVector<'_>,
    known: Known<node::Model>,
    authorized: &mut std::collections::BTreeSet<String>,
) -> Result<(i64, ItemOutcome), GraphStoreError> {
    let (tenant, scope, subject) = (w.tenant, w.scope, w.subject);
    let existing = match known {
        Some(row) => row,
        None => node::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(Condition::all().add(node::Column::NodeKey.eq(spec.node_key.clone())))
            .one(tx)
            .await
            .map_err(map_scope_err)?,
    };

    let payload = spec
        .payload
        .clone()
        .unwrap_or_else(|| serde_json::json!({}));
    let name = spec.name.clone().unwrap_or_default();
    let search_text = compose_search_text(
        spec.name.as_deref(),
        spec.payload.as_ref(),
        &info.full_text_search,
    );
    let mut vector = plan_vector(existing.as_ref(), planned);
    let now = OffsetDateTime::now_utc();

    // The ownership boundary, before either branch writes anything: a
    // reference node names a source namespace in its own payload, and a
    // payload proves nothing about who may speak for it
    // (`fr-source-ownership`). An unclaimed namespace is claimed here; someone
    // else's is refused with `permission_denied`, for an update exactly as for
    // an insert, because an overwrite of another producer's projection is the
    // thing the boundary exists to stop.
    let namespace = match ownership::namespace_of(info.family.as_deref(), spec.payload.as_ref())
        .map_err(|error| item_error(index, ItemFamily::Node, &spec.type_id, error.to_string()))?
    {
        ownership::Namespaced::None => None,
        ownership::Namespaced::Under(namespace) => {
            // Once per batch, whichever path meets the namespace first: the
            // claim and the check are the same for every node that names it.
            if authorized.insert(namespace.to_owned()) {
                let writer = subject.principal();
                super::namespaces::authorize_write(tenant, scope, tx, namespace, &writer).await?;
            }
            Some(namespace.to_owned())
        }
    };

    let current = if let Some(current) = existing {
        current
    } else {
        // `expected_version` is a statement about a stored row. `Some(0)`
        // says "there must be none" -- the one conditional a caller can make
        // with no version to read back, since a stored version is 1 or more
        // -- and it holds here. Any other expectation names a row that is
        // not there, and inserting under it would tell the caller its
        // compare-and-set passed against a version that never existed.
        if let Some(expected) = spec.expected_version
            && expected != 0
        {
            return Err(GraphStoreError::Conflict {
                reason: format!(
                    "expected version {expected}, but no node is stored under key `{}`",
                    spec.node_key
                ),
            });
        }
        let active = new_node_row(
            w,
            spec,
            info,
            NewNode {
                name: name.clone(),
                payload: payload.clone(),
                search_text: search_text.clone(),
                vector,
                namespace,
                now,
            },
        );
        if let Some(id) = insert_node_row(scope, tx, active).await? {
            return Ok((id, ItemOutcome::Inserted));
        }
        let settled = settle_lost_insert(scope, tx, spec).await?;
        vector = plan_vector(Some(&settled), planned);
        settled
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
            // A conflict, not a validation failure: the payload may be
            // perfectly valid under the new type, and the only permitted
            // transition is phantom materialization.
            return Err(GraphStoreError::Conflict {
                reason: format!(
                    "node `{}` is already registered under a different type; a same-key \
                     ingest may not change it",
                    spec.node_key
                ),
            });
        }
    }

    // Compared here so a mismatch is reported with both numbers, and
    // compared *again* in the statement below, which is where it is actually
    // decided: this read and that write are two moments, and a concurrent
    // ingest fits between them.
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
        return Ok((current.id, ItemOutcome::Unchanged));
    }

    if materializing {
        revalidate_incident_edges(scope, tx, current.id, &spec.type_id, index).await?;
    }

    let id = current.id;
    // `PostgreSQL` increments the row's own value. Computing `current.version
    // + 1` here and writing it as a literal is the same read-compute-write
    // that gave two committed states one graph revision: two ingests of this
    // node both read `N`, the second waits on the row lock and then writes
    // its own stale `N + 1`. That is worse for `version` than for the
    // revision, because `version` is the only optimistic-concurrency token
    // this gear gives a caller -- an `expected_version` that should have
    // failed would pass.
    let mut update = node::Entity::update_many().col_expr(
        node::Column::Version,
        Expr::col(node::Column::Version).add(1),
    );
    // And the compare-and-set belongs in the statement rather than in the
    // branch above it. Filtering on the version we read means a concurrent
    // writer that moved it leaves this update matching nothing, which is
    // reported as the conflict it is instead of silently overwriting.
    if let Some(expected) = spec.expected_version {
        update = update.filter(Condition::all().add(node::Column::Version.eq(expected)));
    }
    // The tombstone check above reads the row before this statement writes
    // it, so on its own it is advice rather than a boundary: a delete that
    // commits in between leaves the check passed and the row tombstoned. The
    // filter is what makes the check hold at the instant of the write.
    // Unconditional, unlike the version filter -- a caller does not opt into
    // "do not resurrect what someone just deleted", and `soft_delete` never
    // touches `version`, so the version CAS cannot see a delete either.
    update = update.filter(Condition::all().add(node::Column::DeletedAt.is_null()));
    let written = update
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

    // Nothing matched means the row moved between the read and the write, and
    // only a filter in the statement can be here to notice. Answering
    // `Updated` on zero rows would tell the caller its write landed.
    //
    // Which filter missed is worth separating, because the three ask
    // different things of the caller. The row is re-read to say which.
    //
    // A row still there with `deleted_at` set is the tombstone: not
    // retryable at all before purge. A row still there without it changed
    // version under us: retryable against the version it has now. A row that
    // has vanished entirely is neither -- a scope replacement removes what it
    // no longer declares with a hard delete, so the row and its key are
    // already gone and the key is free this instant. Reporting that as the
    // tombstone told the caller to wait for a purge that had just happened,
    // and a retry policy reading "not before purge" would back off instead of
    // simply re-ingesting.
    //
    // The tombstone and the moved-version arms are both reached under real
    // contention by `a_delete_racing_an_upsert_leaves_no_rewritten_tombstone`.
    // The vanished arm is not: it needs a hard delete to commit inside this
    // transaction's window, which no seam in the public API can hold open.
    if written.rows_affected == 0 {
        let settled = node::Entity::find()
            .filter(Condition::all().add(node::Column::Id.eq(id)))
            .secure()
            .scope_with(scope)
            .limit(1)
            .project_all(tx, |query| {
                node_state_columns(query).into_model::<NodeState>()
            })
            .await
            .map_err(map_scope_err)?
            .into_iter()
            .next();
        return Err(GraphStoreError::Conflict {
            reason: match settled {
                Some(row) if row.deleted_at.is_some() => format!(
                    "node key `{}` was tombstoned while this write was being prepared, and a \
                     tombstoned key cannot be re-ingested before purge",
                    spec.node_key
                ),
                Some(_) => format!(
                    "node `{}` changed between the check and the write; re-read it and retry \
                     with the version it has now",
                    spec.node_key
                ),
                None => format!(
                    "node `{}` was removed while this write was being prepared; the key is \
                     free again, so re-ingest it",
                    spec.node_key
                ),
            },
        });
    }

    Ok((
        id,
        if materializing {
            ItemOutcome::Materialized
        } else {
            ItemOutcome::Updated
        },
    ))
}

/// Insert a new node row, or yield to a batch that inserted the same key
/// first: `Some(id)` when this insert wrote the row, `None` when another one
/// did.
///
/// Two batches that both found no row both insert, and the second used to
/// lose a unique violation on `(tenant_id, node_key)` -- a conflict on the
/// whole batch over a key both producers were entitled to write (gears-rust
/// #5042). `DO NOTHING` lets it wait for the first and then find the row
/// (`settle_lost_insert`), and a serial order in which the first inserted and
/// this one then updated is exactly what the caller's update branch writes.
/// The same shape as `insert_phantom`, including how the elided insert is
/// reported.
/// What a new node row carries besides its key, type and writer.
struct NewNode {
    name: String,
    payload: serde_json::Value,
    search_text: String,
    vector: VectorWrite,
    namespace: Option<String>,
    now: OffsetDateTime,
}

/// The row a node is inserted as, by the item path and the bulk insert alike.
fn new_node_row(
    w: Writer<'_>,
    spec: &NodeSpec,
    info: &TypeInfo,
    new: NewNode,
) -> node::ActiveModel {
    let subject = w.subject;
    node::ActiveModel {
        tenant_id: ActiveValue::Set(w.tenant),
        id: ActiveValue::NotSet,
        node_key: ActiveValue::Set(spec.node_key.clone()),
        gts_node_type_id: ActiveValue::Set(info.id),
        name: ActiveValue::Set(new.name),
        payload: ActiveValue::Set(new.payload),
        search_text: ActiveValue::Set(new.search_text),
        embedding: ActiveValue::Set(new.vector.embedding),
        embedding_epoch: ActiveValue::Set(new.vector.epoch),
        embedding_input_hash: ActiveValue::Set(new.vector.input_hash),
        // Written once, on insert, and never by an upsert: the row's record
        // of who created it is provenance, and provenance that a later write
        // can rewrite is not provenance. Who may write the namespace *now* is
        // the registry's answer, not this column's.
        source_namespace: ActiveValue::Set(new.namespace),
        owner_principal: ActiveValue::Set(subject.principal()),
        version: ActiveValue::Set(1),
        created_at: ActiveValue::Set(new.now),
        updated_at: ActiveValue::Set(new.now),
        deleted_at: ActiveValue::Set(None),
        created_by_subject_id: ActiveValue::Set(subject.subject_id),
        created_by_subject_type: ActiveValue::Set(subject.subject_type.clone()),
        updated_by_subject_id: ActiveValue::Set(subject.subject_id),
        updated_by_subject_type: ActiveValue::Set(subject.subject_type.clone()),
        deleted_by_subject_id: ActiveValue::Set(None),
        deleted_by_subject_type: ActiveValue::Set(None),
    }
}

/// The conflict clause of a bulk insert: `ON CONFLICT (tenant_id, key) DO
/// UPDATE SET key = EXCLUDED.key WHERE false`.
///
/// It inserts what `DO NOTHING` inserts and returns what it returns -- a row
/// whose update `WHERE false` rejects is not in `RETURNING` -- but PostgreSQL
/// locks a conflicting row even when the update's condition is false. So a key
/// another batch took since the up-front read is locked here, inside the
/// statement and in its key order, rather than later by the item path, after
/// the rows behind it in the run are already locked; the batch then takes
/// every lock in key order, as #5042 requires (gears-rust #5040).
fn locking_conflict<C>(tenant: C, key: C) -> OnConflict
where
    C: sea_orm::sea_query::IntoIden + Copy + 'static,
{
    OnConflict::columns([tenant, key])
        .update_column(key)
        .action_and_where(Expr::val(false))
        .to_owned()
}

/// Insert `rows` -- new keys, in key order -- in one statement per
/// [`PREFETCH_CHUNK`] rows, and return the keys and ids of the rows that
/// landed.
///
/// `RETURNING` returns only the rows the statement inserted, so a key another
/// batch inserted since the up-front read is absent from the answer -- and,
/// with `conflict` from [`locking_conflict`], already locked by this
/// transaction in key order. The caller writes it on the item path, which
/// settles a lost race as before (gears-rust #5040).
async fn insert_new_rows<E>(
    scope: &AccessScope,
    tx: &impl DBRunner,
    budget: RemainingBudget,
    rows: Vec<E::ActiveModel>,
    conflict: OnConflict,
    key_and_id: impl Fn(&E::Model) -> (String, i64),
) -> Result<BTreeMap<String, i64>, GraphStoreError>
where
    E: sea_orm::EntityTrait + toolkit_db::secure::ScopableEntity,
    E::ActiveModel: sea_orm::ActiveModelTrait<Entity = E> + Send,
    E::Model: sea_orm::IntoActiveModel<E::ActiveModel>,
{
    let mut landed = BTreeMap::new();
    let mut rest = rows;
    while !rest.is_empty() {
        // Between statements, as the item loops check between items: a batch
        // past its deadline does not start its next thousand rows.
        if budget.is_exhausted() {
            return Err(GraphStoreError::Deadline);
        }
        let take = PREFETCH_CHUNK.min(rest.len());
        let chunk: Vec<E::ActiveModel> = rest.drain(..take).collect();
        // Unlike the single-row insert, a chunk whose every row lost its race
        // is not an error: the returning select reads all rows, and none is
        // an empty answer.
        let models = E::insert_many(chunk)
            .secure()
            .scope_unchecked(scope)
            .map_err(map_scope_err)?
            .on_conflict_raw(conflict.clone())
            .exec_with_returning(tx)
            .await
            .map_err(map_scope_err)?;
        landed.extend(models.iter().map(&key_and_id));
    }
    Ok(landed)
}

async fn insert_node_row(
    scope: &AccessScope,
    tx: &impl DBRunner,
    active: node::ActiveModel,
) -> Result<Option<i64>, GraphStoreError> {
    let inserted = node::Entity::insert(active)
        .secure()
        .scope_unchecked(scope)
        .map_err(map_scope_err)?
        .on_conflict_raw(
            OnConflict::columns([node::Column::TenantId, node::Column::NodeKey])
                .do_nothing()
                .to_owned(),
        )
        .exec_with_returning(tx)
        .await;
    match inserted {
        Ok(model) => Ok(Some(model.id)),
        Err(error) if lost_insert_race(&error) => Ok(None),
        Err(error) => Err(map_scope_err(error)),
    }
}

/// Whether an `INSERT ... ON CONFLICT DO NOTHING RETURNING` lost its race:
/// the conflict elided the insert, so `RETURNING` produced no row.
///
/// The one place that reads the error as the race, for every insert that
/// settles one (`insert_node_row`, `insert_phantom`, the edge insert). On a
/// backend with `RETURNING` -- PostgreSQL, the only one this store runs on --
/// `SeaORM` reports the empty answer from `exec_with_returning` as
/// `RecordNotFound`: its returning select found nothing. `RecordNotInserted`
/// is what a backend *without* `RETURNING` says, so here it would be
/// something else, and it surfaces as an error.
/// `an_elided_insert_that_asks_for_its_row_reports_record_not_found` pins the
/// variant against the server, which is why it is decided here once rather
/// than spelled out at each insert.
#[must_use]
pub fn lost_insert_race(error: &toolkit_db::secure::ScopeError) -> bool {
    matches!(
        error,
        toolkit_db::secure::ScopeError::Db(sea_orm::DbErr::RecordNotFound(_))
    )
}

/// The row another batch inserted under this key while this one was about
/// to, read so this write can land on it as an update.
async fn settle_lost_insert(
    scope: &AccessScope,
    tx: &impl DBRunner,
    spec: &NodeSpec,
) -> Result<node::Model, GraphStoreError> {
    // "There must be none" was true when this batch looked and is not now:
    // that is the claim `Some(0)` exists to lose.
    if spec.expected_version == Some(0) {
        return Err(GraphStoreError::Conflict {
            reason: format!(
                "expected no node under key `{}`, but another batch created it first",
                spec.node_key
            ),
        });
    }
    node::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(Condition::all().add(node::Column::NodeKey.eq(spec.node_key.clone())))
        .one(tx)
        .await
        .map_err(map_scope_err)?
        .ok_or_else(|| GraphStoreError::Conflict {
            reason: format!(
                "node `{}` was created and removed while this write was being prepared; the \
                 key is free again, so re-ingest it",
                spec.node_key
            ),
        })
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
        .limit(1)
        .project_all(tx, |query| {
            type_meta_columns(query).into_model::<TypeMeta>()
        })
        .await
        .map_err(map_scope_err)?
        .into_iter()
        .next();
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
    // A phantom is not the caller's write. The batch named an endpoint that
    // did not exist and the gear materialized it, so two producers whose
    // edges reference the same new node are both right and neither asked for
    // this row. A plain insert made one of them lose a unique violation --
    // reported as a conflict on a twenty-thousand-edge batch, over a node
    // nothing in the request mentioned, with no way for the producer to
    // predict or avoid it. `DO NOTHING` lets the loser converge on the
    // winner's row instead, which is what a materialization that happens
    // behind the caller's back has to do.
    //
    // `on_conflict_raw` because the clause updates nothing: there is no
    // column list for the tenant-immutability check to validate, and the
    // winner's row is kept exactly as it was written.
    let inserted = node::Entity::insert(active)
        .secure()
        .scope_unchecked(scope)
        .map_err(map_scope_err)?
        .on_conflict_raw(
            OnConflict::columns([node::Column::TenantId, node::Column::NodeKey])
                .do_nothing()
                .to_owned(),
        )
        .exec_with_returning(tx)
        .await;

    match inserted {
        Ok(model) => Ok(model.id),
        // Nothing was inserted, so somebody else got there first. Read their
        // row and use it -- the endpoint the edge names is that node.
        Err(error) if lost_insert_race(&error) => {
            let settled = node::Entity::find()
                .secure()
                .scope_with(scope)
                .filter(Condition::all().add(node::Column::NodeKey.eq(key.to_owned())))
                .filter(Condition::all().add(node::Column::DeletedAt.is_null()))
                .limit(1)
                .project_all(tx, |query| {
                    node_ident_columns(query).into_model::<NodeIdent>()
                })
                .await
                .map_err(map_scope_err)?
                .into_iter()
                .next();
            // Unless what they wrote is already gone. A tombstoned key is not
            // reusable before purge, and materializing an endpoint onto one
            // would resurrect it by the back door.
            settled
                .map(|row| row.id)
                .ok_or_else(|| GraphStoreError::Conflict {
                    reason: format!(
                        "node key `{key}` was created and tombstoned while this batch was \
                     materializing it as an edge endpoint; it cannot be re-ingested \
                     before purge"
                    ),
                })
        }
        Err(error) => Err(map_scope_err(error)),
    }
}

/// What a new edge row carries besides its spec and writer.
struct NewEdge<'a> {
    key: String,
    type_id: i32,
    src: i64,
    dst: i64,
    payload: serde_json::Value,
    declaring: Option<(&'a str, &'a str)>,
    now: OffsetDateTime,
}

/// The row an edge is inserted as, by the item path and the bulk insert alike.
fn new_edge_row(w: Writer<'_>, spec: &EdgeSpec, new: NewEdge<'_>) -> edge::ActiveModel {
    let subject = w.subject;
    edge::ActiveModel {
        tenant_id: ActiveValue::Set(w.tenant),
        id: ActiveValue::NotSet,
        edge_key: ActiveValue::Set(new.key),
        gts_edge_type_id: ActiveValue::Set(new.type_id),
        src_node_id: ActiveValue::Set(new.src),
        dst_node_id: ActiveValue::Set(new.dst),
        discriminator: ActiveValue::Set(spec.discriminator.clone()),
        payload: ActiveValue::Set(new.payload),
        created_at: ActiveValue::Set(new.now),
        updated_at: ActiveValue::Set(new.now),
        deleted_at: ActiveValue::Set(None),
        scope_attribute: ActiveValue::Set(new.declaring.map(|(attribute, _)| attribute.to_owned())),
        scope_value: ActiveValue::Set(new.declaring.map(|(_, value)| value.to_owned())),
        created_by_subject_id: ActiveValue::Set(subject.subject_id),
        created_by_subject_type: ActiveValue::Set(subject.subject_type.clone()),
        updated_by_subject_id: ActiveValue::Set(subject.subject_id),
        updated_by_subject_type: ActiveValue::Set(subject.subject_type.clone()),
        deleted_by_subject_id: ActiveValue::Set(None),
        deleted_by_subject_type: ActiveValue::Set(None),
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "one item of the edge phase over the transaction's working state; bundling it would name a struct for a single call site"
)]
async fn upsert_edge(
    w: Writer<'_>,
    tx: &impl DBRunner,
    spec: &EdgeSpec,
    info: &TypeInfo,
    src: i64,
    dst: i64,
    declaring: Option<(&str, &str)>,
    known: Known<edge::Model>,
) -> Result<ItemOutcome, GraphStoreError> {
    let (scope, subject) = (w.scope, w.subject);
    let edge_key = identity::derive_edge_key(info.uuid, spec);
    let now = OffsetDateTime::now_utc();
    let payload = spec
        .payload
        .clone()
        .unwrap_or_else(|| serde_json::json!({}));

    let existing = match known {
        Some(row) => row,
        None => edge::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(Condition::all().add(edge::Column::EdgeKey.eq(edge_key.clone())))
            .one(tx)
            .await
            .map_err(map_scope_err)?,
    };

    let current = if let Some(current) = existing {
        current
    } else {
        let active = new_edge_row(
            w,
            spec,
            NewEdge {
                key: edge_key.clone(),
                type_id: info.id,
                src,
                dst,
                payload: payload.clone(),
                declaring,
                now,
            },
        );
        // Two batches declaring one new edge both found no row, and the
        // second lost a unique violation on `(tenant_id, edge_key)` -- the
        // edge half of what `insert_node_row` settles for nodes (gears-rust
        // #5042). `DO NOTHING` waits for the first; this write then lands on
        // its row through the update below, which also decides ownership
        // between two declaring scopes exactly as for an edge that existed.
        let inserted = edge::Entity::insert(active)
            .secure()
            .scope_unchecked(scope)
            .map_err(map_scope_err)?
            .on_conflict_raw(
                OnConflict::columns([edge::Column::TenantId, edge::Column::EdgeKey])
                    .do_nothing()
                    .to_owned(),
            )
            .exec_with_returning(tx)
            .await;
        match inserted {
            Ok(_) => return Ok(ItemOutcome::Inserted),
            Err(error) if lost_insert_race(&error) => {}
            Err(error) => return Err(map_scope_err(error)),
        }
        edge::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(Condition::all().add(edge::Column::EdgeKey.eq(edge_key.clone())))
            .one(tx)
            .await
            .map_err(map_scope_err)?
            .ok_or_else(|| GraphStoreError::Conflict {
                reason: format!(
                    "edge `{edge_key}` was created and removed while this write was being \
                     prepared; the key is free again, so re-ingest it"
                ),
            })?
    };

    // Ownership is bookkeeping about who declared the edge, not content a
    // reader can observe, so re-declaring an otherwise identical edge claims
    // it without making the batch a change: the revision must not advance for
    // a convergent replay. The claim still has to happen, or an edge first
    // written by an unscoped ingest would stay unowned and never converge.
    //
    // **An owned edge is not adopted away from its owner.** Overwriting the
    // mark whenever the declaring scope differed made ownership follow
    // whoever wrote last: a second scope re-declaring the same edge took it,
    // and from then on the first scope's replacement no longer removed it
    // while the second one's did. The producer that lost the edge was told
    // nothing. That is the union-state problem the scope registry already
    // refuses for a whole scope, so an edge answers the same way -- a
    // conflict, which the caller can act on, rather than a silent transfer it
    // cannot see.
    let owned_by = current
        .scope_attribute
        .as_deref()
        .zip(current.scope_value.as_deref());
    if let (Some((attribute, value)), Some(owner)) = (declaring, owned_by)
        && owner != (attribute, value)
    {
        return Err(GraphStoreError::Conflict {
            reason: format!(
                "edge `{edge_key}` was declared by scope `{}={}` and may not be \
                 re-declared under `{attribute}={value}`; a move between scopes \
                 is a deletion and a re-declaration, not a write",
                owner.0, owner.1
            ),
        });
    }
    let claim = declaring.filter(|(attribute, value)| {
        current.scope_attribute.as_deref() != Some(*attribute)
            || current.scope_value.as_deref() != Some(*value)
    });
    // The guard above reads the ownership; the write has to hold it. Two
    // scopes can both read the edge unowned before either commits, and then
    // neither is refused: whichever UPDATE ran second overwrote the first's
    // mark, and nobody was told. So a scoped write is a compare-and-set on
    // the ownership it read, and a write that matched nothing was beaten to
    // the claim -- it answers the conflict a known owner answers, found by
    // reading who got there first.
    let ownership_as_read = ownership_as_read(&current);
    if current.payload == payload && current.deleted_at.is_none() {
        if let Some((attribute, value)) = claim {
            let claimed = edge::Entity::update_many()
                .col_expr(
                    edge::Column::ScopeAttribute,
                    Expr::value(Some(attribute.to_owned())),
                )
                .col_expr(
                    edge::Column::ScopeValue,
                    Expr::value(Some(value.to_owned())),
                )
                .filter(Condition::all().add(edge::Column::Id.eq(current.id)))
                .filter(ownership_as_read)
                .secure()
                .scope_with(scope)
                .exec(tx)
                .await
                .map_err(map_scope_err)?;
            if claimed.rows_affected == 0 {
                return Err(lost_write(scope, tx, current.id, &edge_key, declaring).await);
            }
        }
        return Ok(ItemOutcome::Unchanged);
    }

    let id = current.id;
    let mut update = edge::Entity::update_many();
    if let Some((attribute, value)) = claim {
        update = update
            .col_expr(
                edge::Column::ScopeAttribute,
                Expr::value(Some(attribute.to_owned())),
            )
            .col_expr(
                edge::Column::ScopeValue,
                Expr::value(Some(value.to_owned())),
            );
    }
    if declaring.is_some() {
        update = update.filter(ownership_as_read);
    }
    let written = update
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

    // Zero rows means the row this write was prepared against is not the row
    // that is there now, and answering `Updated` would tell the caller its
    // payload landed when nothing holds it. Two things can have happened.
    // The row is gone: a scope replacement removes an edge it no longer
    // declares with a hard delete, not a tombstone, and these transactions
    // begin without setting an isolation level, so at the server default the
    // row can disappear between the read above and this statement rather
    // than raising a serialization failure. Or, for a scoped write, another
    // scope's claim landed first and the ownership filter matched nothing.
    // `lost_write` re-reads the row to say which.
    //
    // Unlike a node, an edge has no tombstone conflict to report here: a
    // tombstoned edge is deliberately revived by this very statement, which
    // clears `deleted_at`. The removed case is retryable -- the key is free,
    // so a re-ingest inserts rather than updates -- and the claimed case is
    // the conflict a known owner answers.
    //
    // The removed case is not covered by a case, and said so rather than
    // left to be assumed: reaching it needs a hard delete to commit between
    // the read above and this write, inside one transaction with no seam to
    // hold it open at. What *is* covered is that a deleted edge is revived
    // rather than refused (`a_deleted_edge_is_revived_by_the_next_upsert`),
    // and the claim race, which
    // `two_scopes_racing_to_claim_an_unowned_edge_leave_it_with_one` opens
    // against the built-in store with a barrier.
    if written.rows_affected == 0 {
        return Err(lost_write(scope, tx, id, &edge_key, declaring).await);
    }

    Ok(ItemOutcome::Updated)
}

/// The ownership an edge was read with, as the predicate its write must
/// still match: the pair the row carried, or its absence.
fn ownership_as_read(current: &edge::Model) -> Condition {
    Condition::all()
        .add(match &current.scope_attribute {
            Some(attribute) => edge::Column::ScopeAttribute.eq(attribute.clone()),
            None => edge::Column::ScopeAttribute.is_null(),
        })
        .add(match &current.scope_value {
            Some(value) => edge::Column::ScopeValue.eq(value.clone()),
            None => edge::Column::ScopeValue.is_null(),
        })
}

/// What an edge write that matched nothing found in the row's place: another
/// scope's claim, or no row at all. Read rather than guessed, because the
/// two ask different things of the caller.
async fn lost_write(
    scope: &AccessScope,
    tx: &impl DBRunner,
    id: i64,
    edge_key: &str,
    declaring: Option<(&str, &str)>,
) -> GraphStoreError {
    let row = match edge::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(Condition::all().add(edge::Column::Id.eq(id)))
        .limit(1)
        .project_all(tx, |query| {
            edge_owner_columns(query).into_model::<EdgeOwner>()
        })
        .await
    {
        Ok(rows) => rows.into_iter().next(),
        Err(error) => return map_scope_err(error),
    };
    let owner = row.and_then(|row| row.scope_attribute.zip(row.scope_value));
    lost_write_verdict(
        edge_key,
        owner
            .as_ref()
            .map(|(attribute, value)| (attribute.as_str(), value.as_str())),
        declaring,
    )
}

/// What an edge write that matched nothing answers, from what the re-read
/// found: a row now owned by another scope is a claim lost, and no row (or
/// a row this scope owns) is a row removed. Pure, so both arms are pinned by
/// a unit case rather than by a race.
fn lost_write_verdict(
    edge_key: &str,
    owner: Option<(&str, &str)>,
    declaring: Option<(&str, &str)>,
) -> GraphStoreError {
    match (owner, declaring) {
        (Some((attribute, value)), Some((declaring_attribute, declaring_value)))
            if (attribute, value) != (declaring_attribute, declaring_value) =>
        {
            GraphStoreError::Conflict {
                reason: format!(
                    "edge `{edge_key}` was claimed by scope `{attribute}={value}` while this \
                     write under `{declaring_attribute}={declaring_value}` was being \
                     prepared; a move between scopes is a deletion and a re-declaration, \
                     not a write"
                ),
            }
        }
        _ => GraphStoreError::Conflict {
            reason: format!(
                "edge `{edge_key}` was removed while this write was being prepared; \
                 re-ingest it"
            ),
        },
    }
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
                        // Live rows only; a row already tombstoned is settled
                        // below as a no-op rather than as an absence.
                        let live = node::Entity::find()
                            .secure()
                            .scope_with(&scope)
                            .filter(Condition::all().add(node::Column::NodeKey.eq(key.clone())))
                            .filter(Condition::all().add(node::Column::DeletedAt.is_null()))
                            .limit(1)
                            .project_all(tx, |query| {
                                node_ident_columns(query).into_model::<NodeIdent>()
                            })
                            .await
                            .map_err(map_scope_err)?
                            .into_iter()
                            .next();
                        let Some(model) = live else {
                            return already_tombstoned_node(&scope, tx, &key, epoch).await;
                        };

                        let removed = node::Entity::update_many()
                            .col_expr(node::Column::DeletedAt, Expr::value(Some(now)))
                            .col_expr(
                                node::Column::DeletedBySubjectId,
                                Expr::value(Some(subject.subject_id)),
                            )
                            .col_expr(
                                node::Column::DeletedBySubjectType,
                                Expr::value(subject.subject_type.clone()),
                            )
                            .filter(
                                Condition::all()
                                    .add(node::Column::Id.eq(model.id))
                                    .add(node::Column::DeletedAt.is_null()),
                            )
                            .secure()
                            .scope_with(&scope)
                            .exec(tx)
                            .await
                            .map_err(map_scope_err)?
                            .rows_affected;
                        // The node first, and its edges only if it was ours to
                        // delete. The edges used to go first, so a delete that
                        // then lost the node's compare-and-set had already
                        // tombstoned them: the transaction committed those
                        // writes on the no-op path below, and the answer said
                        // `tombstoned_edges: 0` about edges it had just
                        // deleted. Losing the node now means writing nothing
                        // at all, which is what a no-op is.
                        //
                        // The row was live when it was read and is not now:
                        // another delete landed in between, or a scope
                        // replacement purged it. Either way the caller's
                        // intent already holds, and rule 3 of the Soft Delete
                        // Contract says so -- deleting an already-deleted row
                        // is a no-op, not a failure, because a producer
                        // retrying a delete whose response was lost cannot
                        // tell the two apart from outside. The no-op settle
                        // is the same answer the pre-read takes, and it
                        // leaves the revision where it was rather than
                        // bumping it for a write that did not happen.
                        if removed == 0 {
                            return already_tombstoned_node(&scope, tx, &key, epoch).await;
                        }

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
                            .project_all(tx, |query| {
                                edge_state_columns(query).into_model::<EdgeState>()
                            })
                            .await
                            .map_err(map_scope_err)?;

                        // Counted from what the statements matched, not
                        // from what the scan found: a scope replacement can
                        // hard-delete a row between the two, and a count the
                        // caller reconciles against is worth nothing if it
                        // reports writes that did not land.
                        let mut edges = 0u64;
                        for e in incident {
                            edges += edge::Entity::update_many()
                                .col_expr(edge::Column::DeletedAt, Expr::value(Some(now)))
                                .col_expr(
                                    edge::Column::DeletedBySubjectId,
                                    Expr::value(Some(subject.subject_id)),
                                )
                                .col_expr(
                                    edge::Column::DeletedBySubjectType,
                                    Expr::value(subject.subject_type.clone()),
                                )
                                .filter(
                                    Condition::all()
                                        .add(edge::Column::Id.eq(e.id))
                                        .add(edge::Column::DeletedAt.is_null()),
                                )
                                .secure()
                                .scope_with(&scope)
                                .exec(tx)
                                .await
                                .map_err(map_scope_err)?
                                .rows_affected;
                        }

                        (removed, edges)
                    }
                    DeleteRequest::Edge(key) => {
                        // One read decides whether there is anything to
                        // delete. A second read of the key, under read
                        // committed, can see an upsert that revived the edge
                        // after the first one, and a revived edge is the same
                        // row, not a key taken by another.
                        let row = edge::Entity::find()
                            .secure()
                            .scope_with(&scope)
                            .filter(Condition::all().add(edge::Column::EdgeKey.eq(key.clone())))
                            .limit(1)
                            .project_all(tx, |query| {
                                edge_ends_columns(query).into_model::<EdgeEnds>()
                            })
                            .await
                            .map_err(map_scope_err)?
                            .into_iter()
                            .next();
                        let model = match row {
                            None => {
                                return settle_no_op(&scope, tx, None, "edge", &key, epoch).await;
                            }
                            Some(row) if row.deleted_at.is_some() => {
                                return settle_no_op(&scope, tx, Some(true), "edge", &key, epoch)
                                    .await;
                            }
                            Some(row) => row,
                        };
                        // An edge is a statement about two nodes: tombstoning
                        // it needs both endpoints visible under the caller's
                        // scope, the rule the edge read follows. Denied and
                        // absent answer alike.
                        let mut endpoints = vec![model.src_node_id, model.dst_node_id];
                        endpoints.dedup();
                        let visible = node::Entity::find()
                            .secure()
                            .scope_with(&scope)
                            .filter(Condition::all().add(node::Column::Id.is_in(endpoints.clone())))
                            .filter(Condition::all().add(node::Column::DeletedAt.is_null()))
                            .project_all(tx, |query| {
                                node_ident_columns(query).into_model::<NodeIdent>()
                            })
                            .await
                            .map_err(map_scope_err)?;
                        if visible.len() != endpoints.len() {
                            return endpoint_hidden(&scope, tx, &key, epoch).await;
                        }
                        let removed = edge::Entity::update_many()
                            .col_expr(edge::Column::DeletedAt, Expr::value(Some(now)))
                            .col_expr(
                                edge::Column::DeletedBySubjectId,
                                Expr::value(Some(subject.subject_id)),
                            )
                            .col_expr(
                                edge::Column::DeletedBySubjectType,
                                Expr::value(subject.subject_type.clone()),
                            )
                            .filter(
                                Condition::all()
                                    .add(edge::Column::Id.eq(model.id))
                                    .add(edge::Column::DeletedAt.is_null()),
                            )
                            .secure()
                            .scope_with(&scope)
                            .exec(tx)
                            .await
                            .map_err(map_scope_err)?
                            .rows_affected;
                        // Same rule as the node above.
                        if removed == 0 {
                            return lost_edge_delete(&scope, tx, &key, model.id, epoch).await;
                        }
                        (0u64, removed)
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

/// Settle a delete that found no live row to tombstone.
///
/// Rule 3 of the Soft Delete Contract: "deleting an already-deleted row is a
/// no-op that leaves it untouched, exactly as a converging ingest replay
/// does". Answering `NotFound` instead would make a retry of a delete whose
/// response was lost look like a delete of something that never existed, and
/// a producer cannot tell those apart from outside. A key that genuinely does
/// not exist still reads as absent, so nothing about enumeration changes.
///
/// Only a *tombstoned* row under the key is that proof. The row the delete
/// read can also have been purged by a scope replacement and the key taken
/// by a new, live row before this re-read runs -- the purge frees the key,
/// and read committed lets the re-read see the newcomer. Settling on that
/// row would report a delete that touched nothing, while a live node the
/// caller named stays. It is a `Conflict` instead: the delete read one row
/// and the key now holds another, and a retry deletes the one that is there.
async fn already_tombstoned_node(
    scope: &AccessScope,
    tx: &impl DBRunner,
    key: &str,
    epoch: i64,
) -> Result<DeleteOutcome, TxStoreError> {
    let found = node::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(Condition::all().add(node::Column::NodeKey.eq(key.to_owned())))
        .limit(1)
        .project_all(tx, |query| {
            node_state_columns(query).into_model::<NodeState>()
        })
        .await
        .map_err(map_scope_err)?
        .into_iter()
        .next()
        .map(|row| row.deleted_at.is_some());
    settle_no_op(scope, tx, found, "node", key, epoch).await
}

/// Settle an edge delete that lost its compare-and-set on the row it read.
async fn lost_edge_delete(
    scope: &AccessScope,
    tx: &impl DBRunner,
    key: &str,
    read_id: i64,
    epoch: i64,
) -> Result<DeleteOutcome, TxStoreError> {
    let found = edge_state(scope, tx, key).await?;
    settle_no_op(
        scope,
        tx,
        lost_edge_verdict(read_id, found),
        "edge",
        key,
        epoch,
    )
    .await
}

/// What a lost edge delete finds under the key, as `settle_no_op` reads it.
///
/// An edge, unlike a node, is revived by the next upsert. So a live row
/// under the key is the row the delete read when its id matches: another
/// delete tombstoned it and an upsert revived it between the read and the
/// compare-and-set, and this delete settles as the no-op that ordered before
/// the revive. Only a live row with another id is a key taken again. Pure,
/// so each arm is pinned by a unit case rather than by the race.
fn lost_edge_verdict(read_id: i64, found: Option<EdgeState>) -> Option<bool> {
    match found {
        Some(EdgeState {
            id,
            deleted_at: None,
        }) if id == read_id => Some(true),
        other => other.map(|row| row.deleted_at.is_some()),
    }
}

/// The row under `key` now, with its tombstone: `None` when no row holds the
/// key.
async fn edge_state(
    scope: &AccessScope,
    tx: &impl DBRunner,
    key: &str,
) -> Result<Option<EdgeState>, TxStoreError> {
    Ok(edge::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(Condition::all().add(edge::Column::EdgeKey.eq(key.to_owned())))
        .limit(1)
        .project_all(tx, |query| {
            edge_state_columns(query).into_model::<EdgeState>()
        })
        .await
        .map_err(map_scope_err)?
        .into_iter()
        .next())
}

/// Settle an edge delete whose endpoint is not visible, although the edge
/// was live when it was read.
///
/// Two reads, two snapshots: these transactions begin at the server's
/// default isolation, so a node delete can commit between the edge read
/// above and the endpoint read -- and a node delete tombstones the node
/// and its incident edges in one transaction. The endpoint that vanished
/// then took this edge with it, and the delete the caller asked for has
/// already happened: rule 3 of the Soft Delete Contract, a no-op, exactly
/// as it would be had the edge read come a moment later. Answering
/// `NotFound` here was the seam the race test found once in nine runs.
///
/// The verdict is read back from the edge, not inferred from the endpoint:
/// an endpoint hidden for any other reason -- a scope the caller does not
/// hold, a scope replacement's hard delete -- leaves the edge live or gone,
/// and both of those are still `NotFound`, the answer the edge read gives.
async fn endpoint_hidden(
    scope: &AccessScope,
    tx: &impl DBRunner,
    key: &str,
    epoch: i64,
) -> Result<DeleteOutcome, TxStoreError> {
    let tombstoned = edge_state(scope, tx, key)
        .await?
        .map(|row| row.deleted_at.is_some());
    hidden_endpoint_verdict(tombstoned)?;
    let revision = current_revision(scope, tx).await?;
    Ok(DeleteOutcome {
        revision: GraphRevision {
            source_epoch: epoch,
            revision,
        },
        tombstoned_nodes: 0,
        tombstoned_edges: 0,
    })
}

/// What an edge delete may answer when an endpoint is not visible, from the
/// edge's own state: only a tombstone under the key is a delete that already
/// happened. Pure, so both arms are pinned by a unit case rather than by the
/// race that found the seam.
fn hidden_endpoint_verdict(tombstoned: Option<bool>) -> Result<(), GraphStoreError> {
    match tombstoned {
        Some(true) => Ok(()),
        Some(false) | None => Err(GraphStoreError::NotFound),
    }
}

/// The revision as it stands, with nothing tombstoned — or absence, when the
/// key was never there.
async fn settle_no_op(
    scope: &AccessScope,
    tx: &impl DBRunner,
    tombstoned: Option<bool>,
    what: &str,
    key: &str,
    epoch: i64,
) -> Result<DeleteOutcome, TxStoreError> {
    no_op_verdict(tombstoned, what, key)?;
    let revision = current_revision(scope, tx).await?;
    Ok(DeleteOutcome {
        revision: GraphRevision {
            source_epoch: epoch,
            revision,
        },
        tombstoned_nodes: 0,
        tombstoned_edges: 0,
    })
}

/// What a delete that found no live row may answer, from what it found
/// under the key instead: nothing (`None`), a live row (`Some(false)`), or a
/// tombstoned one (`Some(true)`). Only the last is a no-op.
fn no_op_verdict(tombstoned: Option<bool>, what: &str, key: &str) -> Result<(), GraphStoreError> {
    match tombstoned {
        None => Err(GraphStoreError::NotFound),
        Some(false) => Err(GraphStoreError::Conflict {
            reason: format!(
                "the {what} `{key}` this delete read was removed and the key is now held by \
                 another, live {what}; retry the delete to remove that one"
            ),
        }),
        Some(true) => Ok(()),
    }
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
/// authority (DESIGN § 3.7, `source_namespace_owner`).
#[must_use]
pub const fn migrated_embedding_dimension() -> u32 {
    crate::infra::storage::migrations::m0001_initial_schema::EMBEDDING_DIMENSION
}

/// One step of the node phase: a node the batch carries, or an edge endpoint
/// it names without carrying (`edge` is the first edge that names it).
#[derive(Clone, Copy)]
enum NodeStep<'a> {
    Node(usize),
    Endpoint { key: &'a str, edge: usize },
}

impl NodeStep<'_> {
    fn key<'r>(&'r self, request: &'r IngestRequest) -> &'r str {
        match self {
            NodeStep::Node(index) => &request.nodes[*index].node_key,
            NodeStep::Endpoint { key, .. } => key,
        }
    }
}

/// Upsert every node of the batch and resolve every edge endpoint it names
/// but does not carry, recording each id. Returns whether any stored state
/// changed.
///
/// **In key order, nodes and endpoints together, not in request order.**
/// Every step here can take a row lock on a `node` row or on its unique key
/// -- an upsert, a phantom insert, an insert that waits on another batch's
/// insert of the same key -- and two batches that take the same locks in
/// different orders deadlock (gears-rust #5042: most of the overlapping
/// batches on the Studio stand were killed as `40P01`). Sorting by key gives
/// every batch the same order. Nodes and endpoints share one order because
/// they share one key space: a phantom of `k2` after a node of `k5` in one
/// batch, and the reverse in another, is the same cycle.
///
/// The per-item report is still in request order: outcomes are collected by
/// index and handed to the tally afterwards.
async fn write_nodes(
    w: Writer<'_>,
    tx: &impl DBRunner,
    request: &IngestRequest,
    types: &BTreeMap<String, TypeInfo>,
    node_ids: &mut BTreeMap<String, Endpoint>,
    tally: &mut IngestTally,
    embedding: &EmbeddingPlan,
) -> Result<bool, GraphStoreError> {
    let create_phantoms = request.options.create_phantoms.unwrap_or(true);
    let carried: std::collections::BTreeSet<&str> = request
        .nodes
        .iter()
        .map(|spec| spec.node_key.as_str())
        .collect();
    let mut steps: Vec<NodeStep<'_>> = (0..request.nodes.len()).map(NodeStep::Node).collect();
    let mut named = std::collections::BTreeSet::new();
    for (edge, spec) in request.edges.iter().enumerate() {
        for key in [spec.src_node_key.as_str(), spec.dst_node_key.as_str()] {
            if !carried.contains(key) && named.insert(key) {
                steps.push(NodeStep::Endpoint { key, edge });
            }
        }
    }
    // Stable, so a key the batch carries twice is written in request order.
    steps.sort_by(|a, b| a.key(request).cmp(b.key(request)));

    // What the item loop below would read one key at a time, read up front
    // in a few statements (gears-rust #5040): the live nodes the edges name
    // without carrying, and the stored rows of the nodes carried. They take
    // no lock and run before the batch waits on any key, so what they find is
    // read again under a lock at its turn in key order (below); a key not
    // found here is new, and a concurrent insert of it is met by the
    // lost-race paths. A key not found as a live endpoint still takes the
    // per-key path, tombstone check and phantom included.
    let external: Vec<&str> = named.iter().copied().collect();
    node_ids.extend(lookup_endpoints(w.scope, tx, &external).await?);
    let carried_keys: Vec<&str> = carried.iter().copied().collect();
    let mut stored = stored_nodes(w.scope, tx, &carried_keys).await?;
    let mut seen = std::collections::BTreeSet::new();

    // The nodes the up-front read found no row for are inserted together,
    // in key order, before the item loop: one statement per thousand rows
    // instead of one per node. Only a first occurrence qualifies, and only
    // one the item path would insert without a question to ask -- no
    // `expected_version` other than `Some(0)`, a registered type, a planned
    // vector. A namespace is authorized once, before the rows that name it.
    // Whatever does not qualify, or lost its key to a concurrent batch, is
    // written by the item loop as before.
    // New nodes are inserted in bulk, but never ahead of the key order: the
    // steps run in one global key order, as #5042 requires of every write, a
    // run of consecutive new nodes is collected, and the run is inserted
    // before any step that writes on its own -- a phantom endpoint, an
    // update, a repeat -- and at the end. A node qualifies when the item path
    // would insert it without a question to ask: its first occurrence, no
    // stored row, no `expected_version` but `Some(0)`, a registered type and
    // a planned vector. A namespace is authorized once, at the first node
    // that names it (gears-rust #5040).
    let batch = NodeBatch {
        w,
        request,
        types,
        embedding,
        now: OffsetDateTime::now_utc(),
    };
    let mut authorized = std::collections::BTreeSet::new();
    let mut run: Vec<(usize, node::ActiveModel)> = Vec::new();
    let mut changed = false;
    let mut outcomes: Vec<Option<ItemOutcome>> = vec![None; request.nodes.len()];
    // The up-front reads above took no lock and ran before this batch waited
    // on any key. A row another batch commits while this one waits on an
    // earlier key is therefore missing from them, where the per-key read they
    // replace -- made at the key's turn, after those waits -- saw it. So each
    // prefetched row is read again at its turn in key order, under a lock,
    // a run of consecutive keys per statement: a node this batch writes under
    // `FOR NO KEY UPDATE`, the lock its update takes anyway, and an endpoint
    // it only links to under `FOR SHARE`, which keeps a concurrent batch from
    // materializing it into a type the edge does not admit until this one
    // commits. Neither blocks a foreign-key check of another batch's edge.
    let prefetched_endpoints: std::collections::BTreeSet<String> = external
        .iter()
        .filter(|key| node_ids.contains_key(**key))
        .map(|key| (*key).to_owned())
        .collect();
    let mut relocked = std::collections::BTreeSet::new();
    for position in 0..steps.len() {
        // Between items, not inside one: an item half written is not a state
        // this store has, and the transaction is what guarantees that.
        if w.budget.is_exhausted() {
            return Err(GraphStoreError::Deadline);
        }
        let step = steps[position];
        let state = RelockState {
            request,
            stored: &stored,
            seen: &seen,
            relocked: &relocked,
            prefetched: &prefetched_endpoints,
        };
        if let Some(mode) = state.mode(&step) {
            let keys = state.run_keys(&steps[position..], mode);
            // The pending run holds earlier keys; its locks come first.
            batch
                .flush(tx, &mut run, node_ids, &mut outcomes, &mut authorized)
                .await?;
            relock_run(w.scope, tx, mode, &keys, &mut stored, node_ids).await?;
            relocked.extend(keys);
        }
        if let NodeStep::Node(index) = step
            && let Some((row, namespace)) = batch.new_row(index, &stored, &seen)
        {
            // A claim locks the namespace row, so the earlier keys pending
            // in the run are inserted before it, not after.
            if let Some(namespace) = namespace
                && !authorized.contains(&namespace)
            {
                batch
                    .flush(tx, &mut run, node_ids, &mut outcomes, &mut authorized)
                    .await?;
                super::namespaces::authorize_write(
                    w.tenant,
                    w.scope,
                    tx,
                    &namespace,
                    &w.subject.principal(),
                )
                .await?;
                authorized.insert(namespace);
            }
            seen.insert(request.nodes[index].node_key.clone());
            run.push((index, row));
            continue;
        }
        batch
            .flush(tx, &mut run, node_ids, &mut outcomes, &mut authorized)
            .await?;
        let index = match step {
            NodeStep::Node(index) => index,
            NodeStep::Endpoint { key, edge } => {
                changed |= resolve_endpoint(
                    w,
                    tx,
                    key,
                    edge,
                    &request.edges[edge].type_id,
                    types,
                    node_ids,
                    create_phantoms,
                    tally,
                )
                .await?;
                continue;
            }
        };
        let spec = &request.nodes[index];
        let info = types.get(&spec.type_id).ok_or_else(|| {
            item_error(
                index,
                ItemFamily::Node,
                &spec.type_id,
                "type is not registered".into(),
            )
        })?;
        let (id, write) = batch
            .upsert(
                tx,
                index,
                info,
                known(&mut stored, &mut seen, &spec.node_key),
                &mut authorized,
            )
            .await?;
        node_ids.insert(
            spec.node_key.clone(),
            Endpoint {
                id,
                type_id: info.id,
            },
        );
        outcomes[index] = Some(write);
    }
    batch
        .flush(tx, &mut run, node_ids, &mut outcomes, &mut authorized)
        .await?;
    for outcome in outcomes.into_iter().flatten() {
        changed |= tally.node(&outcome);
    }
    Ok(changed)
}

/// The node phase's fixed inputs, for the steps that insert new nodes in
/// runs and write the rest one at a time.
struct NodeBatch<'a> {
    w: Writer<'a>,
    request: &'a IngestRequest,
    types: &'a BTreeMap<String, TypeInfo>,
    embedding: &'a EmbeddingPlan,
    now: OffsetDateTime,
}

impl NodeBatch<'_> {
    /// The row node `index` is inserted as when it joins a bulk run, with the
    /// namespace it names, or `None` when the item path has a question to ask
    /// about it. The namespace is not authorized here: a claim takes a lock,
    /// and the caller first inserts the pending run of earlier keys.
    fn new_row(
        &self,
        index: usize,
        stored: &BTreeMap<String, node::Model>,
        seen: &std::collections::BTreeSet<String>,
    ) -> Option<(node::ActiveModel, Option<String>)> {
        let spec = &self.request.nodes[index];
        if seen.contains(spec.node_key.as_str())
            || stored.contains_key(&spec.node_key)
            || !matches!(spec.expected_version, None | Some(0))
        {
            return None;
        }
        let (Some(info), Some(decided)) = (
            self.types.get(&spec.type_id),
            self.embedding.nodes.get(index),
        ) else {
            return None;
        };
        let Ok(namespaced) = ownership::namespace_of(info.family.as_deref(), spec.payload.as_ref())
        else {
            return None;
        };
        let namespace = match namespaced {
            ownership::Namespaced::None => None,
            ownership::Namespaced::Under(namespace) => Some(namespace.to_owned()),
        };
        let row = new_node_row(
            self.w,
            spec,
            info,
            NewNode {
                name: spec.name.clone().unwrap_or_default(),
                payload: spec
                    .payload
                    .clone()
                    .unwrap_or_else(|| serde_json::json!({})),
                search_text: compose_search_text(
                    spec.name.as_deref(),
                    spec.payload.as_ref(),
                    &info.full_text_search,
                ),
                vector: plan_vector(
                    None,
                    PlannedVector {
                        decided,
                        active_epoch: self.embedding.epoch,
                    },
                ),
                namespace: namespace.clone(),
                now: self.now,
            },
        );
        Some((row, namespace))
    }

    /// Insert the pending run, then write on the item path, right after it,
    /// every node of the run a concurrent batch took since the up-front read:
    /// `RETURNING` leaves it out, and the item path settles the lost race.
    async fn flush(
        &self,
        tx: &impl DBRunner,
        run: &mut Vec<(usize, node::ActiveModel)>,
        node_ids: &mut BTreeMap<String, Endpoint>,
        outcomes: &mut [Option<ItemOutcome>],
        authorized: &mut std::collections::BTreeSet<String>,
    ) -> Result<(), GraphStoreError> {
        if run.is_empty() {
            return Ok(());
        }
        let (indices, rows): (Vec<usize>, Vec<node::ActiveModel>) = run.drain(..).unzip();
        let landed = insert_new_rows::<node::Entity>(
            self.w.scope,
            tx,
            self.w.budget,
            rows,
            locking_conflict(node::Column::TenantId, node::Column::NodeKey),
            |row| (row.node_key.clone(), row.id),
        )
        .await?;
        for index in indices {
            if self.w.budget.is_exhausted() {
                return Err(GraphStoreError::Deadline);
            }
            let spec = &self.request.nodes[index];
            let info = &self.types[&spec.type_id];
            let (id, outcome) = match landed.get(&spec.node_key) {
                Some(&id) => (id, ItemOutcome::Inserted),
                None => self.upsert(tx, index, info, None, authorized).await?,
            };
            node_ids.insert(
                spec.node_key.clone(),
                Endpoint {
                    id,
                    type_id: info.id,
                },
            );
            outcomes[index] = Some(outcome);
        }
        Ok(())
    }

    /// Node `index` on the item path.
    async fn upsert(
        &self,
        tx: &impl DBRunner,
        index: usize,
        info: &TypeInfo,
        known: Known<node::Model>,
        authorized: &mut std::collections::BTreeSet<String>,
    ) -> Result<(i64, ItemOutcome), GraphStoreError> {
        // Index-aligned with the request's nodes, by the port's contract. A
        // missing entry would silently unembed a node, so it is a store
        // failure rather than a default.
        let decided = self.embedding.nodes.get(index).ok_or_else(|| {
            GraphStoreError::Internal(format!(
                "embedding plan covers {} nodes; the batch has {}",
                self.embedding.nodes.len(),
                self.request.nodes.len()
            ))
        })?;
        upsert_node(
            self.w,
            tx,
            &self.request.nodes[index],
            info,
            index,
            PlannedVector {
                decided,
                active_epoch: self.embedding.epoch,
            },
            known,
            authorized,
        )
        .await
    }
}

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
    tally: &mut IngestTally,
) -> Result<bool, GraphStoreError> {
    if node_ids.contains_key(key) {
        return Ok(false);
    }
    if let Some(endpoint) = lookup_endpoint(w.scope, tx, key).await? {
        node_ids.insert(key.to_owned(), endpoint);
        return Ok(false);
    }
    // No live node, but perhaps a tombstoned one. The key still occupies its
    // row until purge, so the edge can neither link to it -- that is an edge
    // to a node every read calls absent -- nor materialize a phantom over it,
    // which would bring the key back by the back door. The same refusal a
    // node re-ingest under a tombstoned key gets, and whether phantom
    // creation is on changes nothing about it.
    if endpoint_is_tombstoned(w.scope, tx, key).await? {
        return Err(GraphStoreError::Conflict {
            reason: format!(
                "edge[{index}] names endpoint `{key}`, which is tombstoned; the key cannot be \
                 linked to or re-ingested before purge"
            ),
        });
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
    tally.phantom_created();
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
    tally: &mut IngestTally,
) -> Result<bool, GraphStoreError> {
    let create_phantoms = request.options.create_phantoms.unwrap_or(true);
    let mut changed = false;

    // In edge-key order, for the reason `write_nodes` writes in node-key
    // order: two batches upserting the same edges must lock them in one
    // order. The key is the stored one, derived from type, endpoints and
    // discriminator; an edge of an unregistered type sorts first and is
    // refused below.
    let mut order: Vec<(String, usize)> = request
        .edges
        .iter()
        .enumerate()
        .map(|(index, spec)| {
            let key = types
                .get(&spec.type_id)
                .map(|info| identity::derive_edge_key(info.uuid, spec))
                .unwrap_or_default();
            (key, index)
        })
        .collect();
    order.sort();
    let mut outcomes: Vec<Option<ItemOutcome>> = vec![None; request.edges.len()];

    // The stored edges, read up front as `write_nodes` reads the nodes, and
    // the endpoint types, read once per type rather than once per edge.
    let edge_keys: Vec<&str> = order
        .iter()
        .map(|(key, _)| key.as_str())
        .filter(|key| !key.is_empty())
        .collect();
    let mut stored = stored_edges(w.scope, tx, &edge_keys).await?;
    let mut seen = std::collections::BTreeSet::new();
    let mut endpoint_type_cache = EndpointTypeCache::default();

    let declaring = request
        .replace_scope
        .as_ref()
        .map(|replace| (replace.attribute.as_str(), replace.value.as_str()));

    // First pass, reads only: every edge's endpoints resolved and its endpoint
    // constraints checked, in key order.
    let mut checked: Vec<(String, usize, Endpoint, Endpoint)> = Vec::with_capacity(order.len());
    for (edge_key, index) in order {
        let spec = &request.edges[index];
        if w.budget.is_exhausted() {
            return Err(GraphStoreError::Deadline);
        }
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
                tally,
            )
            .await?;
        }

        let src = node_ids[&spec.src_node_key];
        let dst = node_ids[&spec.dst_node_key];

        // Endpoint constraints, checked here because this is the only place
        // both endpoints are resolved and still inside the ingest transaction,
        // so an endpoint's type cannot change between the check and the commit.
        let resolved = endpoint_type_cache
            .resolve(w.scope, tx, &[src.type_id, dst.type_id])
            .await?;
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

        checked.push((edge_key, index, src, dst));
    }

    // The writes, in key order. New edges are inserted in runs, as
    // `write_nodes` inserts new nodes: a first occurrence with no stored row
    // joins the pending run, and the run is inserted before any edge written
    // on its own and at the end, so no lock is taken out of key order. A key
    // a concurrent batch took since the read is written on the item path
    // right after its run.
    //
    // A stored edge is read again at its turn, under the lock its update
    // takes, as `write_nodes` reads a stored node again: the up-front read
    // ran before the endpoint pass and every earlier write, and a delete or
    // a rewrite that committed since would otherwise be answered as
    // `Unchanged` from the old row. One statement per run of consecutive
    // stored keys.
    let now = OffsetDateTime::now_utc();
    let mut run: Vec<(usize, String, Endpoint, Endpoint, edge::ActiveModel)> = Vec::new();
    let mut relocked = std::collections::BTreeSet::new();
    for position in 0..checked.len() {
        if w.budget.is_exhausted() {
            return Err(GraphStoreError::Deadline);
        }
        let keys = stale_edge_run(&checked[position..], &stored, &seen, &relocked);
        if !keys.is_empty() {
            // The pending run holds earlier keys; its locks come first.
            flush_new_edges(w, tx, request, types, declaring, &mut run, &mut outcomes).await?;
            relock_edge_run(w.scope, tx, &keys, &mut stored).await?;
            relocked.extend(keys);
        }
        let (edge_key, index, src, dst) = checked[position].clone();
        let spec = &request.edges[index];
        let info = types.get(&spec.type_id).ok_or_else(|| {
            item_error(
                index,
                ItemFamily::Edge,
                &spec.type_id,
                "type is not registered".into(),
            )
        })?;
        if !seen.contains(edge_key.as_str()) && !stored.contains_key(&edge_key) {
            seen.insert(edge_key.clone());
            let row = new_edge_row(
                w,
                spec,
                NewEdge {
                    key: edge_key.clone(),
                    type_id: info.id,
                    src: src.id,
                    dst: dst.id,
                    payload: spec
                        .payload
                        .clone()
                        .unwrap_or_else(|| serde_json::json!({})),
                    declaring,
                    now,
                },
            );
            run.push((index, edge_key, src, dst, row));
            continue;
        }
        flush_new_edges(w, tx, request, types, declaring, &mut run, &mut outcomes).await?;
        outcomes[index] = Some(
            upsert_edge(
                w,
                tx,
                spec,
                info,
                src.id,
                dst.id,
                declaring,
                known(&mut stored, &mut seen, &edge_key),
            )
            .await?,
        );
    }
    flush_new_edges(w, tx, request, types, declaring, &mut run, &mut outcomes).await?;
    for outcome in outcomes.into_iter().flatten() {
        changed |= tally.edge(&outcome);
    }
    Ok(changed)
}

/// Insert a pending run of new edges, then write on the item path, right
/// after it, every edge of the run a concurrent batch took since the
/// up-front read: `RETURNING` leaves it out.
async fn flush_new_edges(
    w: Writer<'_>,
    tx: &impl DBRunner,
    request: &IngestRequest,
    types: &BTreeMap<String, TypeInfo>,
    declaring: Option<(&str, &str)>,
    run: &mut Vec<(usize, String, Endpoint, Endpoint, edge::ActiveModel)>,
    outcomes: &mut [Option<ItemOutcome>],
) -> Result<(), GraphStoreError> {
    if run.is_empty() {
        return Ok(());
    }
    let mut pending = Vec::with_capacity(run.len());
    let mut rows = Vec::with_capacity(run.len());
    for (index, key, src, dst, row) in run.drain(..) {
        pending.push((index, key, src, dst));
        rows.push(row);
    }
    let landed = insert_new_rows::<edge::Entity>(
        w.scope,
        tx,
        w.budget,
        rows,
        locking_conflict(edge::Column::TenantId, edge::Column::EdgeKey),
        |row| (row.edge_key.clone(), row.id),
    )
    .await?;
    for (index, key, src, dst) in pending {
        if w.budget.is_exhausted() {
            return Err(GraphStoreError::Deadline);
        }
        outcomes[index] = Some(if landed.contains_key(&key) {
            ItemOutcome::Inserted
        } else {
            let spec = &request.edges[index];
            upsert_edge(
                w,
                tx,
                spec,
                &types[&spec.type_id],
                src.id,
                dst.id,
                declaring,
                None,
            )
            .await?
        });
    }
    Ok(())
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

#[cfg(test)]
mod lost_write_verdict_tests {
    use super::{GraphStoreError, lost_write_verdict};

    fn reason(error: GraphStoreError) -> String {
        match error {
            GraphStoreError::Conflict { reason } => reason,
            other => panic!("a lost write is a conflict, got {other:?}"),
        }
    }

    /// The row is now another scope's: the claim was lost, and the answer
    /// names who got there first and what a move would take.
    #[test]
    fn a_row_now_owned_by_another_scope_is_a_lost_claim() {
        let text = reason(lost_write_verdict(
            "e",
            Some(("repository", "acme/infra")),
            Some(("component", "auth")),
        ));
        assert!(
            text.contains("claimed by scope `repository=acme/infra`"),
            "{text}"
        );
        assert!(text.contains("under `component=auth`"), "{text}");
    }

    /// No row: it was removed, and the key is free, so the answer is to
    /// re-ingest -- for a scoped write and an unscoped one alike.
    #[test]
    fn a_missing_row_is_a_removed_row() {
        for declaring in [None, Some(("repository", "acme/infra"))] {
            let text = reason(lost_write_verdict("e", None, declaring));
            assert!(text.contains("was removed"), "{text}");
            assert!(text.contains("re-ingest it"), "{text}");
        }
    }

    /// A row this scope already owns cannot have refused this scope's write
    /// by ownership, so whatever matched nothing, it was not a claim lost.
    #[test]
    fn a_row_owned_by_the_declaring_scope_is_not_a_lost_claim() {
        let text = reason(lost_write_verdict(
            "e",
            Some(("repository", "acme/infra")),
            Some(("repository", "acme/infra")),
        ));
        assert!(text.contains("was removed"), "{text}");
    }
}

#[cfg(test)]
mod hidden_endpoint_verdict_tests {
    use graph_storage_sdk::plugin_api::GraphStoreError;

    use super::hidden_endpoint_verdict;

    /// The endpoint went with a node delete that took this edge too: the
    /// delete already happened.
    #[test]
    fn a_tombstoned_edge_settles_as_a_no_op() {
        assert!(hidden_endpoint_verdict(Some(true)).is_ok());
    }

    /// An endpoint the caller cannot see, over an edge that is still there
    /// or no longer there, is what the edge read answers: absent.
    #[test]
    fn a_live_or_missing_edge_is_not_found() {
        for state in [Some(false), None] {
            assert!(
                matches!(
                    hidden_endpoint_verdict(state),
                    Err(GraphStoreError::NotFound)
                ),
                "{state:?} must stay not-found"
            );
        }
    }
}

#[cfg(test)]
mod no_op_verdict_tests {
    use graph_storage_sdk::plugin_api::GraphStoreError;

    use super::no_op_verdict;

    /// A tombstoned row under the key is a delete that already happened.
    #[test]
    fn a_tombstoned_row_settles_as_a_no_op() {
        assert!(no_op_verdict(Some(true), "node", "k").is_ok());
    }

    /// No row at all is a key that does not exist.
    #[test]
    fn no_row_is_not_found() {
        assert!(matches!(
            no_op_verdict(None, "node", "k"),
            Err(GraphStoreError::NotFound)
        ));
    }

    /// A live row under the key is not the row the delete read: that one was
    /// purged and the key taken again. Settling on it would report a delete
    /// that touched nothing while the node the caller named is still there.
    #[test]
    fn a_live_row_under_the_key_is_a_conflict_not_a_settled_delete() {
        for what in ["node", "edge"] {
            let verdict = no_op_verdict(Some(false), what, "k");
            assert!(
                matches!(&verdict, Err(GraphStoreError::Conflict { reason }) if reason.contains(what)),
                "a live {what} under the key must be a conflict, got {verdict:?}"
            );
        }
    }
}

#[cfg(test)]
mod lost_edge_verdict_tests {
    use super::{EdgeState, lost_edge_verdict};

    fn live(id: i64) -> EdgeState {
        EdgeState {
            id,
            deleted_at: None,
        }
    }

    fn tombstoned(id: i64) -> EdgeState {
        EdgeState {
            id,
            deleted_at: Some(time::OffsetDateTime::UNIX_EPOCH),
        }
    }

    /// The row the delete read, live again: tombstoned and revived after the
    /// read. The delete ordered before the revive, so it is a no-op.
    #[test]
    fn the_same_row_live_again_settles_as_a_no_op() {
        assert_eq!(lost_edge_verdict(7, Some(live(7))), Some(true));
    }

    /// Another row live under the key: the one the delete read was purged
    /// and the key taken again, which stays a conflict.
    #[test]
    fn another_live_row_under_the_key_stays_a_conflict() {
        assert_eq!(lost_edge_verdict(7, Some(live(8))), Some(false));
    }

    /// A tombstone settles and no row is absent, whatever row was read.
    #[test]
    fn a_tombstone_settles_and_no_row_is_absent() {
        assert_eq!(lost_edge_verdict(7, Some(tombstoned(7))), Some(true));
        assert_eq!(lost_edge_verdict(7, Some(tombstoned(8))), Some(true));
        assert_eq!(lost_edge_verdict(7, None), None);
    }
}
